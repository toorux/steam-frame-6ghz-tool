//! Frame SSH operations. Only the fixed commands below may be sent to the headset.
use crate::i18n::{self, t};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};
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
    Foundation::ERROR_BUFFER_OVERFLOW,
    NetworkManagement::{
        IpHelper::{
            GetAdaptersAddresses, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211,
            IP_ADAPTER_ADDRESSES_LH,
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
const CONFIG: &str = "/etc/conf.d/wireless-regdom";
pub const COMMANDS: &str = "sudo -S -p '' true  # 验证 sudo 权限\nsudo -S -p '' cat /etc/conf.d/wireless-regdom  # 预检，不写入\nsudo -S -p '' /usr/sbin/iw reg set US\n# 如果尚未启用 US，则执行：\nsudo -S -p '' sed -i 's/^#WIRELESS_REGDOM=\"US\"$/WIRELESS_REGDOM=\"US\"/' /etc/conf.d/wireless-regdom\nsudo -S -p '' cat /etc/conf.d/wireless-regdom  # 复查\ngrep -n '^WIRELESS_REGDOM=' /etc/conf.d/wireless-regdom\n/usr/sbin/iw reg get";
pub const COMMANDS_EN: &str = "sudo -S -p '' true  # Check sudo access\nsudo -S -p '' cat /etc/conf.d/wireless-regdom  # Preflight; read only\nsudo -S -p '' /usr/sbin/iw reg set US\n# Only if US is not already enabled:\nsudo -S -p '' sed -i 's/^#WIRELESS_REGDOM=\"US\"$/WIRELESS_REGDOM=\"US\"/' /etc/conf.d/wireless-regdom\nsudo -S -p '' cat /etc/conf.d/wireless-regdom  # Verify\ngrep -n '^WIRELESS_REGDOM=' /etc/conf.d/wireless-regdom\n/usr/sbin/iw reg get";
const SET_RUNTIME: &str = "sudo -S -p '' /usr/sbin/iw reg set US";
const SET_CONFIG: &str = "sudo -S -p '' sed -i 's/^#WIRELESS_REGDOM=\"US\"$/WIRELESS_REGDOM=\"US\"/' /etc/conf.d/wireless-regdom";
const GET_CONFIG: &str = "sudo -S -p '' cat /etc/conf.d/wireless-regdom";
const GREP_CONFIG: &str = "grep -n '^WIRELESS_REGDOM=' /etc/conf.d/wireless-regdom";
const GET_REG: &str = "/usr/sbin/iw reg get";

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
    TcpStream::connect_timeout(
        &SocketAddr::new(IpAddr::V4(ip), 22),
        Duration::from_millis(550),
    )
    .is_ok()
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
        let mut known = vec![];
        for name in ["frame:22", "frame.local:22"] {
            if let Ok(addresses) = name.to_socket_addrs() {
                for address in addresses {
                    if let IpAddr::V4(ip) = address.ip()
                        && nets.iter().any(|n| n.contains(ip))
                        && !known.contains(&ip)
                    {
                        known.push(ip);
                        let _ = tx.send(Candidate {
                            ip,
                            ssh_open: ssh_open(ip),
                        });
                    }
                }
            }
        }
        let next = AtomicU64::new(0);
        thread::scope(|scope| {
            for _ in 0..24 {
                let tx = tx.clone();
                let stop = stop.clone();
                let count = count.clone();
                let nets = &nets;
                let next = &next;
                scope.spawn(move || {
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
                        if reverse_name(ip).is_some_and(|n| is_frame_name(&n)) {
                            let _ = tx.send(Candidate {
                                ip,
                                ssh_open: ssh_open(ip),
                            });
                        }
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });
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
fn save_host(ip: Ipv4Addr, fingerprint: &str) -> Result<(), String> {
    let path = known_hosts_path()?;
    let mut hosts = known_hosts()?;
    match hosts.0.get(&ip.to_string()) {
        Some(old) if old != fingerprint => {
            return Err(t(
                "SSH 主机密钥已变化；拒绝发送密码",
                "SSH host key changed; password was not sent",
            )
            .into());
        }
        Some(_) => return Ok(()),
        None => {}
    }
    hosts.0.insert(ip.to_string(), fingerprint.into());
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
    pub new_host: bool,
}
pub fn probe(ip: Ipv4Addr) -> Result<Probe, String> {
    let (_, fingerprint) = connect(ip)?;
    let hosts = known_hosts()?;
    match hosts.0.get(&ip.to_string()) {
        Some(old) if old != &fingerprint => Err(t(
            "SSH 主机密钥已变化；拒绝发送密码",
            "SSH host key changed; password was not sent",
        )
        .into()),
        old => Ok(Probe {
            new_host: old.is_none(),
            fingerprint,
        }),
    }
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
pub struct Outcome {
    pub lines: Vec<String>,
    pub success: bool,
    pub sudo_auth_failed: bool,
}
fn command(
    session: &Session,
    code: &str,
    sudo_password: Option<&str>,
) -> Result<(i32, String), String> {
    let mut channel = session.channel_session().map_err(|e| e.to_string())?;
    channel.exec(code).map_err(|e| {
        if i18n::is_english() {
            format!("Could not start remote command: {e}")
        } else {
            format!("远端命令启动失败：{e}")
        }
    })?;
    if let Some(password) = sudo_password {
        let mut secret = password.as_bytes().to_vec();
        secret.push(b'\n');
        let write = channel.write_all(&secret);
        secret.zeroize();
        write.map_err(|e| e.to_string())?;
    }
    channel.send_eof().map_err(|e| e.to_string())?;
    let mut stdout = String::new();
    let mut stderr = String::new();
    (&mut channel)
        .take(32 * 1024)
        .read_to_string(&mut stdout)
        .map_err(|e| e.to_string())?;
    channel
        .stderr()
        .take(8 * 1024)
        .read_to_string(&mut stderr)
        .map_err(|e| e.to_string())?;
    channel.wait_close().map_err(|e| e.to_string())?;
    let status = channel.exit_status().map_err(|e| e.to_string())?;
    if status != 0 {
        return Ok((status, stderr.trim().to_string()));
    }
    Ok((status, stdout))
}
fn valid_config(content: &str) -> Result<bool, String> {
    let active: Vec<_> = content
        .lines()
        .filter(|line| line.starts_with("WIRELESS_REGDOM="))
        .collect();
    if active.len() > 1
        || active
            .first()
            .is_some_and(|line| *line != "WIRELESS_REGDOM=\"US\"")
    {
        return Err(t(
            "配置中存在其他已启用的国家码；未修改",
            "Another country code is enabled in the configuration; no changes made",
        )
        .into());
    }
    if active.is_empty()
        && !content
            .lines()
            .any(|line| line == "#WIRELESS_REGDOM=\"US\"")
    {
        return Err(t(
            "配置中没有 README 预期的 US 行；未修改",
            "The expected US line is missing from the configuration; no changes made",
        )
        .into());
    }
    Ok(!active.is_empty())
}
fn reg_is_us(output: &str) -> bool {
    let mut section = "";
    let mut global = false;
    let mut phy0 = false;
    for line in output.lines() {
        if line == "global" || line.starts_with("phy#") {
            section = line;
        }
        if line.trim_start().starts_with("country US:") {
            if section == "global" {
                global = true;
            }
            if section.starts_with("phy#0") {
                phy0 = true;
            }
        }
    }
    global && phy0
}
pub fn execute(ip: Ipv4Addr, expected: &str, credentials: Credentials) -> Outcome {
    let mut lines = vec![if i18n::is_english() {
        format!("Connecting to {ip}; checking host key")
    } else {
        format!("连接 {ip}，核对主机密钥")
    }];
    let mut sudo_auth_failed = false;
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
        if hosts
            .0
            .get(&ip.to_string())
            .is_some_and(|old| old != expected)
        {
            return Err(t(
                "SSH 主机密钥已变化；拒绝发送密码",
                "SSH host key changed; password was not sent",
            )
            .into());
        }
        save_host(ip, expected)?;
        if session
            .userauth_password(&credentials.username, &credentials.password)
            .is_err()
        {
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
            session
                .userauth_keyboard_interactive(
                    &credentials.username,
                    &mut PasswordPrompt(&credentials.password),
                )
                .map_err(|_| {
                    t(
                        "SSH 用户名或密码验证失败",
                        "SSH username or password was rejected",
                    )
                    .to_string()
                })?;
        }
        if !session.authenticated() {
            return Err(t("SSH 验证未完成", "SSH authentication did not complete").into());
        }
        lines.push(t("SSH 登录成功", "SSH login succeeded").into());
        let sudo = if credentials.sudo_password.is_empty() {
            &credentials.password
        } else {
            &credentials.sudo_password
        };
        let (code, _) = command(&session, "sudo -S -p '' true", Some(sudo))?;
        if code != 0 {
            sudo_auth_failed = true;
            return Err(t(
                "sudo 密码或权限验证失败；尚未修改头显",
                "sudo password or permission check failed; headset unchanged",
            )
            .into());
        }
        let (code, config) = command(&session, GET_CONFIG, Some(sudo))?;
        if code != 0 {
            return Err(if i18n::is_english() {
                format!("Could not read {CONFIG}; headset unchanged")
            } else {
                format!("无法读取 {CONFIG}；尚未修改头显")
            });
        }
        let enabled = valid_config(&config)?;
        lines.push(
            t(
                "永久配置预检通过",
                "Persistent configuration preflight passed",
            )
            .into(),
        );
        let (code, _) = command(&session, SET_RUNTIME, Some(sudo))?;
        if code != 0 {
            return Err(t(
                "临时设置 US 失败；永久配置未修改",
                "Runtime US setting failed; persistent configuration unchanged",
            )
            .into());
        }
        lines.push(t("已执行临时设置 US", "Runtime US setting applied").into());
        if !enabled {
            let (code, _) = command(&session, SET_CONFIG, Some(sudo))?;
            if code != 0 {
                return Err(t(
                    "临时设置已完成，但永久配置修改失败",
                    "Runtime setting applied, but persistent configuration update failed",
                )
                .into());
            }
            lines.push(t("已启用永久配置 US", "Persistent US setting enabled").into());
        } else {
            lines.push(
                t(
                    "永久配置已是 US，未重复修改",
                    "Persistent configuration is already US; file unchanged",
                )
                .into(),
            );
        }
        let (code, after) = command(&session, GET_CONFIG, Some(sudo))?;
        if code != 0 || valid_config(&after) != Ok(true) {
            return Err(t(
                "临时设置已完成，但永久配置复查失败",
                "Runtime setting applied, but persistent configuration verification failed",
            )
            .into());
        }
        let (code, active) = command(&session, GREP_CONFIG, None)?;
        if code != 0
            || active.lines().count() != 1
            || !active
                .lines()
                .next()
                .is_some_and(|line| line.ends_with(":WIRELESS_REGDOM=\"US\""))
        {
            return Err(t(
                "永久配置不是唯一一条已启用的 US 设置",
                "Persistent US setting is not the only enabled region entry",
            )
            .into());
        }
        let (code, state) = command(&session, GET_REG, None)?;
        if code != 0 || !reg_is_us(&state) {
            return Err(t("永久配置已启用，但运行时未确认全局及 phy#0 都是 US", "Persistent US enabled, but runtime global and phy#0 regions were not both confirmed as US").into());
        }
        lines.push(t("复查通过：全局和 phy#0 为 US，永久配置已启用；请稍后自行重启头显再复查", "Verification passed: global and phy#0 are US, persistent setting enabled. Restart the headset yourself and verify again.").into());
        Ok(())
    })();
    let success = result.is_ok();
    if let Err(e) = result {
        lines.push(format!("[ERROR] {e}"));
    }
    Outcome {
        success,
        sudo_auth_failed,
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_filters_exact_hostname_and_config_is_strict() {
        assert!(virtual_adapter("Hyper-V Virtual Ethernet Adapter"));
        assert!(!virtual_adapter(
            "MediaTek Wi-Fi 7 MT7925 Wireless LAN Card"
        ));
        assert!(is_frame_name("frame.local"));
        assert!(is_frame_name("FRAME"));
        assert!(!is_frame_name("frame-other.local"));
        assert_eq!(valid_config("#WIRELESS_REGDOM=\"US\"\n"), Ok(false));
        assert_eq!(valid_config("WIRELESS_REGDOM=\"US\"\n"), Ok(true));
        assert!(valid_config("WIRELESS_REGDOM=\"CN\"\n#WIRELESS_REGDOM=\"US\"\n").is_err());
        assert!(reg_is_us(
            "global\ncountry US: DFS-FCC\nphy#0 (self-managed)\ncountry US: DFS-FCC"
        ));
        assert!(!reg_is_us(
            "global\ncountry US: DFS-FCC\nphy#0 (self-managed)\ncountry CN: DFS-UNKNOWN"
        ));
        assert!(!COMMANDS.contains("password"));
    }
    #[test]
    fn translated_preview_keeps_the_same_commands() {
        let commands = |preview: &str| {
            preview
                .lines()
                .map(|line| line.split("  #").next().unwrap().trim())
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(commands(COMMANDS), commands(COMMANDS_EN));
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
