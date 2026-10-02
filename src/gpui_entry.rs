#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod frame;
mod gpui_frame;
mod gpui_log;
mod gpui_state;
mod i18n;
mod logs;
mod protocol;
mod service;
mod settings;
mod ui_log;
mod updater;

use i18n::t;
use std::{os::windows::ffi::OsStrExt, ptr::{null, null_mut}, thread, time::{Duration, Instant}};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, ERROR_CANCELLED, GetLastError, HANDLE},
    System::Threading::CreateMutexW,
    UI::{Shell::{IsUserAnAdmin, ShellExecuteW},
        WindowsAndMessaging::{FindWindowW, FlashWindowEx, IsIconic, SetForegroundWindow, ShowWindow,
            FLASHWINFO, FLASHW_TRAY, SW_RESTORE, SW_SHOWNORMAL}},
};

const WINDOW_TITLE: &str = "Steam Frame 6GHz Tool";
const GUI_MUTEX: &str = "Local\\SteamFrame6GHzToolGuiV1";

struct GuiInstance(HANDLE);
impl GuiInstance {
    fn enter(name: &str) -> Result<Option<Self>, String> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if handle.is_null() { return Err(format!("CreateMutexW: {}", unsafe { GetLastError() })); }
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if exists {
            unsafe { CloseHandle(handle); }
            Ok(None)
        } else {
            Ok(Some(Self(handle)))
        }
    }
}
impl Drop for GuiInstance {
    fn drop(&mut self) { unsafe { CloseHandle(self.0); } }
}

fn activate_existing(wait: bool) -> bool {
    let title: Vec<u16> = WINDOW_TITLE.encode_utf16().chain(Some(0)).collect();
    let deadline = Instant::now() + if wait { Duration::from_secs(5) } else { Duration::ZERO };
    loop {
        let window = unsafe { FindWindowW(null(), title.as_ptr()) };
        if !window.is_null() {
            unsafe {
                if IsIconic(window) != 0 { ShowWindow(window, SW_RESTORE); }
                if SetForegroundWindow(window) == 0 {
                    let flash = FLASHWINFO { cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                        hwnd: window, dwFlags: FLASHW_TRAY, uCount: 3, dwTimeout: 0 };
                    FlashWindowEx(&flash);
                }
            }
            return true;
        }
        if Instant::now() >= deadline { return false; }
        thread::sleep(Duration::from_millis(100));
    }
}

fn request_administrator() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let path: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let result = unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), path.as_ptr(), null(), null(), SW_SHOWNORMAL) };
    if result as isize > 32 { return Ok(()); }
    let error = unsafe { GetLastError() };
    if error == ERROR_CANCELLED { return Ok(()); }
    Err(format!("ShellExecuteW: {error}"))
}

fn main() {
    i18n::initialize();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--service"] {
        if service::dispatch().is_err() { std::process::exit(1); }
        return;
    }
    if args == ["--list"] {
        match backend::enumerate() {
            Ok(items) => for adapter in items {
                println!("{} | {} | {} | {:?}", adapter.id, adapter.name, adapter.pnp, adapter.compatibility);
            },
            Err(error) => { eprintln!("{error}"); std::process::exit(1); }
        }
        return;
    }
    if !args.is_empty() && args != ["--demo"] {
        rfd::MessageDialog::new().set_title(t("参数错误", "Invalid arguments"))
            .set_description(t("支持无参数启动、--demo 演示、--list 只读枚举；--service 仅由 Windows 服务管理器调用。", "Start without arguments, or use --demo / --list. --service is reserved for Windows Service Manager."))
            .show();
        return;
    }
    if unsafe { IsUserAnAdmin() } == 0 {
        if activate_existing(false) { return; }
        if let Err(error) = request_administrator() {
            rfd::MessageDialog::new().set_title(WINDOW_TITLE)
                .set_description(format!("{}: {error}", t("无法申请管理员权限", "Could not request administrator access")))
                .show();
        }
        return;
    }
    let _instance = match GuiInstance::enter(GUI_MUTEX) {
        Ok(Some(instance)) => instance,
        Ok(None) => {
            if !activate_existing(true) {
                rfd::MessageDialog::new().set_title(WINDOW_TITLE)
                    .set_description(t("程序已在运行，但未找到它的窗口。", "The app is already running, but its window could not be found."))
                    .show();
            }
            return;
        }
        Err(error) => {
            rfd::MessageDialog::new().set_title(WINDOW_TITLE)
                .set_description(format!("{}: {error}", t("无法检查程序是否已在运行", "Could not check whether the app is already running")))
                .show();
            return;
        }
    };
    gpui_state::open(args == ["--demo"]);
}

#[cfg(test)]
mod startup_tests {
    use super::GuiInstance;

    #[test]
    fn only_one_gui_instance_can_hold_the_name() {
        let name = format!("Local\\SteamFrameGuiTest{}", std::process::id());
        let first = GuiInstance::enter(&name).unwrap().expect("first instance");
        assert!(GuiInstance::enter(&name).unwrap().is_none());
        drop(first);
        assert!(GuiInstance::enter(&name).unwrap().is_some());
    }
}
