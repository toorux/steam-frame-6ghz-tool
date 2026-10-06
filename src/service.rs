//! Optional, device-triggered service. No installation or writes on normal UI startup.
use crate::{backend, logs, protocol::Result};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString, c_void},
    fs::{self, OpenOptions},
    io::Write,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::*,
        NetworkManagement::Ndis::GUID_NDIS_LAN_CLASS,
        Security::{Authorization::*, SECURITY_ATTRIBUTES},
        Storage::FileSystem::{CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT},
        System::{Com::CoTaskMemFree, Services::*},
        UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath},
    },
    core::w,
};

const NAME: windows_sys::core::PCWSTR = w!("SteamFrame6GHzAutoApply");
const EXE: &str = "steam-frame-6ghz-tool.exe";
const LOG: &str = "auto-apply.log";
const PAUSED: &str = "auto-apply.paused";
const LOG_CONFIG: &str = "log-directory.json";
const READY_TIMEOUT: Duration = Duration::from_secs(60);

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
fn win_error(context: &str) -> String {
    describe_error(context, std::io::Error::last_os_error())
}
fn describe_error(context: &str, error: std::io::Error) -> String {
    if matches!(error.raw_os_error(), Some(5 | 1314)) {
        format!("权限不足：请关闭程序，右键选择“以管理员身份运行”，再重试。\n{context}：{error}")
    } else {
        format!("{context}：{error}")
    }
}
fn check(ok: i32, context: &str) -> Result<()> {
    if ok == 0 {
        Err(win_error(context))
    } else {
        Ok(())
    }
}

struct Sc(SC_HANDLE);
impl Sc {
    fn manager(access: u32) -> Result<Self> {
        let h = unsafe { OpenSCManagerW(null(), null(), access) };
        if h.is_null() {
            Err(win_error("访问服务管理器"))
        } else {
            Ok(Self(h))
        }
    }
    fn open(&self, access: u32) -> Result<Option<Self>> {
        let h = unsafe { OpenServiceW(self.0, NAME, access) };
        if !h.is_null() {
            return Ok(Some(Self(h)));
        }
        if unsafe { GetLastError() } == ERROR_SERVICE_DOES_NOT_EXIST {
            Ok(None)
        } else {
            Err(win_error("读取自动应用服务"))
        }
    }
}
impl Drop for Sc {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}

pub fn directory() -> Result<PathBuf> {
    let mut raw = null_mut();
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, null_mut(), &mut raw) };
    if hr < 0 {
        return Err(format!("读取 Program Files 失败：{hr:#x}"));
    }
    let path = unsafe {
        let mut len = 0;
        while *raw.add(len) != 0 {
            len += 1;
        }
        let value = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(raw, len)));
        CoTaskMemFree(raw.cast());
        value
    };
    Ok(path.join("Steam Frame 6 GHz Tool"))
}

fn command(dir: &Path) -> String {
    format!("\"{}\" --service", dir.join(EXE).display())
}

fn configured_log_directory() -> Result<PathBuf> {
    let bytes = fs::read(directory()?.join(LOG_CONFIG))
        .map_err(|e| format!("读取日志位置失败，请重新安装自动服务：{e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("日志位置配置无效：{e}"))
}
pub fn log_directory() -> Result<PathBuf> {
    if directory()?
        .join(LOG_CONFIG)
        .try_exists()
        .map_err(|e| e.to_string())?
    {
        configured_log_directory()
    } else {
        logs::beside(&std::env::current_exe().map_err(|e| e.to_string())?)
    }
}

struct Run {
    state_dir: PathBuf,
    logs: logs::Session,
}

// Refuse to operate on another service that happens to use our name.
fn verify_service(service: &Sc, dir: &Path) -> Result<bool> {
    let mut storage = [0usize; 1024]; // Aligned, 8 KiB QueryServiceConfig buffer.
    let mut needed = 0;
    check(
        unsafe {
            QueryServiceConfigW(
                service.0,
                storage.as_mut_ptr().cast(),
                size_of_val(&storage) as u32,
                &mut needed,
            )
        },
        "核验服务配置",
    )?;
    let config = unsafe { &*storage.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    let actual = unsafe {
        let mut len = 0;
        while *config.lpBinaryPathName.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(config.lpBinaryPathName, len))
    };
    if actual != command(dir) || config.dwServiceType != SERVICE_WIN32_OWN_PROCESS {
        return Err("同名服务不是本工具安装的配置，未修改".into());
    }
    Ok(config.dwStartType != SERVICE_DISABLED)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct State {
    pub installed: bool,
    pub paused: bool,
    pub running: bool,
    pub enabled: bool,
    pub exit_code: u32,
    pub needs_update: bool,
    pub needs_repair: bool,
}
fn different_binary(installed: &[u8], current: &[u8]) -> bool {
    installed.len() != current.len() || Sha256::digest(installed) != Sha256::digest(current)
}
fn copy_state(installed: std::io::Result<Vec<u8>>, current: &[u8]) -> Result<(bool, bool)> {
    match installed {
        Ok(installed) => Ok((different_binary(&installed, current), false)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((false, true)),
        Err(e) => Err(format!("读取服务程序副本失败：{e}")),
    }
}
pub fn state() -> Result<State> {
    let manager = Sc::manager(SC_MANAGER_CONNECT)?;
    let Some(service) = manager.open(SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)? else {
        return Ok(State::default());
    };
    let dir = directory()?;
    let enabled = verify_service(&service, &dir)?;
    let mut status = SERVICE_STATUS::default();
    check(
        unsafe { QueryServiceStatus(service.0, &mut status) },
        "读取服务运行状态",
    )?;
    let running = status.dwCurrentState != SERVICE_STOPPED;
    let current = fs::read(std::env::current_exe().map_err(|e| e.to_string())?)
        .map_err(|e| format!("读取当前程序失败：{e}"))?;
    let (needs_update, needs_repair) = copy_state(fs::read(dir.join(EXE)), &current)?;
    Ok(State {
        installed: true,
        paused: !running && dir.join(PAUSED).exists(),
        running,
        enabled,
        exit_code: status.dwWin32ExitCode,
        needs_update,
        needs_repair,
    })
}

pub fn reinstall() -> Result<String> {
    let current = state()?;
    if !current.installed {
        return Err("自动应用未安装".into());
    }
    if current.running || current.paused {
        return Err("服务正在处理或已暂停；先查看日志并人工复查，暂不更新".into());
    }
    if !current.needs_update && !current.needs_repair {
        return Ok("服务副本已是当前版本，无需更新".into());
    }
    uninstall()?;
    install().map_err(|e| format!("旧服务已卸载，但重新安装失败：{e}"))
}

fn clear_empty_install_directory(dir: &Path) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("检查服务安装目录失败：{e}")),
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(format!("服务安装目录已存在且不是普通目录：{}；未接管", dir.display()));
            }
            // Remove only an empty orphan, then create it atomically with our protected ACL.
            fs::remove_dir(dir).map_err(|e| format!("服务安装目录已存在且无法安全清理：{}：{e}", dir.display()))
        }
    }
}

fn secure_directory(dir: &Path) -> Result<()> {
    // Created atomically with a protected DACL. Never adopt an existing directory,
    // junction or user-controlled executable for a LocalSystem service.
    let mut descriptor = null_mut();
    check(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;BU)"),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        },
        "创建安装目录权限",
    )?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let result = check(
        unsafe { CreateDirectoryW(wide(dir).as_ptr(), &attributes) },
        "创建受保护目录（已有目录不会覆盖）",
    );
    unsafe {
        LocalFree(descriptor);
    }
    result
}

fn set_trigger(service: &Sc) -> Result<()> {
    let mut id = wide("USB\\VID_28DE&PID_2432");
    let mut data = SERVICE_TRIGGER_SPECIFIC_DATA_ITEM {
        dwDataType: SERVICE_TRIGGER_DATA_TYPE_STRING,
        cbData: (id.len() * 2) as u32,
        pData: id.as_mut_ptr().cast(),
    };
    let mut class = GUID_NDIS_LAN_CLASS;
    let mut trigger = SERVICE_TRIGGER {
        dwTriggerType: SERVICE_TRIGGER_TYPE_DEVICE_INTERFACE_ARRIVAL,
        dwAction: SERVICE_TRIGGER_ACTION_SERVICE_START,
        pTriggerSubtype: &mut class,
        cDataItems: 1,
        pDataItems: &mut data,
    };
    let info = SERVICE_TRIGGER_INFO {
        cTriggers: 1,
        pTriggers: &mut trigger,
        pReserved: null_mut(),
    };
    check(
        unsafe {
            ChangeServiceConfig2W(
                service.0,
                SERVICE_CONFIG_TRIGGER_INFO,
                (&info as *const SERVICE_TRIGGER_INFO).cast(),
            )
        },
        "设置设备到达触发器",
    )
}

pub fn install() -> Result<String> {
    let manager = Sc::manager(SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE)?;
    if manager.open(SERVICE_QUERY_CONFIG)?.is_some() {
        return Err("自动应用服务已存在；更新或解除暂停请先卸载再开启".into());
    }
    let dir = directory()?;
    clear_empty_install_directory(&dir)?;
    secure_directory(&dir)?;
    let result = (|| {
        let source = std::env::current_exe().map_err(|e| e.to_string())?;
        let log_dir = logs::beside(&source)?;
        let install_log = logs::Session::new(&log_dir)?;
        install_log.write("安装自动应用服务；日志位置为原程序旁的 logs 目录")?;
        fs::write(
            dir.join(LOG_CONFIG),
            serde_json::to_vec(&log_dir).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::copy(source, dir.join(EXE)).map_err(|e| e.to_string())?;
        // Disabled until trigger registration succeeds, so partial installation cannot run.
        let raw = unsafe {
            CreateServiceW(
                manager.0,
                NAME,
                w!("Steam Frame 6 GHz 自动应用"),
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_DISABLED,
                SERVICE_ERROR_NORMAL,
                wide(command(&dir)).as_ptr(),
                null(),
                null_mut(),
                w!("WlanSvc\0"),
                null(),
                null(),
            )
        };
        if raw.is_null() {
            return Err(win_error("创建自动应用服务"));
        }
        let service = Sc(raw);
        if let Err(e) = set_trigger(&service) {
            if unsafe { DeleteService(service.0) } == 0 {
                return Err(format!("{e}；回滚失败，服务保持禁用，请通过关闭并卸载清理"));
            }
            return Err(e);
        }
        check(
            unsafe {
                ChangeServiceConfigW(
                    service.0,
                    SERVICE_NO_CHANGE,
                    SERVICE_DEMAND_START,
                    SERVICE_NO_CHANGE,
                    null(),
                    null(),
                    null_mut(),
                    null(),
                    null(),
                    null(),
                    null(),
                )
            },
            "启用触发服务",
        )?;
        // Apply to an already-connected device too; a trigger may have raced this call.
        if unsafe { StartServiceW(service.0, 0, null()) } == 0
            && unsafe { GetLastError() } != ERROR_SERVICE_ALREADY_RUNNING
        {
            return Err(win_error("服务已安装，但首次启动失败；可关闭并卸载后重试"));
        }
        Ok(format!(
            "自动应用已开启（本机所有 Steam Frame 适配器）。日志：{}",
            log_dir.display()
        ))
    })();
    // Only remove our freshly-created files if there is no registered service left.
    if result.is_err() && matches!(manager.open(SERVICE_QUERY_CONFIG), Ok(None)) {
        let _ = fs::remove_file(dir.join(EXE));
        let _ = fs::remove_file(dir.join(LOG_CONFIG));
        let _ = fs::remove_dir(&dir);
    }
    result
}

pub fn uninstall() -> Result<String> {
    let manager = Sc::manager(SC_MANAGER_CONNECT)?;
    let dir = directory()?;
    let Some(service) = manager.open(SERVICE_ALL_ACCESS)? else {
        return Ok("自动应用未安装".into());
    };
    verify_service(&service, &dir)?;
    check(
        unsafe {
            ChangeServiceConfigW(
                service.0,
                SERVICE_NO_CHANGE,
                SERVICE_DISABLED,
                SERVICE_NO_CHANGE,
                null(),
                null(),
                null_mut(),
                null(),
                null(),
                null(),
                null(),
            )
        },
        "禁用自动应用",
    )?;
    let mut status = SERVICE_STATUS::default();
    check(
        unsafe { QueryServiceStatus(service.0, &mut status) },
        "读取服务状态",
    )?;
    if status.dwCurrentState != SERVICE_STOPPED {
        if unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } == 0
            && unsafe { GetLastError() } != ERROR_SERVICE_NOT_ACTIVE
        {
            return Err(win_error("服务已禁用，但停止失败；稍后再卸载"));
        }
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            check(
                unsafe { QueryServiceStatus(service.0, &mut status) },
                "等待服务停止",
            )?;
            if status.dwCurrentState == SERVICE_STOPPED {
                break;
            }
            if Instant::now() >= deadline {
                return Err("服务已禁用，当前操作尚未结束；稍后再卸载".into());
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
    check(unsafe { DeleteService(service.0) }, "删除自动应用服务")?;
    drop(service);
    // Do not recursively delete anything, or follow a replaced installation junction.
    if let Ok(metadata) = fs::symlink_metadata(&dir) {
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("服务已移除，但安装目录为链接；未删除文件".into());
        }
        for name in [EXE, LOG, "auto-apply.previous.log", PAUSED, LOG_CONFIG] {
            let file = dir.join(name);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match fs::remove_file(&file) {
                    Ok(()) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                    Err(_) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(200))
                    }
                    Err(e) => {
                        return Err(format!("服务已移除；文件 {} 未能删除：{e}", file.display()));
                    }
                }
            }
        }
        fs::remove_dir(&dir).map_err(|e| format!("服务已移除，目录保留：{e}"))?;
    }
    Ok("自动应用已关闭并卸载；原程序旁的 logs 目录保留。不会撤销适配器当前的运行时设置。".into())
}

pub fn read_log() -> Result<String> {
    logs::read(&log_directory()?)
}
fn log(run: &Run, text: &str) -> Result<()> {
    run.logs.write(text)
}

fn attempt_once(
    dir: &Run,
    attempt: impl FnOnce() -> Result<Option<backend::Report>>,
) -> Result<bool> {
    // An existing marker is never overwritten, including after a process crash.
    let marker = dir.state_dir.join(PAUSED);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .map_err(|e| format!("无法建立执行保护标记；自动应用可能已暂停：{e}"))?;
    file.write_all("操作进行中；异常中断后需人工复查。\n".as_bytes())
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let Some(report) = attempt()? else {
        fs::remove_file(marker).map_err(|e| e.to_string())?;
        return Ok(false); // Lock contention only; no driver operation to retry.
    };
    for line in &report.logs {
        log(dir, line)?;
    }
    log(
        dir,
        &format!(
            "执行结果：{:?}；uncertain={}",
            report.status, report.uncertain
        ),
    )?;
    if report.uncertain {
        return Err("操作结果不确定，自动应用暂停；没有重发".into());
    }
    fs::remove_file(marker).map_err(|e| e.to_string())?;
    report
        .status
        .map_err(|e| format!("本次失败，不重发：{e}"))?;
    Ok(true)
}

#[derive(Default)]
struct Control {
    stop: bool,
    pending: bool,
    finishing: bool,
}
impl Control {
    fn trigger(&mut self) -> u32 {
        if self.finishing || self.stop {
            ERROR_SHUTDOWN_IN_PROGRESS
        } else {
            self.pending = true;
            NO_ERROR
        }
    }
    fn finish_unless_pending(&mut self) -> bool {
        if self.pending && !self.stop {
            self.pending = false;
            false
        } else {
            self.finishing = true;
            true
        }
    }
}
static CONTROL: Mutex<Control> = Mutex::new(Control {
    stop: false,
    pending: false,
    finishing: false,
});

unsafe extern "system" fn handler(code: u32, _: u32, _: *mut c_void, _: *mut c_void) -> u32 {
    let Ok(mut control) = CONTROL.lock() else {
        return ERROR_SHUTDOWN_IN_PROGRESS;
    };
    match code {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            control.stop = true;
            NO_ERROR
        }
        SERVICE_CONTROL_TRIGGEREVENT => control.trigger(),
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}
fn stopping() -> bool {
    CONTROL.lock().map_or(true, |s| s.stop)
}
fn delay(duration: Duration) -> bool {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        if stopping() {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
    !stopping()
}

fn apply_batch(dir: &Run) -> Result<()> {
    if dir.state_dir.join(PAUSED).exists() {
        return Err(
            "自动应用已暂停：上次操作结果不确定或被中断。请查看日志，手动复查后卸载并重新开启。"
                .into(),
        );
    }
    let deadline = Instant::now() + READY_TIMEOUT;
    let adapters = loop {
        // Bounded readiness retries only. Never retry a submitted driver operation here.
        if !delay(Duration::from_secs(2)) {
            return Ok(());
        }
        match backend::enumerate() {
            Ok(items) if !items.is_empty() => break items,
            Ok(_) => {}
            Err(e) => log(dir, &format!("等待无线接口：{e}"))?,
        }
        if Instant::now() >= deadline {
            return Err("等待适配器就绪超时；未发送设置".into());
        }
    };
    for adapter in adapters {
        if stopping() {
            break;
        }
        if !adapter.supported() {
            log(
                dir,
                &format!("跳过 {}：{:?}", adapter.id, adapter.compatibility),
            )?;
            continue;
        }
        log(dir, &format!("自动检查 {} / {}", adapter.id, adapter.pnp))?;
        while !stopping() {
            if attempt_once(dir, || backend::try_auto_operate(&adapter))? {
                break;
            }
            log(dir, "另一实例正在操作；仅等待互斥锁，尚未访问设备")?;
            if Instant::now() >= deadline {
                return Err("等待互斥锁超时；未发送设置".into());
            }
            if !delay(Duration::from_secs(2)) {
                break;
            }
        }
    }
    Ok(())
}

fn report_status(handle: SERVICE_STATUS_HANDLE, state: u32, exit: u32) -> Result<()> {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_TRIGGEREVENT
        } else {
            0
        },
        dwWin32ExitCode: exit,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    check(unsafe { SetServiceStatus(handle, &status) }, "报告服务状态")
}

unsafe extern "system" fn service_main(_: u32, _: *mut windows_sys::core::PWSTR) {
    let handle = unsafe { RegisterServiceCtrlHandlerExW(NAME, Some(handler), null()) };
    if handle.is_null() {
        return;
    }
    if report_status(handle, SERVICE_RUNNING, NO_ERROR).is_err() {
        return;
    }
    let result = std::panic::catch_unwind(|| -> Result<()> {
        let dir = Run {
            state_dir: directory()?,
            logs: logs::Session::new(&configured_log_directory()?)?,
        };
        log(&dir, "设备触发服务启动")?;
        let mut failed = false;
        loop {
            if let Err(e) = apply_batch(&dir) {
                failed = true;
                log(&dir, &e)?;
            }
            // Serialize the last pending-event check with the SCM callback. Events
            // after this point return SHUTDOWN_IN_PROGRESS and are requeued by SCM.
            if CONTROL
                .lock()
                .map_or(true, |mut s| s.finish_unless_pending())
            {
                break;
            }
            log(&dir, "收到新的设备到达通知，再次检查状态")?;
        }
        log(
            &dir,
            if stopping() {
                "收到停止请求，服务退出"
            } else {
                "本次处理结束，服务退出"
            },
        )?;
        if failed {
            Err("本次存在失败，详情见执行日志".into())
        } else {
            Ok(())
        }
    });
    // Even an early error must close the race with a new trigger before reporting stopped.
    if let Ok(mut control) = CONTROL.lock() {
        control.finishing = true;
    }
    let _ = report_status(
        handle,
        SERVICE_STOPPED,
        if matches!(result, Ok(Ok(()))) {
            NO_ERROR
        } else {
            ERROR_GEN_FAILURE
        },
    );
}

pub fn dispatch() -> Result<()> {
    // This entry point does nothing unless launched by the Service Control Manager.
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: NAME as *mut u16,
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: null_mut(),
            lpServiceProc: None,
        },
    ];
    check(
        unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) },
        "连接服务管理器",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_copy_mismatch_is_detected() {
        assert!(!different_binary(b"same", b"same"));
        assert!(different_binary(b"old", b"new"));
    }
    #[test]
    fn missing_service_copy_requires_repair_but_access_errors_are_not_hidden() {
        assert_eq!(copy_state(Ok(b"same".to_vec()), b"same").unwrap(), (false, false));
        assert_eq!(copy_state(Ok(b"old".to_vec()), b"new").unwrap(), (true, false));
        assert_eq!(copy_state(Err(std::io::Error::from(std::io::ErrorKind::NotFound)), b"new").unwrap(), (false, true));
        assert!(copy_state(Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)), b"new").is_err());
    }
    #[test]
    fn access_errors_explain_how_to_restart_as_administrator() {
        for code in [ERROR_ACCESS_DENIED, ERROR_PRIVILEGE_NOT_HELD] {
            let text = describe_error(
                "访问服务管理器",
                std::io::Error::from_raw_os_error(code as i32),
            );
            assert!(text.starts_with("权限不足"));
            assert!(text.contains("以管理员身份运行"));
        }
        assert!(
            !describe_error("启动服务", std::io::Error::from_raw_os_error(1060))
                .contains("权限不足")
        );
    }
    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!(
                    "steam-frame-auto-test-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            for name in [LOG, "auto-apply.previous.log", PAUSED] {
                let _ = fs::remove_file(self.0.join(name));
            }
            if let Ok(entries) = fs::read_dir(self.0.join("logs")) {
                for entry in entries.flatten() {
                    let _ = fs::remove_file(entry.path());
                }
                let _ = fs::remove_dir(self.0.join("logs"));
            }
            let _ = fs::remove_dir(&self.0);
        }
    }
    #[test]
    fn only_an_empty_orphan_install_directory_can_be_cleared() {
        let parent = TestDir::new();
        let install = parent.0.join("install");
        assert!(clear_empty_install_directory(&install).is_ok());
        fs::create_dir(&install).unwrap();
        fs::write(install.join("unknown.txt"), "keep").unwrap();
        assert!(clear_empty_install_directory(&install).is_err());
        assert_eq!(fs::read(install.join("unknown.txt")).unwrap(), b"keep");
        fs::remove_file(install.join("unknown.txt")).unwrap();
        clear_empty_install_directory(&install).unwrap();
        assert!(!install.exists());
    }
    fn report(uncertain: bool) -> backend::Report {
        backend::Report {
            status: if uncertain {
                Err("模拟不确定响应".into())
            } else {
                Ok(crate::protocol::Status {
                    country: "US".into(),
                    info: "6G Support (domain:05), due to REGU_RSN_MANUAL".into(),
                })
            },
            logs: vec!["模拟请求和复查；未访问硬件".into()],
            uncertain,
        }
    }
    #[test]
    fn automatic_execution_logs_and_never_retries_uncertain_or_interrupted_operations() {
        let dir = TestDir::new();
        let run = Run {
            state_dir: dir.0.clone(),
            logs: logs::Session::new(&dir.0.join("logs")).unwrap(),
        };
        assert!(!attempt_once(&run, || Ok(None)).unwrap()); // Busy without device access.
        assert!(!dir.0.join(PAUSED).exists());
        assert!(attempt_once(&run, || Ok(Some(report(false)))).unwrap());
        assert!(!dir.0.join(PAUSED).exists());
        let text = logs::read(&run.logs.dir).unwrap();
        assert!(text.contains("模拟请求和复查"));
        assert!(text.contains("REGU_RSN_MANUAL"));
        assert!(text.contains("uncertain=false"));

        assert!(attempt_once(&run, || Ok(Some(report(true)))).is_err());
        assert!(dir.0.join(PAUSED).exists());
        assert!(attempt_once(&run, || panic!("must not retry an uncertain operation")).is_err());
        fs::remove_file(dir.0.join(PAUSED)).unwrap();
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            attempt_once(&run, || panic!("simulate interrupted operation"))
        }));
        assert!(interrupted.is_err());
        assert!(dir.0.join(PAUSED).exists());
        assert!(attempt_once(&run, || panic!("must not retry an interrupted operation")).is_err());
    }
    #[test]
    fn automatic_log_rotates_and_keeps_latest_error() {
        let dir = TestDir::new();
        let run = Run {
            state_dir: dir.0.clone(),
            logs: logs::Session::new(&dir.0.join("logs")).unwrap(),
        };
        log(&run, &"x".repeat(1_048_576)).unwrap();
        log(&run, "错误：模拟失败，无密码").unwrap();
        assert_eq!(fs::read_dir(&run.logs.dir).unwrap().count(), 2);
        let current = logs::read(&run.logs.dir).unwrap();
        assert!(current.contains("错误：模拟失败"));
        assert!(current.contains("Z]"));
        // Only our logs are pruned; an unrelated file is retained.
        fs::write(run.logs.dir.join("notes.txt"), "keep").unwrap();
        for _ in 0..25 {
            logs::Session::new(&run.logs.dir)
                .unwrap()
                .write("next run")
                .unwrap();
        }
        assert!(run.logs.dir.join("notes.txt").exists());
        assert!(fs::read_dir(&run.logs.dir).unwrap().count() <= 23); // Includes a still-open run and notes.
    }
    #[test]
    fn log_reader_rejects_hard_links_and_active_paths_cannot_be_replaced() {
        let dir = TestDir::new();
        let session = logs::Session::new(&dir.0.join("logs")).unwrap();
        session.write("keep").unwrap();
        assert!(fs::rename(&session.dir, dir.0.join("moved")).is_err());
        fs::write(dir.0.join(PAUSED), "unrelated data").unwrap();
        fs::hard_link(
            dir.0.join(PAUSED),
            session.dir.join("auto-20000101T000000Z-1-000.log"),
        )
        .unwrap();
        assert!(logs::read(&session.dir).is_err());
        assert_eq!(
            fs::read_to_string(dir.0.join(PAUSED)).unwrap(),
            "unrelated data"
        );
    }
    #[test]
    fn triggers_are_coalesced_and_shutdown_events_are_requeued() {
        let mut control = Control::default();
        assert_eq!(control.trigger(), NO_ERROR);
        assert_eq!(control.trigger(), NO_ERROR);
        assert!(!control.finish_unless_pending());
        assert!(control.finish_unless_pending());
        assert_eq!(control.trigger(), ERROR_SHUTDOWN_IN_PROGRESS);
        let mut stopped = Control {
            stop: true,
            ..Default::default()
        };
        assert_eq!(stopped.trigger(), ERROR_SHUTDOWN_IN_PROGRESS);
        assert!(stopped.finish_unless_pending());
    }
    #[test]
    fn service_command_is_quoted_and_uses_only_service_mode() {
        assert_eq!(
            command(Path::new(r"C:\Program Files\Steam Frame 6 GHz Tool")),
            "\"C:\\Program Files\\Steam Frame 6 GHz Tool\\steam-frame-6ghz-tool.exe\" --service"
        );
        assert_eq!(wide("USB\\VID_28DE&PID_2432").last(), Some(&0));
    }
}
