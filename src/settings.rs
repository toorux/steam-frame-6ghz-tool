//! Current-user shortcuts and the selected adapter's Device Manager power setting.
use crate::{backend::{self, Adapter}, i18n::t};
use base64::Engine;
use std::{fs, io::{Read, Write}, os::windows::{ffi::OsStrExt, process::CommandExt}, path::{Path, PathBuf}, process::{Command, Stdio},
    sync::mpsc::{self, Receiver}, thread, time::{Duration, Instant}};
use windows_sys::Win32::{Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    Storage::FileSystem::ReplaceFileW,
    System::{Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS},
        Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ}}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Desktop, StartMenu, RemoveStartMenu, ReadPower, DisablePower }

struct Output { message: String, power: Option<(String, bool)> }
type MultiLinkEvent = (Option<bool>, Result<bool, String>);

#[derive(Default)]
pub struct Settings {
    receiver: Option<Receiver<(Action, Result<Output, String>)>>,
    pub notice: Option<Result<String, String>>,
    pub power: Option<(String, bool)>,
    pub power_error: Option<String>,
    pub target: Option<Adapter>,
    multilink_receiver: Option<Receiver<MultiLinkEvent>>,
    pub multilink: Option<bool>,
    pub multilink_error: Option<String>,
}

impl Settings {
    pub fn busy(&self) -> bool { self.receiver.is_some() }

    pub fn multilink_busy(&self) -> bool { self.multilink_receiver.is_some() }

    pub fn start_multilink(&mut self, target: Option<bool>, demo: bool) {
        if self.multilink_busy() { return; }
        self.multilink_error = None;
        if target.is_none() { self.multilink = None; }
        let (tx, rx) = mpsc::channel();
        self.multilink_receiver = Some(rx);
        thread::spawn(move || {
            let result = if demo { Ok(target.unwrap_or(true)) }
                else if let Some(value) = target { set_multilink(value) }
                else { read_multilink() };
            let _ = tx.send((target, result));
        });
    }

    pub fn poll_multilink(&mut self) -> Option<String> {
        let event = match self.multilink_receiver.as_ref()?.try_recv() {
            Ok(event) => event,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => (None, Err(t("多链路操作意外中断。", "Multi-link operation stopped unexpectedly.").into())),
        };
        self.multilink_receiver = None;
        let (target, result) = event;
        match result {
            Ok(enabled) => {
                self.multilink = Some(enabled);
                let message = if enabled { t("多链路串流已开启。", "Multi-link streaming is on.") }
                    else { t("多链路串流已关闭。", "Multi-link streaming is off.") }.to_string();
                if target.is_some() { self.notice = Some(Ok(message.clone())); }
                Some(message)
            }
            Err(error) => {
                self.multilink = None;
                self.multilink_error = Some(error.clone());
                if target.is_some() { self.notice = Some(Err(error.clone())); }
                Some(format!("[ERROR] {error}"))
            }
        }
    }

    pub fn start(&mut self, action: Action, adapter: Option<Adapter>, demo: bool) {
        if self.busy() { return; }
        if action == Action::ReadPower {
            self.power = None;
            self.power_error = None;
        }
        self.notice = None;
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        thread::spawn(move || {
            let result = if demo {
                Ok(Output { message: t("演示模式：未修改系统。", "Demo mode: no system changes.").into(), power: None })
            } else { perform(action, adapter.as_ref()) };
            let _ = tx.send((action, result));
        });
    }

    pub fn poll(&mut self) -> Option<String> {
        let event = match self.receiver.as_ref()?.try_recv() {
            Ok(event) => event,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => (Action::DisablePower,
                Err(t("操作线程意外结束，请重新检查状态。", "The operation stopped unexpectedly. Check the status again.").into())),
        };
        self.receiver = None;
        let (action, result) = event;
        let message = match result {
            Ok(output) => {
                if let Some(power) = output.power { self.power = Some(power); self.power_error = None; }
                Ok(output.message)
            }
            Err(error) => {
                if matches!(action, Action::ReadPower | Action::DisablePower) {
                    self.power = None;
                    self.power_error = Some(error.clone());
                }
                Err(error)
            }
        };
        let log = match &message { Ok(text) => text.clone(), Err(text) => format!("[ERROR] {text}") };
        if action != Action::ReadPower { self.notice = Some(message); }
        Some(log)
    }
}

fn perform(action: Action, adapter: Option<&Adapter>) -> Result<Output, String> {
    if matches!(action, Action::ReadPower | Action::DisablePower) {
        let adapter = adapter.ok_or_else(|| t("请先在本机设置中选择 Steam Frame 网卡。", "Select a Steam Frame adapter on the This PC tab first.").to_string())?;
        let output = run(POWER, &[("SF_ADAPTER_ID", adapter.id.clone()), ("SF_ADAPTER_PNP", adapter.pnp.clone()),
            ("SF_DISABLE_POWER", (action == Action::DisablePower).to_string())])
            .map_err(|e| format!("{}\n{e}", t("无法读取或修改网卡节能设置。请确认设备已连接、驱动支持此设置，并以管理员身份运行。", "Could not access adapter power management. Check that the adapter is connected, the driver supports this setting, and the app is running as administrator.")))?;
        let enabled = match output.trim() {
            "ENABLED" => true, "DISABLED" => false,
            _ => return Err(t("无法确认节能设置的最终状态。", "Could not verify the final power-saving setting.").into()),
        };
        if action == Action::DisablePower && enabled {
            return Err(t("节能设置仍然开启，未能确认修改成功。", "Power saving is still enabled; the change could not be verified.").into());
        }
        return Ok(Output { message: if enabled {
            t("网卡节能已开启。", "Adapter power saving is enabled.")
        } else { t("网卡节能已关闭。", "Adapter power saving is disabled.") }.into(), power: Some((adapter.id.clone(), enabled)) });
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let output = run(SHORTCUT, &[("SF_EXE", exe.to_string_lossy().into_owned()),
        ("SF_SHORTCUT_FOLDER", if action == Action::Desktop { "DesktopDirectory" } else { "Programs" }.into()),
        ("SF_SHORTCUT_REMOVE", (action == Action::RemoveStartMenu).to_string())])?;
    Ok(Output { message: format!("{}\n{}", if action == Action::RemoveStartMenu {
        t("开始菜单快捷方式已移除（若原本不存在则无需处理）。", "The Start menu shortcut has been removed, or was already absent.")
    } else { t("快捷方式已创建。请不要移动或删除当前程序文件。", "Shortcut created. Keep the app in its current location.") }, output.trim()), power: None })
}

fn registry_string(root: HKEY, key: &str, name: &str) -> Option<String> {
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let key = wide(key);
    let name = wide(name);
    let mut bytes = 0u32;
    if unsafe { RegGetValueW(root, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut bytes) } != 0 {
        return None;
    }
    let mut value = vec![0u16; bytes as usize / 2];
    if unsafe { RegGetValueW(root, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), value.as_mut_ptr().cast(), &mut bytes) } != 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&value[..value.iter().position(|ch| *ch == 0).unwrap_or(value.len())]))
}

fn steamvr_paths() -> Result<(PathBuf, PathBuf), String> {
    let root = registry_string(HKEY_LOCAL_MACHINE, "SOFTWARE\\WOW6432Node\\Valve\\Steam", "InstallPath")
        .or_else(|| registry_string(HKEY_CURRENT_USER, "Software\\Valve\\Steam", "SteamPath"))
        .ok_or_else(|| t("找不到 Steam 安装位置。", "Steam installation was not found.").to_string())?;
    let root = PathBuf::from(root);
    let user = root.join("config/steamvr.vrsettings");
    let default = root.join("steamapps/common/SteamVR/drivers/vrlink/resources/settings/default.vrsettings");
    if !default.is_file() {
        return Err(t("找不到 SteamVR 的多链路设置定义；请确认 SteamVR 已安装在 Steam 默认库。", "SteamVR's multi-link setting was not found. Check that SteamVR is installed in the default Steam library.").into());
    }
    Ok((user, default))
}

fn multilink_value(bytes: &[u8]) -> Result<Option<bool>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("{}: {e}", t("SteamVR 设置文件不是有效 JSON", "SteamVR settings are not valid JSON")))?;
    let Some(section) = value.get("driver_vrlink") else { return Ok(None); };
    let Some(section) = section.as_object() else {
        return Err(t("SteamVR 的 driver_vrlink 设置格式异常。", "SteamVR's driver_vrlink settings have an invalid format.").into());
    };
    match section.get("allowMultipleLinks") {
        None => Ok(None),
        Some(value) => value.as_bool().map(Some).ok_or_else(|| t("多链路设置不是布尔值；未修改。", "Multi-link setting is not a boolean; no changes made.").into()),
    }
}

fn read_multilink() -> Result<bool, String> {
    let (user, default) = steamvr_paths()?;
    let fallback = fs::read(&default).map_err(|e| e.to_string())?;
    let fallback = multilink_value(&fallback)?.ok_or_else(|| t("SteamVR 未定义多链路默认值。", "SteamVR does not define a multi-link default.").to_string())?;
    match fs::read(&user) {
        Ok(bytes) => Ok(multilink_value(&bytes)?.unwrap_or(fallback)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(fallback),
        Err(error) => Err(error.to_string()),
    }
}

fn steamvr_running() -> Result<bool, String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE { return Err(std::io::Error::last_os_error().to_string()); }
    let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut found = unsafe { Process32FirstW(snapshot, &mut entry) };
    let mut running = false;
    while found != 0 {
        let end = entry.szExeFile.iter().position(|ch| *ch == 0).unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
        if ["vrserver.exe", "vrmonitor.exe", "vrcompositor.exe", "vrdashboard.exe"].iter()
            .any(|process| name.eq_ignore_ascii_case(process)) { running = true; break; }
        found = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    unsafe { CloseHandle(snapshot); }
    Ok(running)
}

fn updated_multilink(bytes: Option<&[u8]>, enabled: bool) -> Result<Vec<u8>, String> {
    let mut value: serde_json::Value = match bytes {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|e| e.to_string())?,
        None => serde_json::json!({}),
    };
    let object = value.as_object_mut().ok_or_else(|| t("SteamVR 设置文件格式异常；未修改。", "SteamVR settings have an invalid format; no changes made.").to_string())?;
    let section = object.entry("driver_vrlink").or_insert_with(|| serde_json::json!({}));
    let section = section.as_object_mut().ok_or_else(|| t("driver_vrlink 设置格式异常；未修改。", "driver_vrlink settings have an invalid format; no changes made.").to_string())?;
    if section.get("allowMultipleLinks").is_some_and(|v| !v.is_boolean()) {
        return Err(t("多链路设置不是布尔值；未修改。", "Multi-link setting is not a boolean; no changes made.").into());
    }
    section.insert("allowMultipleLinks".into(), enabled.into());
    let mut encoded = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn set_multilink(enabled: bool) -> Result<bool, String> {
    let (path, _) = steamvr_paths()?;
    if steamvr_running()? {
        return Err(t("请先关闭 SteamVR，再修改多链路串流。", "Close SteamVR before changing multi-link streaming.").into());
    }
    let old = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    if let Some(bytes) = &old {
        if fs::symlink_metadata(&path).map_err(|e| e.to_string())?.file_type().is_symlink() {
            return Err(t("SteamVR 设置文件是符号链接；未修改。", "SteamVR settings are a symlink; no changes made.").into());
        }
        if multilink_value(bytes)? == Some(enabled) { return Ok(enabled); }
    }
    let updated = updated_multilink(old.as_deref(), enabled)?;
    let suffix = format!("{}.{}", std::process::id(), std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos());
    let temp = path.with_file_name(format!("steamvr.vrsettings.tmp.{suffix}"));
    let backup = path.with_file_name(format!("steamvr.vrsettings.backup.{suffix}"));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|e| e.to_string())?;
    let result = (|| -> Result<(), String> {
        file.write_all(&updated).and_then(|_| file.sync_all()).map_err(|e| e.to_string())?;
        drop(file);
        if steamvr_running()? || fs::read(&path).ok() != old {
            return Err(t("SteamVR 已启动或配置在写入期间发生变化；未修改。", "SteamVR started or its settings changed during the write; no changes made.").into());
        }
        if old.is_some() {
            let wide = |path: &Path| path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
            if unsafe { ReplaceFileW(wide(&path).as_ptr(), wide(&temp).as_ptr(), wide(&backup).as_ptr(), 0, std::ptr::null(), std::ptr::null()) } == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
        } else { fs::rename(&temp, &path).map_err(|e| e.to_string())?; }
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(&temp); }
    result?;
    if read_multilink()? != enabled { return Err(t("写入后复查失败；请检查 SteamVR 设置。", "Read-back failed; check SteamVR settings.").into()); }
    Ok(enabled)
}

// User paths/identifiers are environment data, never interpolated into PowerShell source.
fn run(script: &str, env: &[(&str, String)]) -> Result<String, String> {
    let script = format!("$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); try {{ {script} }} catch {{ [Console]::Error.WriteLine($_.InvocationInfo.PositionMessage + ' ' + $_.Exception.Message); exit 1 }}");
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut child = Command::new(backend::system_dir()?.join("WindowsPowerShell/v1.0/powershell.exe"))
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", &base64::engine::general_purpose::STANDARD.encode(bytes)])
        .envs(env.iter().map(|(k, v)| (*k, v))).creation_flags(0x08000000)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| e.to_string())?;
    fn drain(mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
        let mut retained = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let n = pipe.read(&mut buffer)?;
            if n == 0 { return Ok(retained); }
            let keep = n.min(65536usize.saturating_sub(retained.len()));
            retained.extend_from_slice(&buffer[..keep]);
        }
    }
    let stdout = child.stdout.take().ok_or("Missing stdout")?;
    let stderr = child.stderr.take().ok_or("Missing stderr")?;
    let out = thread::spawn(move || drain(stdout));
    let err = thread::spawn(move || drain(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < Duration::from_secs(25) => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill(); let _ = child.wait();
                break Err(t("操作超时或中断；结果未确认，请检查系统状态后再试。", "The operation timed out or was interrupted. Its result is unconfirmed; check the system before retrying.").to_string());
            }
        }
    };
    let output = out.join().map_err(|_| "Output reader stopped")?.map_err(|e| e.to_string())?;
    let error = err.join().map_err(|_| "Error reader stopped")?.map_err(|e| e.to_string())?;
    if !status?.success() { return Err(String::from_utf8_lossy(&error).trim().to_string()); }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

const SHORTCUT: &str = r#"
$folder = [Environment]::GetFolderPath($env:SF_SHORTCUT_FOLDER)
if ([string]::IsNullOrWhiteSpace($folder) -or !(Test-Path -LiteralPath $folder -PathType Container)) { throw 'The user shortcut folder is unavailable.' }
$path = Join-Path $folder 'Steam Frame 6GHz Tool.lnk'
$shell = New-Object -ComObject WScript.Shell
$marker = 'Steam Frame 6GHz Tool shortcut'
if (Test-Path -LiteralPath $path) {
    $item = Get-Item -LiteralPath $path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'The shortcut path is not a regular file.' }
    $existing = $shell.CreateShortcut($path)
    if ($existing.Description -ne $marker) { throw 'A shortcut with this name already exists and was not created by this tool. It has not been changed.' }
}
if ($env:SF_SHORTCUT_REMOVE -eq 'true') {
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
    if (Test-Path -LiteralPath $path) { throw 'Shortcut removal could not be verified.' }
} else {
    $shortcut = $shell.CreateShortcut($path)
    $shortcut.TargetPath = $env:SF_EXE
    $shortcut.Arguments = ''
    $shortcut.WorkingDirectory = Split-Path -LiteralPath $env:SF_EXE
$shortcut.IconLocation = $env:SF_EXE + ',0'
    $shortcut.Description = $marker
    $shortcut.Save()
    $saved = $shell.CreateShortcut($path)
    if (!(Test-Path -LiteralPath $path -PathType Leaf) -or $saved.TargetPath -ne $env:SF_EXE -or $saved.Arguments -ne '') { throw 'Shortcut verification failed.' }
}
[Console]::WriteLine($path)
"#;

// Device Manager's power-management checkbox; never touches advanced driver
// properties, wake permissions, other adapters, or global USB/power policies.
const POWER: &str = r#"
$id = [guid]::Parse($env:SF_ADAPTER_ID)
$devices = @(Get-CimInstance -ClassName Win32_NetworkAdapter | Where-Object { $_.GUID -and ([guid]$_.GUID) -eq $id })
if ($devices.Count -ne 1) { throw 'The selected network adapter is missing or ambiguous.' }
$pnp = $devices[0].PNPDeviceID
if ($pnp -ine $env:SF_ADAPTER_PNP -or $pnp -notmatch '^USB\\VID_28DE&PID_2432(?:&[^\\]+)?\\') { throw 'The selected device is not the expected Steam Frame USB adapter.' }
$pattern = '^' + [regex]::Escape($pnp) + '_[0-9]+$'
$items = @(Get-CimInstance -Namespace root/wmi -ClassName MSPower_DeviceEnable | Where-Object { $_.InstanceName -ieq $pnp -or $_.InstanceName -imatch $pattern })
if ($items.Count -ne 1) { throw 'The driver does not expose one unambiguous Device Manager power setting. No changes were made.' }
$power = $items[0]
if ($env:SF_DISABLE_POWER -eq 'true' -and $power.Enable) {
    $power | Set-CimInstance -Property @{ Enable = $false } -ErrorAction Stop | Out-Null
}
$verified = @($power | Get-CimInstance -ErrorAction Stop)
if ($verified.Count -ne 1 -or $null -eq $verified[0].Enable) { throw 'Could not read back the power setting.' }
if ($verified[0].Enable) { [Console]::WriteLine('ENABLED') } else { [Console]::WriteLine('DISABLED') }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_report_failure_and_prevent_duplicates() {
        let mut state = Settings::default();
        let (tx, rx) = mpsc::channel();
        state.receiver = Some(rx);
        state.start(Action::Desktop, None, false);
        tx.send((Action::DisablePower, Err("access denied".into()))).unwrap();
        assert_eq!(state.poll().as_deref(), Some("[ERROR] access denied"));
        assert!(!state.busy());
        assert!(state.notice.as_ref().unwrap().is_err());
        assert!(state.power.is_none());
    }

    #[test]
    fn multilink_edit_preserves_other_steamvr_settings() {
        let original = br#"{"driver_vrlink":{"allowMultipleLinks":false,"targetBandwidth":200},"steamvr":{"showAdvancedSettings":true}}"#;
        assert_eq!(multilink_value(original).unwrap(), Some(false));
        let changed = updated_multilink(Some(original), true).unwrap();
        assert_eq!(multilink_value(&changed).unwrap(), Some(true));
        let value: serde_json::Value = serde_json::from_slice(&changed).unwrap();
        assert_eq!(value["driver_vrlink"]["targetBandwidth"], 200);
        assert_eq!(value["steamvr"]["showAdvancedSettings"], true);
        assert!(updated_multilink(Some(br#"{"driver_vrlink":{"allowMultipleLinks":"false"}}"#), true).is_err());
        assert_eq!(multilink_value(br#"{"driver_vrlink":{}}"#).unwrap(), None);
    }

    #[test]
    #[ignore = "Read-only check of the locally installed SteamVR configuration"]
    fn installed_multilink_setting_can_be_read() {
        assert!(steamvr_paths().unwrap().1.is_file());
        assert!(matches!(read_multilink(), Ok(true | false)));
    }

    #[test]
    fn windows_replacement_preserves_a_backup() {
        let suffix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("steam-frame-multilink-{}-{suffix}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let old = dir.join("steamvr.vrsettings");
        let new = dir.join("new.vrsettings");
        let backup = dir.join("backup.vrsettings");
        fs::write(&old, b"old").unwrap();
        fs::write(&new, b"new").unwrap();
        let wide = |path: &Path| path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        assert_ne!(unsafe { ReplaceFileW(wide(&old).as_ptr(), wide(&new).as_ptr(), wide(&backup).as_ptr(), 0, std::ptr::null(), std::ptr::null()) }, 0);
        assert_eq!(fs::read(&old).unwrap(), b"new");
        assert_eq!(fs::read(&backup).unwrap(), b"old");
        fs::remove_file(old).unwrap();
        fs::remove_file(backup).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn powershell_scripts_parse_without_running_them() {
        for source in [SHORTCUT, POWER] {
            run("$tokens=$null; $errors=$null; [void][Management.Automation.Language.Parser]::ParseInput($env:SF_SCRIPT, [ref]$tokens, [ref]$errors); if ($errors.Count) { throw ($errors | Out-String) }", &[("SF_SCRIPT", source.into())]).unwrap();
        }
    }

    #[test]
    fn power_script_targets_one_device_and_verifies_the_write() {
        // Mock CIM commands; this test must never access or change actual hardware.
        let mock = r#"
$script:selected = [pscustomobject]@{InstanceName=$env:SF_ADAPTER_PNP+'_0'; Enable=$true}
$script:other = [pscustomobject]@{InstanceName='USB\VID_1234&PID_5678\other_0'; Enable=$true}
$script:writes=0
function Get-CimInstance {
    param([Parameter(ValueFromPipeline=$true)]$InputObject, $ClassName, $Namespace)
    process {
        if ($InputObject) { $InputObject }
        elseif ($ClassName -eq 'Win32_NetworkAdapter') { [pscustomobject]@{GUID=$env:SF_ADAPTER_ID; PNPDeviceID=$env:SF_ADAPTER_PNP} }
        else { $script:selected; $script:other; if ($env:SF_DUPLICATE -eq 'true') { $script:selected } }
    }
}
function Set-CimInstance {
    param([Parameter(ValueFromPipeline=$true)]$InputObject, $Property)
    process {
        if ($InputObject -ne $script:selected) { throw 'Wrong target' }
        $script:writes++
        if ($env:SF_REFUSE -ne 'true') { $InputObject.Enable=$Property.Enable }
    }
}
"#;
        let mut env = vec![("SF_ADAPTER_ID", "a7acc371-e53d-4a63-88bc-088d01bb4603".into()),
            ("SF_ADAPTER_PNP", "USB\\VID_28DE&PID_2432\\test".into()), ("SF_DISABLE_POWER", "false".into()),
            ("SF_DUPLICATE", "false".into()), ("SF_REFUSE", "false".into())];
        let script = format!("{mock}\n{POWER}\nif (!$script:other.Enable) {{ throw 'Other adapter changed' }}");
        assert_eq!(run(&script, &env).unwrap().trim(), "ENABLED");
        env[2].1 = "true".into();
        assert_eq!(run(&script, &env).unwrap().trim(), "DISABLED");
        env[4].1 = "true".into();
        assert_eq!(run(&script, &env).unwrap().trim(), "ENABLED"); // Rust refuses to call this success.
        env[3].1 = "true".into();
        assert!(run(&script, &env).is_err());
        env[1].1 = "USB\\VID_1234&PID_5678\\wrong".into();
        assert!(run(&script, &env).is_err());
    }
}
