//! Current-user shortcuts and the selected adapter's Device Manager power setting.
use crate::{backend::{self, Adapter}, i18n::t};
use base64::Engine;
use std::{io::Read, os::windows::process::CommandExt, process::{Command, Stdio},
    sync::mpsc::{self, Receiver}, thread, time::{Duration, Instant}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Desktop, StartMenu, RemoveStartMenu, ReadPower, DisablePower }

struct Output { message: String, power: Option<(String, bool)> }

#[derive(Default)]
pub struct Settings {
    receiver: Option<Receiver<(Action, Result<Output, String>)>>,
    pub notice: Option<Result<String, String>>,
    pub power: Option<(String, bool)>,
    pub power_error: Option<String>,
    pub target: Option<Adapter>,
}

impl Settings {
    pub fn busy(&self) -> bool { self.receiver.is_some() }

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
    fn powershell_scripts_parse_without_running_them() {
        for source in [SHORTCUT, POWER] {
            run("$tokens=$null; $errors=$null; [void][Management.Automation.Language.Parser]::ParseInput($env:SF_SCRIPT, [ref]$tokens, [ref]$errors); if ($errors.Count) { throw ($errors | Out-String) }", &[("SF_SCRIPT", source.into())]).unwrap();
        }
    }

    #[test]
    fn shortcuts_roundtrip_in_a_temporary_folder() {
        let folder = std::env::temp_dir().join(format!("steam-frame-shortcut-test-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&folder).unwrap();
        let exe = folder.join("测试 '[app].exe");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let script = SHORTCUT.replace("[Environment]::GetFolderPath($env:SF_SHORTCUT_FOLDER)", "$env:SF_TEST_FOLDER");
        let mut env = vec![("SF_TEST_FOLDER", folder.to_string_lossy().into_owned()),
            ("SF_EXE", exe.to_string_lossy().into_owned()), ("SF_SHORTCUT_REMOVE", "false".into())];
        run(&script, &env).unwrap();
        run(&script, &env).unwrap(); // Idempotent creation.
        assert!(folder.join("Steam Frame 6GHz Tool.lnk").is_file());
        // An unrelated same-name shortcut must never be overwritten or deleted.
        run("$s=New-Object -ComObject WScript.Shell; $l=$s.CreateShortcut((Join-Path $env:SF_TEST_FOLDER 'Steam Frame 6GHz Tool.lnk')); $l.Description='Someone else'; $l.Save()", &env).unwrap();
        assert!(run(&script, &env).is_err());
        env[2].1 = "true".into();
        assert!(run(&script, &env).is_err());
        run("$s=New-Object -ComObject WScript.Shell; $l=$s.CreateShortcut((Join-Path $env:SF_TEST_FOLDER 'Steam Frame 6GHz Tool.lnk')); $l.Description='Steam Frame 6GHz Tool shortcut'; $l.Save()", &env).unwrap();
        run(&script, &env).unwrap();
        run(&script, &env).unwrap(); // Removing an absent shortcut is harmless.
        assert!(!folder.join("Steam Frame 6GHz Tool.lnk").exists());
        std::fs::remove_file(exe).unwrap();
        std::fs::remove_dir(folder).unwrap();
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
