use crate::protocol::{self, Command, Result, Status, Transport};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Command as Process, Stdio},
    ptr::{null, null_mut},
    thread,
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
        NetworkManagement::WiFi::*,
        System::{
            SystemInformation::{GetSystemDirectoryW, GetSystemTime, GetWindowsDirectoryW},
            Threading::CreateMutexW,
        },
    },
    core::GUID,
};

pub fn timestamp() -> String {
    unsafe {
        let mut t = std::mem::zeroed();
        GetSystemTime(&mut t);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
        )
    }
}
pub fn windows_dir() -> PathBuf {
    let mut b = [0u16; 32768];
    let n = unsafe { GetWindowsDirectoryW(b.as_mut_ptr(), b.len() as u32) } as usize;
    PathBuf::from(String::from_utf16_lossy(&b[..n.min(b.len())]))
}
fn system_dir() -> Result<PathBuf> {
    let mut b = [0u16; 32768];
    let n = unsafe { GetSystemDirectoryW(b.as_mut_ptr(), b.len() as u32) } as usize;
    if n == 0 || n >= b.len() {
        return Err("无法确定系统目录".into());
    }
    Ok(PathBuf::from(String::from_utf16_lossy(&b[..n])))
}
fn check(code: u32, context: &str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "{context}: Windows 错误 {code}。错误 5 请以管理员身份运行；设备拔出请刷新列表。"
        ))
    }
}
fn guid_text(g: GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}
fn normalized_guid(s: &str) -> String {
    s.trim().trim_matches(['{', '}']).to_ascii_lowercase()
}

struct Wlan(HANDLE);
impl Wlan {
    fn open() -> Result<Self> {
        let (mut handle, mut version) = (null_mut(), 0);
        check(
            unsafe { WlanOpenHandle(2, null(), &mut version, &mut handle) },
            "WlanOpenHandle",
        )?;
        Ok(Self(handle))
    }
}
impl Drop for Wlan {
    fn drop(&mut self) {
        unsafe {
            WlanCloseHandle(self.0, null());
        }
    }
}

#[derive(Clone)]
pub struct Adapter {
    pub guid: GUID,
    pub id: String,
    pub name: String,
    pub pnp: String,
    pub service: String,
    pub compatibility: std::result::Result<String, String>,
    pub unverified_driver: bool,
}
impl Adapter {
    pub fn supported(&self) -> bool {
        self.compatibility.is_ok()
    }
}

#[derive(Deserialize)]
struct Inventory {
    #[serde(rename = "GUID")]
    guid: Option<String>,
    #[serde(rename = "PNPDeviceID")]
    pnp: Option<String>,
    #[serde(rename = "ServiceName")]
    service: Option<String>,
    #[serde(rename = "DriverPath")]
    driver_path: Option<String>,
}

fn inventory() -> Result<Vec<Inventory>> {
    // Fixed read-only CIM query; no user input or shell interpolation.
    let script = r#"$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $driver=Get-CimInstance Win32_SystemDriver -Filter "Name='rtwlanuval'"; $items=@(Get-CimInstance Win32_NetworkAdapter | Where-Object { $_.GUID } | Select-Object GUID,PNPDeviceID,ServiceName,@{Name='DriverPath';Expression={$driver.PathName}}); ConvertTo-Json -InputObject $items -Compress"#;
    let mut child = Process::new(system_dir()?.join("WindowsPowerShell/v1.0/powershell.exe"))
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    // Drain both pipes while waiting, with a fixed memory bound and 20s deadline.
    let stdout = child.stdout.take().ok_or("缺少 stdout")?;
    let stderr = child.stderr.take().ok_or("缺少 stderr")?;
    let out = thread::spawn(move || {
        let mut b = Vec::new();
        stdout.take(1_048_577).read_to_end(&mut b).map(|_| b)
    });
    let err = thread::spawn(move || {
        let mut b = Vec::new();
        stderr.take(65_537).read_to_end(&mut b).map(|_| b)
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if start.elapsed() < Duration::from_secs(20) => {
                thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("设备清单查询失败或超过 20 秒".to_string());
            }
        }
    };
    let output = out
        .join()
        .map_err(|_| "清单读取线程失败")?
        .map_err(|e| e.to_string())?;
    let error = err
        .join()
        .map_err(|_| "错误读取线程失败")?
        .map_err(|e| e.to_string())?;
    let status = status?;
    if !status.success() {
        return Err(format!(
            "CIM 清单查询失败：{}",
            String::from_utf8_lossy(&error)
        ));
    }
    if output.len() > 1_048_576 {
        return Err("清单超过大小限制".into());
    }
    serde_json::from_slice(&output).map_err(|e| format!("设备清单 JSON 错误：{e}"))
}

fn steam_frame_hardware(pnp: &str) -> bool {
    let id = pnp.to_ascii_uppercase();
    let Some(component) = id.strip_prefix("USB\\").and_then(|s| s.split('\\').next()) else {
        return false;
    };
    let parts: Vec<_> = component.split('&').collect();
    parts.contains(&"VID_28DE") && parts.contains(&"PID_2432")
}

fn supported_hardware(pnp: &str, service: &str) -> bool {
    steam_frame_hardware(pnp) && service.eq_ignore_ascii_case("rtwlanuval")
}

fn driver_compatibility(
    pnp: &str,
    service: &str,
    path_matches: bool,
    hash: &Result<String>,
) -> Result<String> {
    match hash {
        _ if !supported_hardware(pnp, service) => {
            Err("不是已验证的 Valve USB 适配器 / rtwlanuval 服务".into())
        }
        _ if !path_matches => Err("驱动服务路径无法核验；禁止私有请求".into()),
        Err(e) => Err(format!("无法读取驱动文件：{e}")),
        Ok(h) if h != protocol::DRIVER_HASH => {
            Ok(format!("提示：驱动版本未验证，仍允许操作 / SHA256 {h}"))
        }
        _ => Ok(format!(
            "已验证 5.32.908.2026 / SHA256 {}",
            protocol::DRIVER_HASH
        )),
    }
}

pub fn enumerate() -> Result<Vec<Adapter>> {
    let device_info = inventory().map_err(|e| format!("无法识别 Steam Frame 适配器：{e}"))?;
    let hash = system_dir()
        .and_then(|p| fs::read(p.join("drivers/rtwlanuval.sys")).map_err(|e| e.to_string()))
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
    let wlan = Wlan::open()?;
    let mut list = null_mut();
    check(
        unsafe { WlanEnumInterfaces(wlan.0, null(), &mut list) },
        "WlanEnumInterfaces",
    )?;
    if list.is_null() {
        return Err("WLAN 清单为空指针".into());
    }
    struct Memory(*mut WLAN_INTERFACE_INFO_LIST);
    impl Drop for Memory {
        fn drop(&mut self) {
            unsafe {
                WlanFreeMemory(self.0.cast());
            }
        }
    }
    let memory = Memory(list);
    let count = unsafe { (*memory.0).dwNumberOfItems } as usize;
    if count > 1024 {
        return Err("WLAN 清单数量无效".into());
    }
    // Windows owns this variable-length array; take only its reported item count.
    let entries = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!((*memory.0).InterfaceInfo).cast::<WLAN_INTERFACE_INFO>(),
            count,
        )
    };
    let mut result = Vec::new();
    for entry in entries {
        let id = guid_text(entry.InterfaceGuid);
        let item = device_info
            .iter()
            .find(|r| r.guid.as_ref().is_some_and(|g| normalized_guid(g) == id));
        let pnp = item.and_then(|r| r.pnp.clone()).unwrap_or_default();
        // Identify by USB hardware ID, not a renameable display name or driver version.
        if !steam_frame_hardware(&pnp) {
            continue;
        }
        let service = item.and_then(|r| r.service.clone()).unwrap_or_default();
        let path_matches = item
            .and_then(|r| r.driver_path.as_deref())
            .is_some_and(|path| {
                let path = path.trim().trim_matches('"').to_ascii_lowercase();
                let expected = system_dir().map(|p| {
                    p.join("drivers/rtwlanuval.sys")
                        .to_string_lossy()
                        .replace('/', "\\")
                        .to_ascii_lowercase()
                });
                let expanded = path.replace(
                    "\\systemroot\\",
                    &format!("{}\\", windows_dir().display()).to_ascii_lowercase(),
                );
                expected.is_ok_and(|p| expanded.trim_start_matches("\\??\\") == p)
            });
        let compatibility = driver_compatibility(&pnp, &service, path_matches, &hash);
        let len = entry
            .strInterfaceDescription
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(256);
        result.push(Adapter {
            guid: entry.InterfaceGuid,
            id,
            name: String::from_utf16_lossy(&entry.strInterfaceDescription[..len]),
            pnp,
            service,
            compatibility,
            unverified_driver: hash.as_ref().is_ok_and(|h| h != protocol::DRIVER_HASH),
        });
    }
    result.sort_by_key(|a| (!a.supported(), a.name.clone(), a.id.clone()));
    Ok(result)
}

struct DiagnosticLock(HANDLE);
impl DiagnosticLock {
    fn acquire() -> Result<Self> {
        Self::try_acquire()?
            .ok_or_else(|| "另一实例正在操作。请关闭其他诊断工具，稍后重试查看状态。".into())
    }
    fn try_acquire() -> Result<Option<Self>> {
        // ponytail: one system-wide mutex because this driver uses shared buffers.
        let name: Vec<u16> = "Global\\SteamFrameLabOriginalDriverDiagnosticV1\0"
            .encode_utf16()
            .collect();
        let handle = unsafe { CreateMutexW(null(), 1, name.as_ptr()) };
        if handle.is_null() {
            return Err("无法取得诊断互斥锁；请使用管理员权限".into());
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(handle);
            }
            return Ok(None);
        }
        Ok(Some(Self(handle)))
    }
}
impl Drop for DiagnosticLock {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct Driver {
    wlan: Wlan,
    guid: GUID,
    logs: Vec<String>,
}
impl Transport for Driver {
    fn exchange(&mut self, command: Command) -> Result<Vec<u8>> {
        let request = protocol::packet(command);
        let mut response = request.clone();
        let mut returned = 0;
        let code = unsafe {
            WlanIhvControl(
                self.wlan.0,
                &self.guid,
                wlan_ihv_control_type_driver,
                request.len() as u32,
                request.as_ptr().cast(),
                response.len() as u32,
                response.as_mut_ptr().cast(),
                &mut returned,
            )
        };
        self.logs.push(format!(
            "{} {command:?}: result={code}, bytes={returned}",
            timestamp()
        ));
        check(code, "WlanIhvControl（不自动重发）")?;
        let bytes = protocol::decode(&request, &response, returned as usize)?.to_vec();
        if command == Command::Country {
            self.logs.push(format!(
                "Country payload_len={}, payload_hex={bytes:02X?}",
                bytes.len()
            ));
        }
        Ok(bytes)
    }
}

pub struct Report {
    pub status: Result<Status>,
    pub logs: Vec<String>,
    pub uncertain: bool,
}
pub fn operate(selected: &Adapter, set_us: bool) -> Report {
    operate_locked(selected, set_us, false, DiagnosticLock::acquire())
}
/// None means another instance holds the lock; no device API has been called.
pub fn try_auto_operate(selected: &Adapter) -> Result<Option<Report>> {
    Ok(DiagnosticLock::try_acquire()?.map(|guard| operate_locked(selected, true, true, Ok(guard))))
}
fn validate_setting_baseline(before: &Status) -> Result<()> {
    if before.country_known() || before.country == protocol::UNKNOWN_COUNTRY {
        Ok(())
    } else {
        Err("国家码响应无效；未发送设置请求。".into())
    }
}
fn operate_locked(
    selected: &Adapter,
    set_us: bool,
    automatic: bool,
    guard: Result<DiagnosticLock>,
) -> Report {
    let mut driver = None;
    let mut lines = vec![format!(
        "{} target={} hardware={} service={} action={}",
        timestamp(),
        selected.id,
        selected.pnp,
        selected.service,
        if set_us { "SET_US_ONCE" } else { "STATUS" }
    )];
    let mut uncertain = false;
    let result = (|| {
        let _guard = guard?;
        // Re-enumerate immediately before IHV calls; do not trust an old UI selection.
        let current = enumerate()?
            .into_iter()
            .find(|a| a.id == selected.id && a.pnp == selected.pnp)
            .ok_or("适配器已移除或替换，请刷新列表")?;
        lines.push(current.compatibility.clone()?);
        driver = Some(Driver {
            wlan: Wlan::open()?,
            guid: current.guid,
            logs: vec![],
        });
        let d = driver.as_mut().unwrap();
        let before = protocol::status(d)?;
        lines.push(format!(
            "修改前 / 当前国家={}\n{}",
            before.country, before.info
        ));
        if !set_us {
            return Ok(before);
        }
        // No baseline -> no write. Already in the requested state -> no redundant write.
        validate_setting_baseline(&before)?;
        if !before.country_known() {
            lines.push(format!(
                "[WARN] 原国家码为 00 00；诊断查询正常，{}仅发送一次设置并严格复查。",
                if automatic {
                    "自动应用"
                } else {
                    "用户确认后"
                }
            ));
        }
        if before.manual_us_supported() {
            lines.push("已经是 US/MANUAL 且 6 GHz 支持；未重复发送设置。".into());
            return Ok(before);
        }
        let test = protocol::set_us_once(d);
        uncertain = test.uncertain;
        lines.push(format!("手动请求返回：{:?}", test.reply));
        if let Ok(s) = &test.after {
            lines.push(format!("自动复查国家={}\n{}", s.country, s.info));
        }
        let reply = test.reply?;
        let state = test.after?;
        if !reply.contains("Country code has changed to US") || !state.manual_us_supported() {
            return Err(format!(
                "未确认设置成功。后端返回={reply}；复查国家={}\n{}",
                state.country, state.info
            ));
        }
        Ok(state)
    })();
    if let Some(d) = driver {
        lines.extend(d.logs);
    }
    if let Err(e) = &result {
        lines.push(format!("失败 / 不确定：{e}"));
    }
    Report {
        status: result,
        logs: lines,
        uncertain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_unknown_country_allows_one_checked_setting() {
        let mut before = Status {
            country: protocol::UNKNOWN_COUNTRY.into(),
            info: "[6G Info]\n6G NOT Support".into(),
        };
        assert!(validate_setting_baseline(&before).is_ok());
        before.country = "CN".into();
        assert!(validate_setting_baseline(&before).is_ok());
        before.country.clear();
        assert!(validate_setting_baseline(&before).is_err());
    }
    #[test]
    fn unknown_driver_warns_without_bypassing_device_checks() {
        let pnp = "USB\\VID_28DE&PID_2432\\SERIAL";
        let unknown = Ok("unverified-sha256".into());
        assert!(
            driver_compatibility(pnp, "rtwlanuval", true, &unknown)
                .unwrap()
                .contains("未验证")
        );
        assert!(
            driver_compatibility(pnp, "rtwlanuval", true, &Ok(protocol::DRIVER_HASH.into()))
                .unwrap()
                .starts_with("已验证")
        );
        assert!(driver_compatibility("PCI\\OTHER", "rtwlanuval", true, &unknown).is_err());
        assert!(driver_compatibility(pnp, "other", true, &unknown).is_err());
        assert!(driver_compatibility(pnp, "rtwlanuval", false, &unknown).is_err());
        assert!(driver_compatibility(pnp, "rtwlanuval", true, &Err("unreadable".into())).is_err());
    }
    #[test]
    fn hardware_gate() {
        let devices = [
            "USB\\VID_28DE&PID_2432\\ONE",
            "PCI\\VEN_14C3&DEV_7922\\OTHER",
            "USB\\VID_28DE&PID_24320\\OTHER",
            "usb\\vid_28de&pid_2432&rev_0001\\TWO",
            "",
        ];
        let visible: Vec<_> = devices
            .iter()
            .copied()
            .filter(|id| steam_frame_hardware(id))
            .collect();
        assert_eq!(visible, [devices[0], devices[3]]);
        // A genuine device remains visible even if its driver is unsupported.
        assert!(steam_frame_hardware(devices[0]));
        assert!(!supported_hardware(devices[0], "other"));
        assert!(supported_hardware(
            "USB\\VID_28DE&PID_2432\\SERIAL",
            "rtwlanuval"
        ));
        assert!(supported_hardware(
            "usb\\vid_28de&pid_2432&rev_0001\\x",
            "RTWLANUVAL"
        ));
        for id in [
            "USB\\VID_28DE&PID_24320\\x",
            "PCI\\VID_28DE&PID_2432\\x",
            "USB\\VID_1234&PID_2432\\x",
        ] {
            assert!(!supported_hardware(id, "rtwlanuval"));
        }
        assert!(!supported_hardware("USB\\VID_28DE&PID_2432\\x", "other"));
        assert_eq!(normalized_guid("{ABC}"), "abc");
    }
    #[test]
    #[ignore = "Requires explicit approval: reads actual adapter status, never sets US"]
    fn readonly_adapter_status() {
        let adapters = enumerate().expect("enumeration");
        for a in &adapters {
            println!("{} {} {:?}", a.name, a.id, a.compatibility);
        }
        let eligible: Vec<_> = adapters.iter().filter(|a| a.supported()).collect();
        assert_eq!(
            eligible.len(),
            1,
            "Read-only smoke test requires exactly one compatible adapter"
        );
        let report = operate(eligible[0], false);
        for line in report.logs {
            println!("{line}");
        }
        let status = report.status.expect("read status");
        println!("country={}\n{}", status.country, status.info);
        assert!(status.info.contains("6G Info"));
    }
}
