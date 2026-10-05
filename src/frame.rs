//! Frame SSH operations. Config data is parsed locally and never evaluated as shell code.
use crate::i18n::{self, t};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ssh2::{HashType, KeyboardInteractivePrompt, Prompt, Session};
use std::os::windows::ffi::OsStringExt;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{ERROR_BUFFER_OVERFLOW, INVALID_HANDLE_VALUE},
    NetworkManagement::{
        IpHelper::{
            GetAdaptersAddresses, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211,
            ICMP_ECHO_REPLY, IP_ADAPTER_ADDRESSES_LH, IcmpCloseHandle, IcmpCreateFile,
            IcmpSendEcho,
        },
        Ndis::IfOperStatusUp,
    },
    Networking::WinSock::{AF_INET, NI_NAMEREQD, SOCKADDR, SOCKADDR_IN, getnameinfo},
};
use zeroize::Zeroize;

pub const SSH_HELP: &str = "https://github.com/toorux/steam-frame-6ghz-tool#frame-如何开启ssh";
pub const SSH_HELP_EN: &str =
    "https://github.com/toorux/steam-frame-6ghz-tool/blob/main/README.en.md#enabling-ssh-on-frame";
pub const REPO: &str = "https://github.com/toorux/steam-frame-6ghz-tool";
pub const IP_HELP: &str = "https://github.com/toorux/steam-frame-6ghz-tool#如何查看-frame-ip";
pub const IP_HELP_EN: &str = "https://github.com/toorux/steam-frame-6ghz-tool/blob/main/README.en.md#finding-the-frame-ip";
const GET_CONFIG: &str = "sudo -S -p '' cat /etc/conf.d/wireless-regdom";
const WATCH_SCRIPT: &str = include_str!("../assets/frame-regdom-watch.sh");
const WATCH_UNIT: &str = include_str!("../assets/frame-regdom-watch.service");
const WATCH_SERVICE: &str = "steam-frame-regdom-watch.service";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Network {
    pub base: u32,
    pub prefix: u8,
}
impl Network {
    pub fn host_count(self) -> u64 {
        (1u64 << (32 - self.prefix)) - 2
    }
    pub fn contains(self, ip: Ipv4Addr) -> bool {
        let mask = u32::MAX << (32 - self.prefix);
        (u32::from(ip) & mask) == self.base
    }
    fn host(self, index: u64) -> Ipv4Addr {
        Ipv4Addr::from(self.base + index as u32 + 1)
    }
}

pub fn networks() -> Result<Vec<Network>, String> {
    let mut bytes = vec![0u64; 2048]; // 16 KiB, aligned for IP_ADAPTER_ADDRESSES_LH.
    let mut size = (bytes.len() * 8) as u32;
    let mut result = unsafe {
        GetAdaptersAddresses(
            AF_INET as u32,
            0,
            std::ptr::null(),
            bytes.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if result == ERROR_BUFFER_OVERFLOW {
        bytes.resize((size as usize).div_ceil(8), 0);
        result = unsafe {
            GetAdaptersAddresses(
                AF_INET as u32,
                0,
                std::ptr::null(),
                bytes.as_mut_ptr().cast(),
                &mut size,
            )
        };
    }
    if result != 0 {
        return Err(if i18n::is_english() {
            format!("Could not read local network: Windows error {result}")
        } else {
            format!("读取本机网段失败：Windows 错误 {result}")
        });
    }
    let mut found = vec![];
    let mut adapter = bytes.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !adapter.is_null() {
        let a = unsafe { &*adapter };
        let description = if a.Description.is_null() {
            String::new()
        } else {
            let mut len = 0;
            while unsafe { *a.Description.add(len) } != 0 {
                len += 1;
            }
            std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(a.Description, len) })
                .to_string_lossy()
                .into_owned()
        };
        if a.OperStatus == IfOperStatusUp
            && matches!(a.IfType, IF_TYPE_ETHERNET_CSMACD | IF_TYPE_IEEE80211)
            && !virtual_adapter(&description)
        {
            let mut item = a.FirstUnicastAddress;
            while !item.is_null() {
                let u = unsafe { &*item };
                if !u.Address.lpSockaddr.is_null()
                    && unsafe { (*u.Address.lpSockaddr).sa_family } == AF_INET
                {
                    let sin = unsafe { &*(u.Address.lpSockaddr as *const SOCKADDR_IN) };
                    let ip = Ipv4Addr::from(u32::from_be(unsafe { sin.sin_addr.S_un.S_addr }));
                    let prefix = u.OnLinkPrefixLength;
                    if ip.is_private() && (8..=30).contains(&prefix) {
                        let base = u32::from(ip) & (u32::MAX << (32 - prefix));
                        let net = Network { base, prefix };
                        if !found.contains(&net) {
                            found.push(net);
                        }
                    }
                }
                item = u.Next;
            }
        }
        adapter = a.Next;
    }
    Ok(found)
}
fn virtual_adapter(description: &str) -> bool {
    // ponytail: name-based filter; replace with an adapter capability query if virtual NIC naming becomes inconsistent.
    let name = description.to_ascii_lowercase();
    ["virtual", "hyper-v", "wsl", "tunnel", "tap", "vpn"]
        .iter()
        .any(|word| name.contains(word))
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub ip: Ipv4Addr,
    pub ssh_open: bool,
}
pub struct Scan {
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<AtomicU64>,
    pub done: Arc<AtomicBool>,
    pub total: u64,
    pub results: mpsc::Receiver<Candidate>,
}
pub fn is_frame_name(name: &str) -> bool {
    name.trim_end_matches('.')
        .split('.')
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case("frame"))
}
fn reverse_name(ip: Ipv4Addr) -> Option<String> {
    let addr = SOCKADDR_IN {
        sin_family: AF_INET,
        sin_port: 0,
        sin_addr: windows_sys::Win32::Networking::WinSock::IN_ADDR {
            S_un: windows_sys::Win32::Networking::WinSock::IN_ADDR_0 {
                S_addr: u32::from(ip).to_be(),
            },
        },
        sin_zero: [0; 8],
    };
    let mut text = [0u8; 256];
    let rc = unsafe {
        getnameinfo(
            (&addr as *const SOCKADDR_IN).cast::<SOCKADDR>(),
            std::mem::size_of::<SOCKADDR_IN>() as i32,
            text.as_mut_ptr(),
            text.len() as u32,
            std::ptr::null_mut(),
            0,
            NI_NAMEREQD as i32,
        )
    };
    if rc != 0 {
        return None;
    }
    let end = text.iter().position(|&b| b == 0)?;
    Some(String::from_utf8_lossy(&text[..end]).into_owned())
}
pub fn ssh_open(ip: Ipv4Addr) -> bool {
    ssh_open_with_timeout(ip, Duration::from_millis(550))
}
fn ssh_open_with_timeout(ip: Ipv4Addr, timeout: Duration) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::new(IpAddr::V4(ip), 22),
        timeout,
    )
    .is_ok()
}
fn ping(ip: Ipv4Addr, handle: windows_sys::Win32::Foundation::HANDLE) -> bool {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE { return false; }
    let data = [0u8; 1];
    let mut reply = [0u64; 16];
    let count = unsafe { IcmpSendEcho(handle, u32::from(ip).to_be(), data.as_ptr().cast(), 1,
        std::ptr::null(), reply.as_mut_ptr().cast(), (reply.len() * 8) as u32, 180) };
    count > 0 && unsafe { (*(reply.as_ptr().cast::<ICMP_ECHO_REPLY>())).Status == 0 }
}
pub fn start_scan(nets: Vec<Network>) -> Scan {
    let total = nets.iter().map(|n| n.host_count()).sum();
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    let count = progress.clone();
    let finished = done.clone();
    thread::spawn(move || {
        let (resolved_tx, resolved_rx) = mpsc::channel();
        let direct_tx = tx.clone();
        let direct_nets = nets.clone();
        let direct_stop = stop.clone();
        thread::spawn(move || {
            let mut known = vec![];
            for name in ["frame:22", "frame.local:22"] {
                if direct_stop.load(Ordering::Relaxed) { break; }
                if let Ok(addresses) = name.to_socket_addrs() {
                    for address in addresses {
                        if let IpAddr::V4(ip) = address.ip()
                            && direct_nets.iter().any(|n| n.contains(ip))
                            && !known.contains(&ip)
                        {
                            known.push(ip);
                            let _ = direct_tx.send(Candidate { ip, ssh_open: ssh_open(ip) });
                        }
                    }
                }
            }
            let _ = resolved_tx.send(());
        });
        let next = AtomicU64::new(0);
        thread::scope(|scope| {
            for _ in 0..24 {
                let tx = tx.clone();
                let stop = stop.clone();
                let count = count.clone();
                let nets = &nets;
                let next = &next;
                scope.spawn(move || {
                    let icmp = unsafe { IcmpCreateFile() };
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= total {
                            break;
                        }
                        let mut rest = index;
                        let Some(ip) = nets.iter().find_map(|net| {
                            if rest < net.host_count() {
                                Some(net.host(rest))
                            } else {
                                rest -= net.host_count();
                                None
                            }
                        }) else {
                            break;
                        };
                        let reachable = ping(ip, icmp);
                        let ssh = if reachable { false } else { ssh_open_with_timeout(ip, Duration::from_millis(180)) };
                        if (reachable || ssh) && reverse_name(ip).is_some_and(|n| is_frame_name(&n)) {
                            let _ = tx.send(Candidate {
                                ip,
                                ssh_open: ssh || ssh_open(ip),
                            });
                        }
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                    if !icmp.is_null() && icmp != INVALID_HANDLE_VALUE { unsafe { IcmpCloseHandle(icmp); } }
                });
            }
        });
        if !stop.load(Ordering::Relaxed) { let _ = resolved_rx.recv_timeout(Duration::from_secs(2)); }
        finished.store(true, Ordering::Release);
    });
    Scan {
        cancel,
        progress,
        done,
        total,
        results: rx,
    }
}

#[derive(Serialize, Deserialize, Default)]
struct KnownHosts(BTreeMap<String, String>);
fn known_hosts_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("LOCALAPPDATA")
        .ok_or(t("找不到 LOCALAPPDATA", "LOCALAPPDATA was not found"))?;
    Ok(PathBuf::from(base)
        .join("SteamFrame6GHzTool")
        .join("known-hosts.json"))
}
fn known_hosts() -> Result<KnownHosts, String> {
    match fs::read(known_hosts_path()?) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            if i18n::is_english() {
                format!("Saved host key record is invalid: {e}")
            } else {
                format!("已保存的主机密钥记录无效：{e}")
            }
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KnownHosts::default()),
        Err(e) => Err(if i18n::is_english() {
            format!("Could not read saved host keys: {e}")
        } else {
            format!("无法读取已保存的主机密钥：{e}")
        }),
    }
}
fn save_host(ip: Ipv4Addr, fingerprint: &str, previous: Option<&str>) -> Result<(), String> {
    let path = known_hosts_path()?;
    let mut hosts = known_hosts()?;
    if !trust_record(&mut hosts, ip, fingerprint, previous)? { return Ok(()); }
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(
        path,
        serde_json::to_vec_pretty(&hosts).map_err(|e| e.to_string())?,
    )
    .map_err(|e| {
        if i18n::is_english() {
            format!("Could not save SSH host key: {e}")
        } else {
            format!("无法保存 SSH 主机密钥：{e}")
        }
    })
}
fn trust_record(hosts: &mut KnownHosts, ip: Ipv4Addr, fingerprint: &str, previous: Option<&str>) -> Result<bool, String> {
    if hosts.0.get(&ip.to_string()).map(String::as_str) != previous {
        return Err(t(
            "保存的 SSH 主机密钥在确认后发生变化；拒绝继续",
            "Saved SSH host key changed after confirmation; stopped",
        )
        .into());
    }
    if previous == Some(fingerprint) { return Ok(false); }
    hosts.0.insert(ip.to_string(), fingerprint.into());
    Ok(true)
}
fn connect(ip: Ipv4Addr) -> Result<(Session, String), String> {
    let stream =
        TcpStream::connect_timeout(&SocketAddr::new(IpAddr::V4(ip), 22), Duration::from_secs(3))
            .map_err(|e| {
                if i18n::is_english() {
                    format!("SSH connection failed: {e}")
                } else {
                    format!("SSH 连接失败：{e}")
                }
            })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;
    let mut session = Session::new().map_err(|e| e.to_string())?;
    session.set_tcp_stream(stream);
    session.set_timeout(8000);
    session.handshake().map_err(|e| {
        if i18n::is_english() {
            format!("SSH handshake failed: {e}")
        } else {
            format!("SSH 握手失败：{e}")
        }
    })?;
    let hash = session
        .host_key_hash(HashType::Sha256)
        .ok_or(t("无法读取 SSH 主机密钥", "Could not read SSH host key"))?;
    let fingerprint = format!("SHA256:{}", STANDARD_NO_PAD.encode(hash));
    Ok((session, fingerprint))
}
pub struct Probe {
    pub fingerprint: String,
    pub previous: Option<String>,
}
pub fn probe(ip: Ipv4Addr) -> Result<Probe, String> {
    let (_, fingerprint) = connect(ip)?;
    let hosts = known_hosts()?;
    Ok(Probe { previous: hosts.0.get(&ip.to_string()).cloned(), fingerprint })
}

fn authenticate(session: &Session, credentials: &Credentials) -> Result<(), String> {
    if session.userauth_password(&credentials.username, &credentials.password).is_err() {
        struct PasswordPrompt<'a>(&'a str);
        impl KeyboardInteractivePrompt for PasswordPrompt<'_> {
            fn prompt<'a>(&mut self, _: &str, _: &str, prompts: &[Prompt<'a>]) -> Vec<String> {
                if prompts.len() == 1 && !prompts[0].echo {
                    vec![self.0.to_string()]
                } else {
                    vec![String::new(); prompts.len()]
                }
            }
        }
        session.userauth_keyboard_interactive(
            &credentials.username,
            &mut PasswordPrompt(&credentials.password),
        ).map_err(|_| t("SSH 用户名或密码验证失败", "SSH username or password was rejected").to_string())?;
    }
    if !session.authenticated() {
        return Err(t("SSH 验证未完成", "SSH authentication did not complete").into());
    }
    Ok(())
}

pub fn headset_service_log(ip: Ipv4Addr, credentials: Credentials) -> Result<String, String> {
    let (session, fingerprint) = connect(ip)?;
    match known_hosts()?.0.get(&ip.to_string()) {
        Some(saved) if saved == &fingerprint => {}
        Some(_) => return Err(t("SSH 主机密钥已变化；拒绝发送密码", "SSH host key changed; password was not sent").into()),
        None => return Err(t("请先通过“连接并设置”信任头显，再导出服务日志。", "Trust the headset with Connect and set up before exporting its service log.").into()),
    }
    authenticate(&session, &credentials)?;
    let sudo = if credentials.sudo_password.is_empty() { &credentials.password } else { &credentials.sudo_password };
    let mut output_log = |_line: String| {};
    let code = format!("sudo -S -p '' journalctl -u {WATCH_SERVICE} -b -n 500 --no-pager -o short-iso");
    let (status, output) = command(&session, &code, Some(sudo), &mut output_log)?;
    let mut redacted = output;
    for secret in [&credentials.password, &credentials.sudo_password] {
        if !secret.is_empty() { redacted = redacted.replace(secret, "[REDACTED]"); }
    }
    if status != 0 {
        return Err(format!("{} (exit {status}): {}", t("无法读取头显服务日志", "Could not read the headset service log"),
            redacted.trim()));
    }
    Ok(redacted)
}

pub struct Credentials {
    pub username: String,
    pub password: String,
    pub sudo_password: String,
}
impl Drop for Credentials {
    fn drop(&mut self) {
        self.password.zeroize();
        self.sudo_password.zeroize();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupResult { AlreadySet, Success, NeedsRestart, Partial, Failed }
impl SetupResult {
    pub fn message(self) -> &'static str {
        match self {
            Self::AlreadySet => t("头显已是 US，自动维护也已开启。", "US is already active, and automatic maintenance is running."),
            Self::Success => t("设置成功；自动维护已开启，请重启头显测试。", "Setup complete. Automatic maintenance is running; restart your headset to test it."),
            Self::NeedsRestart => t("环境异常，本次设置不保证成功，请重启头显测试", "Unexpected configuration: setup cannot be guaranteed. Restart your headset and test it."),
            Self::Partial => t("设置部分完成，请查看日志中的已完成步骤和错误。", "Setup only partially completed. Check the log for changes and errors."),
            Self::Failed => t("设置失败，请查看日志。", "Setup failed. Check the log for details."),
        }
    }
}
pub struct Outcome {
    pub result: SetupResult,
    pub error: Option<String>,
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn command(
    session: &Session, code: &str, sudo_password: Option<&str>,
    log: &mut impl FnMut(String),
) -> Result<(i32, String), String> {
    log(format!("$ {code}"));
    let mut channel = session.channel_session().map_err(|e| e.to_string())?;
    // Merge both streams before reading so neither SSH stream can block the other.
    channel.handle_extended_data(ssh2::ExtendedData::Merge).map_err(|e| e.to_string())?;
    channel.exec(code).map_err(|e| e.to_string())?;
    if let Some(password) = sudo_password {
        let mut secret = password.as_bytes().to_vec();
        secret.push(b'\n');
        let result = channel.write_all(&secret);
        secret.zeroize();
        result.map_err(|e| e.to_string())?;
    }
    channel.send_eof().map_err(|e| e.to_string())?;
    let mut output = Vec::new();
    let mut pending = Vec::new();
    let mut buffer = [0u8; 2048];
    let mut truncated = false;
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(60) { return Err(t("远端命令超时，结果可能不完整。", "Remote command timed out; its result may be incomplete.").into()); }
        let count = channel.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 { break; }
        let keep = count.min((64 * 1024usize).saturating_sub(output.len()));
        output.extend_from_slice(&buffer[..keep]);
        pending.extend_from_slice(&buffer[..keep]);
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            log(String::from_utf8_lossy(&pending[..end]).trim_end_matches('\r').to_owned());
            pending.drain(..=end);
        }
        truncated |= keep < count;
    }
    if !pending.is_empty() { log(String::from_utf8_lossy(&pending).into_owned()); }
    channel.wait_close().map_err(|e| e.to_string())?;
    let status = channel.exit_status().map_err(|e| e.to_string())?;
    if truncated { log(t("[输出已截断]", "[Output truncated]").into()); }
    log(format!("[exit {status}]"));
    if truncated { return Err(t("输出过长，停止处理以避免使用不完整配置。", "Output exceeded the limit; stopped to avoid using incomplete configuration.").into()); }
    Ok((status, String::from_utf8(output).map_err(|_| t("输出不是有效 UTF-8；未继续处理", "Output is not valid UTF-8; stopped"))?))
}
#[derive(Debug)]
struct ConfigPlan { content: String, changed: bool, abnormal: bool }
fn config_plan(content: Option<&str>) -> Result<ConfigPlan, String> {
    let original = content.unwrap_or("");
    let mut active = None;
    let mut commented_us = None;
    let lines: Vec<_> = original.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let text = line.trim();
        if text.is_empty() { continue; }
        if text.starts_with('#') {
            if text.trim_start_matches('#').trim() == "WIRELESS_REGDOM=\"US\"" { commented_us = Some(index); }
            continue;
        }
        let Some((key, value)) = text.split_once('=') else {
            return Err(t("配置无法安全解析；未修改。", "Configuration cannot be safely parsed; unchanged.").into());
        };
        if key.trim() != "WIRELESS_REGDOM" {
            return Err(t("配置包含未知设置；未修改。", "Configuration contains unsupported settings; unchanged.").into());
        }
        let value = value.trim();
        let value = if value.len() >= 2 && (value.starts_with('"') && value.ends_with('"') || value.starts_with('\'') && value.ends_with('\'')) {
            &value[1..value.len()-1]
        } else { value };
        if value.len() != 2 || !value.bytes().all(|b| b.is_ascii_uppercase()) || active.is_some() {
            return Err(t("国家码配置无效或存在多条冲突；未修改。", "Invalid or conflicting country-code entries; unchanged.").into());
        }
        active = Some((index, value));
    }
    if active.is_some_and(|(_, value)| value == "US") {
        return Ok(ConfigPlan { content: original.into(), changed: false, abnormal: false });
    }
    let replace = active.map(|(index, _)| index).or(commented_us);
    let mut updated = String::new();
    for (index, line) in original.split_inclusive('\n').enumerate() {
        if replace == Some(index) { updated.push_str("WIRELESS_REGDOM=\"US\"\n"); }
        else { updated.push_str(line); }
    }
    if replace.is_none() {
        if !updated.is_empty() && !updated.ends_with('\n') { updated.push('\n'); }
        updated.push_str("WIRELESS_REGDOM=\"US\"\n");
    }
    Ok(ConfigPlan { content: updated, changed: true, abnormal: original.trim().is_empty() })
}
const PREFLIGHT: &str = r#"set -eu
command -v iw
command -v base64
command -v mktemp
test ! -L /etc/conf.d
if test -e /etc/conf.d; then test -d /etc/conf.d; test -w /etc/conf.d; else test -w /etc; fi
test ! -L /etc/conf.d/wireless-regdom
if test -e /etc/conf.d/wireless-regdom; then
  test -f /etc/conf.d/wireless-regdom
  test -r /etc/conf.d/wireless-regdom
  test -w /etc/conf.d/wireless-regdom
fi"#;
fn write_script(before: Option<&str>, after: &str) -> String {
    let guard = if let Some(before) = before {
        let encoded = STANDARD_NO_PAD.encode(before);
        format!("test -f \"$p\"\ntest \"$(base64 < \"$p\" | tr -d '\\n=')\" = {}\nb=$(mktemp \"$p.backup.XXXXXX\")\ncp -p -- \"$p\" \"$b\"\nprintf 'Backup: %s\\n' \"$b\"", shell_quote(&encoded))
    } else {
        "test ! -e \"$p\"\nprintf 'Original file was absent\\n'".into()
    };
    format!(r#"set -eu
p=/etc/conf.d/wireless-regdom
test ! -L /etc/conf.d
test ! -L "$p"
{guard}
mkdir -p /etc/conf.d
tmp=$(mktemp /etc/conf.d/.wireless-regdom.XXXXXX)
trap 'rm -f -- "$tmp"' EXIT
if test -e "$p"; then cp -p -- "$p" "$tmp"; else chmod 644 "$tmp"; fi
printf %s {after} > "$tmp"
mv -f -- "$tmp" "$p"
trap - EXIT"#, after=shell_quote(after))
}
fn reg_is_us(output: &str) -> bool {
    let mut section = "";
    let mut global = false;
    let mut phy0 = false;
    for line in output.lines() {
        if line == "global" || line.starts_with("phy#") { section = line; }
        if line.trim_start().starts_with("country US:") {
            if section == "global" { global = true; }
            if section.starts_with("phy#0") { phy0 = true; }
        }
    }
    global && phy0
}
fn watcher_paths() -> (&'static str, &'static str) {
    ("/var/lib/steam-frame-6ghz-tool/regdom-watch.sh", "/etc/systemd/system/steam-frame-regdom-watch.service")
}
fn watcher_hash(content: &str) -> String { format!("{:x}", Sha256::digest(content.as_bytes())) }
fn watcher_probe() -> String {
    let (script, unit) = watcher_paths();
    format!(r#"set -eu
command -v iw >/dev/null; command -v stdbuf >/dev/null; command -v awk >/dev/null; command -v sha256sum >/dev/null; command -v systemctl >/dev/null; command -v findmnt >/dev/null
test "$(findmnt -T /var -n -o TARGET)" = /var
grep -Fxq '/etc/systemd/system/*.service' /usr/lib/rauc/atomic-update-keep.conf
grep -Fxq '/etc/systemd/system/*.wants/**' /usr/lib/rauc/atomic-update-keep.conf
test ! -L /var/lib/steam-frame-6ghz-tool
test ! -L {script}
test ! -L {unit}
if test -e {script}; then
  test -f {script}
  grep -Fxq '# Managed by Steam Frame 6 GHz Tool. Do not edit in place.' {script} || {{ echo 'Existing watcher script is not managed by this program.' >&2; exit 1; }}
fi
if test -e {unit}; then
  test -f {unit}
  grep -Fxq '# Managed by Steam Frame 6 GHz Tool. Do not edit in place.' {unit} || {{ echo 'Existing watcher service is not managed by this program.' >&2; exit 1; }}
fi
if test -e {script} && test -e {unit} && test "$(sha256sum {script} | cut -d ' ' -f1)" = {script_hash} && test "$(sha256sum {unit} | cut -d ' ' -f1)" = {unit_hash} && systemctl is-enabled --quiet {service} && systemctl is-active --quiet {service}; then
  printf ready
else
  printf install
fi"#,
        script = shell_quote(script), unit = shell_quote(unit),
        script_hash = shell_quote(&watcher_hash(WATCH_SCRIPT)),
        unit_hash = shell_quote(&watcher_hash(WATCH_UNIT)), service = WATCH_SERVICE)
}
fn watcher_install() -> String {
    let (script, unit) = watcher_paths();
    format!(r#"set -eu
install -d -m 755 /var/lib/steam-frame-6ghz-tool
test "$(stat -c %u /var/lib/steam-frame-6ghz-tool)" = 0
if test ! -e {script} || test "$(sha256sum {script} | cut -d ' ' -f1)" != {script_hash}; then
  if test -e {script}; then
    backup=$(mktemp /var/lib/steam-frame-6ghz-tool/regdom-watch.sh.backup.XXXXXX)
    cp -p -- {script} "$backup"
    printf 'Backup: %s\n' "$backup"
  fi
  tmp=$(mktemp /var/lib/steam-frame-6ghz-tool/.regdom-watch.XXXXXX)
  trap 'rm -f -- "$tmp"' EXIT
  printf %s {script_content} > "$tmp"
  chmod 755 "$tmp"
  mv -f -- "$tmp" {script}
  trap - EXIT
fi
if test ! -e {unit} || test "$(sha256sum {unit} | cut -d ' ' -f1)" != {unit_hash}; then
  if test -e {unit}; then
    backup=$(mktemp /etc/systemd/system/steam-frame-regdom-watch.service.backup.XXXXXX)
    cp -p -- {unit} "$backup"
    printf 'Backup: %s\n' "$backup"
  fi
  tmp=$(mktemp /etc/systemd/system/.steam-frame-regdom-watch.XXXXXX)
  trap 'rm -f -- "$tmp"' EXIT
  printf %s {unit_content} > "$tmp"
  chmod 644 "$tmp"
  mv -f -- "$tmp" {unit}
  trap - EXIT
fi
test "$(sha256sum {script} | cut -d ' ' -f1)" = {script_hash}
test "$(sha256sum {unit} | cut -d ' ' -f1)" = {unit_hash}
systemctl daemon-reload
systemctl enable {service}
systemctl restart {service}
sleep 2
systemctl is-enabled --quiet {service}
systemctl is-active --quiet {service}
printf 'Automatic regulatory watcher installed and running.'"#,
        script = shell_quote(script), unit = shell_quote(unit),
        script_content = shell_quote(WATCH_SCRIPT), unit_content = shell_quote(WATCH_UNIT),
        script_hash = shell_quote(&watcher_hash(WATCH_SCRIPT)),
        unit_hash = shell_quote(&watcher_hash(WATCH_UNIT)), service = WATCH_SERVICE)
}
fn ensure_watcher(run: &mut impl FnMut(&str) -> Result<String, String>, changed: &mut bool) -> Result<(), String> {
    let state = run(&format!("sudo -S -p '' sh -c {}", shell_quote(&watcher_probe())))?;
    match state.as_str() {
        "ready" => Ok(()),
        "install" => {
            *changed = true;
            run(&format!("sudo -S -p '' sh -c {}", shell_quote(&watcher_install())))?;
            Ok(())
        }
        _ => Err(t("自动维护状态无法确认；未安装。", "Could not confirm automatic maintenance state; not installed.").into()),
    }
}
fn apply_settings(run: &mut impl FnMut(&str) -> Result<String, String>, changed: &mut bool, abnormal: &mut bool) -> Result<(), String> {
        run("sudo -S -p '' true")?;
        run(&format!("sudo -S -p '' sh -c {}", shell_quote(PREFLIGHT)))?;
        let exists = run("sudo -S -p '' sh -c 'if test -e /etc/conf.d/wireless-regdom; then printf present; else printf absent; fi'")?;
        let before = if exists == "present" { Some(run(GET_CONFIG)?) } else if exists == "absent" { None } else { return Err("Unexpected configuration probe response".into()); };
        let plan = config_plan(before.as_deref())?;
        *abnormal = plan.abnormal;
        if !plan.changed && reg_is_us(&run("sudo -S -p '' sh -c 'iw reg get'")?) { return Ok(()); }
        // From this point a disconnect may leave a runtime change, even if no reply arrives.
        *changed = true;
        run("sudo -S -p '' sh -c 'iw reg set US'")?;
        if plan.changed {
            run(&format!("sudo -S -p '' sh -c {}", shell_quote(&write_script(before.as_deref(), &plan.content))))?;
        }
        let after = run(GET_CONFIG)?;
        if after != plan.content { return Err(t("配置写入复查失败", "Configuration read-back did not match").into()); }
        let state = run("sudo -S -p '' sh -c 'iw reg get'")?;
        if !reg_is_us(&state) { return Err(t("配置已写入，但未确认全局及 phy#0 都为 US", "Configuration saved, but global and phy#0 regions were not both confirmed as US").into()); }
        Ok(())
}
pub fn execute(ip: Ipv4Addr, expected: &str, previous: Option<&str>, credentials: Credentials, mut emit: impl FnMut(String)) -> Outcome {
    // Redact complete lines, including secrets split across SSH read chunks.
    let mut log = |mut line: String| {
        for secret in [&credentials.password, &credentials.sudo_password] {
            if !secret.is_empty() { line = line.replace(secret, "[REDACTED]"); }
        }
        emit(line);
    };
    let mut changed = false;
    let mut abnormal = false;
    let result = (|| -> Result<(), String> {
        let (session, fingerprint) = connect(ip)?;
        if fingerprint != expected {
            return Err(t(
                "SSH 主机密钥在确认后发生变化；拒绝发送密码",
                "SSH host key changed after confirmation; password was not sent",
            )
            .into());
        }
        let hosts = known_hosts()?;
        if hosts.0.get(&ip.to_string()).map(String::as_str) != previous {
            return Err(t(
                "保存的 SSH 主机密钥在确认后发生变化；拒绝发送密码",
                "Saved SSH host key changed after confirmation; password was not sent",
            )
            .into());
        }
        save_host(ip, expected, previous)?;
        authenticate(&session, &credentials)?;

        log(t("SSH 登录成功；检查环境…", "SSH login succeeded; checking environment…").into());
        let sudo = if credentials.sudo_password.is_empty() { &credentials.password } else { &credentials.sudo_password };
        let mut run = |code: &str| -> Result<String, String> {
            let (status, output) = command(&session, code, Some(sudo), &mut log)?;
            if status != 0 { return Err(format!("{} (exit {status})", t("远端命令失败，请查看日志", "Remote command failed; see log"))); }
            Ok(output)
        };
        apply_settings(&mut run, &mut changed, &mut abnormal)?;
        ensure_watcher(&mut run, &mut changed)
    })();
    let status = match &result {
        Ok(()) if !changed => SetupResult::AlreadySet,
        Ok(()) if abnormal => SetupResult::NeedsRestart,
        Ok(()) => SetupResult::Success,
        Err(_) if changed => SetupResult::Partial,
        Err(_) => SetupResult::Failed,
    };
    if let Err(error) = &result { log(format!("[ERROR] {error}")); }
    log(status.message().into());
    Outcome { result: status, error: result.err() }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retrust_replaces_only_the_confirmed_host_key() {
        let ip: Ipv4Addr = "192.0.2.1".parse().unwrap();
        let other: Ipv4Addr = "192.0.2.2".parse().unwrap();
        let mut hosts = KnownHosts::default();
        assert!(trust_record(&mut hosts, ip, "SHA256:old", None).unwrap());
        assert!(trust_record(&mut hosts, other, "SHA256:other", None).unwrap());
        assert!(!trust_record(&mut hosts, ip, "SHA256:old", Some("SHA256:old")).unwrap());
        assert!(trust_record(&mut hosts, ip, "SHA256:new", None).is_err());
        assert!(trust_record(&mut hosts, ip, "SHA256:new", Some("SHA256:wrong")).is_err());
        assert_eq!(hosts.0[&ip.to_string()], "SHA256:old");
        assert!(trust_record(&mut hosts, ip, "SHA256:new", Some("SHA256:old")).unwrap());
        assert_eq!(hosts.0[&ip.to_string()], "SHA256:new");
        assert_eq!(hosts.0[&other.to_string()], "SHA256:other");
        assert!(trust_record(&mut hosts, ip, "SHA256:third", Some("SHA256:old")).is_err());
    }
    #[test]
    fn discovery_filters_exact_hostname_and_config_is_strict() {
        assert!(virtual_adapter("Hyper-V Virtual Ethernet Adapter"));
        assert!(!virtual_adapter(
            "MediaTek Wi-Fi 7 MT7925 Wireless LAN Card"
        ));
        assert!(is_frame_name("frame.local"));
        assert!(is_frame_name("FRAME"));
        assert!(!is_frame_name("frame-other.local"));
        assert!(!config_plan(Some("WIRELESS_REGDOM=\"US\"\n")).unwrap().changed);
        assert!(reg_is_us(
            "global\ncountry US: DFS-FCC\nphy#0 (self-managed)\ncountry US: DFS-FCC"
        ));
        assert!(!reg_is_us(
            "global\ncountry US: DFS-FCC\nphy#0 (self-managed)\ncountry CN: DFS-UNKNOWN"
        ));
    }
    #[test]
    fn config_plans_preserve_comments_and_reject_ambiguity() {
        for value in [None, Some(""), Some(" \n")] {
            let plan = config_plan(value).unwrap();
            assert!(plan.abnormal && plan.changed);
            assert!(plan.content.contains("WIRELESS_REGDOM=\"US\""));
        }
        for text in ["WIRELESS_REGDOM='CN'\n", "#WIRELESS_REGDOM=\"US\"\n", "# comments\n"] {
            let plan = config_plan(Some(text)).unwrap();
            assert!(plan.changed && !plan.abnormal);
            assert!(!config_plan(Some(&plan.content)).unwrap().changed);
        }
        for text in ["WIRELESS_REGDOM=\"US\"\nWIRELESS_REGDOM=\"CN\"", "echo bad", "WIRELESS_REGDOM=$(id)"] {
            assert!(config_plan(Some(text)).is_err());
        }
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        let script = write_script(Some("# old\n"), "WIRELESS_REGDOM=\"US\"\n");
        assert!(script.find("cp -p").unwrap() < script.find("mv -f").unwrap());
        assert!(script.contains("set -eu"));
        assert!(write_script(None, "x").contains("Original file was absent"));
    }

    #[test]
    fn setup_failures_stop_and_classify_changes() {
        let original = "WIRELESS_REGDOM=\"CN\"\n";
        let updated = "WIRELESS_REGDOM=\"US\"\n";
        // sudo, environment, existence, read, runtime, write/backup, readback, regulatory state
        for fail in 0..8 {
            let mut step = 0;
            let mut changed = false;
            let mut abnormal = false;
            let result = apply_settings(&mut |_| {
                let current = step;
                step += 1;
                if current == fail { return Err("injected failure".into()); }
                Ok(match current {
                    2 => "present", 3 => original, 6 => updated,
                    7 => "global\ncountry US:\nphy#0\ncountry US:", _ => "",
                }.into())
            }, &mut changed, &mut abnormal);
            assert!(result.is_err());
            assert_eq!(step, fail + 1);
            assert_eq!(changed, fail >= 4);
        }
        for (before, abnormal_expected) in [(None, true), (Some(""), true), (Some(updated), false)] {
            let mut reads = 0;
            let mut writes = 0;
            let mut runtime_sets = 0;
            let mut changed = false;
            let mut abnormal = false;
            apply_settings(&mut |code| {
                if code.contains("printf present") { return Ok(if before.is_some() { "present" } else { "absent" }.into()); }
                if code == GET_CONFIG {
                    reads += 1;
                    return Ok(if reads == 1 { before.unwrap_or(updated) } else { updated }.into());
                }
                if code.contains("mv -f") { writes += 1; }
                if code.contains("iw reg set US") { runtime_sets += 1; }
                if code.contains("iw reg get") { return Ok("global\ncountry US:\nphy#0\ncountry US:".into()); }
                Ok(String::new())
            }, &mut changed, &mut abnormal).unwrap();
            assert_eq!(abnormal, abnormal_expected);
            assert_eq!(writes, usize::from(before != Some(updated)));
            assert_eq!(runtime_sets, usize::from(before != Some(updated)));
            assert_eq!(changed, before != Some(updated));
        }
        let mut runtime_sets = 0;
        let mut changed = false;
        let mut abnormal = false;
        apply_settings(&mut |code| {
            if code.contains("printf present") { return Ok("present".into()); }
            if code == GET_CONFIG { return Ok(updated.into()); }
            if code.contains("iw reg set US") { runtime_sets += 1; }
            if code.contains("iw reg get") {
                return Ok(if runtime_sets == 0 { "global\ncountry CN:\nphy#0\ncountry CN:" } else { "global\ncountry US:\nphy#0\ncountry US:" }.into());
            }
            Ok(String::new())
        }, &mut changed, &mut abnormal).unwrap();
        assert_eq!(runtime_sets, 1);
        assert!(changed);
    }
    #[test]
    fn automatic_watcher_is_idempotent_and_reports_install_failures() {
        let mut changed = false;
        let mut calls = 0;
        ensure_watcher(&mut |code| {
            calls += 1;
            assert!(code.contains("steam-frame-regdom-watch.service"));
            Ok("ready".into())
        }, &mut changed).unwrap();
        assert_eq!(calls, 1);
        assert!(!changed);

        ensure_watcher(&mut |code| {
            calls += 1;
            if code.contains("systemctl restart") { Err("install failed".into()) }
            else { Ok("install".into()) }
        }, &mut changed).unwrap_err();
        assert_eq!(calls, 3);
        assert!(changed);
        assert!(watcher_probe().contains("/etc/systemd/system/*.service"));
        assert!(watcher_install().contains("systemctl is-active --quiet"));
        assert!(WATCH_SCRIPT.contains("stdbuf -oL iw event -T"));
        assert!(WATCH_SCRIPT.contains("attempts >= 3"));
    }
    #[test]
    #[ignore = "Read-only check of local network and optional FRAME_TEST_IP SSH handshake"]
    fn local_network_snapshot() {
        let nets = networks().expect("local IPv4 networks");
        println!("networks={nets:?}");
        if let Ok(ip) = std::env::var("FRAME_TEST_IP") {
            let ip: Ipv4Addr = ip.parse().expect("IPv4");
            println!("frame_ip={ip} ssh_open={}", ssh_open(ip));
            if ssh_open(ip) {
                println!(
                    "fingerprint={}",
                    probe(ip).expect("SSH handshake").fingerprint
                );
            }
        }
    }
    #[test]
    #[ignore = "Read-only LAN scan, requires FRAME_TEST_IP"]
    fn discovers_frame_without_authentication() {
        let ip: Ipv4Addr = std::env::var("FRAME_TEST_IP")
            .expect("FRAME_TEST_IP")
            .parse()
            .unwrap();
        let scan = start_scan(networks().unwrap());
        let found = (0..5)
            .filter_map(|_| scan.results.recv_timeout(Duration::from_secs(2)).ok())
            .any(|c| c.ip == ip);
        scan.cancel.store(true, Ordering::Relaxed);
        assert!(found, "did not discover frame at {ip}");
    }
}
