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
mod ui_log;
mod updater;

use i18n::t;

fn main() {
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
    gpui_state::open(args == ["--demo"]);
}
