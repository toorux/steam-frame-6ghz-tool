use crate::{backend::{self, Adapter, Report}, i18n::{self, t}, logs, protocol, service, ui_log::{self, Filter, Level}, updater};
use std::{fs, path::PathBuf, sync::mpsc::{self, Receiver}, thread, time::{Duration, Instant}};

enum Event {
    Devices(
        protocol::Result<Vec<Adapter>>,
        protocol::Result<service::State>,
    ),
    Operation(Report),
    AutoApply(protocol::Result<String>, protocol::Result<service::State>),
}
struct ServiceSnapshot {
    epoch: u64,
    state: protocol::Result<service::State>,
    logs: protocol::Result<(u64, Option<String>, PathBuf)>,
}
struct App {
    adapters: Vec<Adapter>,
    selected: Option<usize>,
    receiver: Option<Receiver<Event>>,
    refreshing: bool,
    device_status: Option<protocol::Status>,
    status: String,
    status_level: Level,
    log: String,
    program_log: Option<logs::Session>,
    service_log: String,
    rows: Vec<ui_log::Row>,
    visible_rows: Vec<usize>,
    logs_dirty: bool,
    filter: Filter,
    errors_only: bool,
    confirm: Option<Adapter>,
    confirm_auto: Option<bool>,
    confirm_service_update: bool,
    confirm_update: bool,
    error_popup: Option<String>,
    uncertain: bool,
    demo: bool,
    auto_state: Option<service::State>,
    auto_error: Option<String>,
    log_error: Option<String>,
    service_receiver: Option<Receiver<ServiceSnapshot>>,
    service_revision: Option<u64>,
    next_service_refresh: Instant,
    service_epoch: u64,
    log_dir: Option<PathBuf>,
    export_receiver: Option<Receiver<Result<Option<PathBuf>, String>>>,
    update_receiver: Option<Receiver<Result<Option<updater::Release>, String>>>,
    available_update: Option<updater::Release>,
    manual_update: bool,
    update_status: String,
    settings: crate::settings::Settings,
}
impl App {
    fn empty(demo: bool) -> Self {
        Self {
            adapters: vec![],
            selected: None,
            receiver: None,
            refreshing: false,
            device_status: None,
            status: t("请选择适配器。", "Select an adapter.").into(),
            status_level: Level::Info,
            log: String::new(),
            program_log: None,
            service_log: String::new(),
            rows: vec![],
            visible_rows: vec![],
            logs_dirty: true,
            filter: Filter::All,
            errors_only: false,
            confirm: None,
            confirm_auto: None,
            confirm_service_update: false,
            confirm_update: false,
            error_popup: None,
            uncertain: false,
            demo,
            auto_state: None,
            auto_error: None,
            log_error: None,
            service_receiver: None,
            service_revision: None,
            next_service_refresh: Instant::now(),
            service_epoch: 0,
            log_dir: None,
            export_receiver: None,
            update_receiver: None,
            available_update: None,
            manual_update: false,
            update_status: String::new(),
            settings: Default::default(),
        }
    }
    fn new(demo: bool) -> Self {
        let mut app = Self::empty(demo);
        match std::env::current_exe()
            .map_err(|e| e.to_string())
            .and_then(|exe| logs::beside(&exe))
            .and_then(|dir| logs::Session::new_program(&dir))
        {
            Ok(session) => app.program_log = Some(session),
            Err(error) => app.error_popup = Some(format!("{}: {error}",
                t("无法创建程序日志", "Could not create program log"))),
        }
        app.record(&format!(
            "{} {}",
            t("Steam Frame 6 GHz 设置工具", "Steam Frame 6 GHz Tool"),
            env!("CARGO_PKG_VERSION")
        ));
        if demo {
            app.service_log = format!(
                "[{}] 设备触发服务启动（演示）\n[{}] 已是 US / MANUAL，未重复设置。\n[{}] [ERROR] 演示错误：设备已移除，未发送设置。\n",
                backend::timestamp(),
                backend::timestamp(),
                backend::timestamp()
            );
        }
        app.refresh();
        if !demo {
            app.check_updates(false);
        }
        app
    }
    fn record(&mut self, text: &str) {
        if let Some(session) = &self.program_log
            && let Err(error) = session.write(text) {
            self.error_popup = Some(format!("{}: {error}",
                t("无法写入程序日志", "Could not write program log")));
        }
        self.log
            .push_str(&format!("[{}] {text}\n", backend::timestamp()));
        self.logs_dirty = true;
    }
    fn failure(&mut self, summary: &str, detail: String, popup: bool) {
        self.status = summary.into();
        self.status_level = Level::Error;
        self.record(&format!("[ERROR] {detail}"));
        if popup {
            self.error_popup = Some(detail);
        }
    }
    fn busy_status(&mut self, message: &str) {
        self.status = message.into();
        self.status_level = Level::Info;
    }
    fn refresh(&mut self) {
        if self.receiver.is_some() {
            return;
        }
        self.selected = None;
        self.adapters.clear();
        self.device_status = None;
        self.confirm = None;
        self.busy_status(t(
            "正在查找 Steam Frame 适配器…",
            "Searching for Steam Frame adapters…",
        ));
        self.next_service_refresh = Instant::now();
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.refreshing = true;
        let demo = self.demo;
        let demo_state = self.auto_state.unwrap_or_default();
        thread::spawn(move || {
            let devices = if demo {
                Ok(vec![demo_adapter()])
            } else {
                backend::enumerate()
            };
            let auto = if demo {
                Ok(demo_state)
            } else {
                service::state()
            };
            let _ = tx.send(Event::Devices(devices, auto));
        });
    }
    fn reset_service_snapshot(&mut self) {
        self.service_epoch += 1;
        self.service_revision = None;
        self.next_service_refresh = Instant::now();
    }
    fn set_auto(&mut self, install: bool) {
        if self.receiver.is_some() {
            return;
        }
        self.confirm_auto = None;
        self.reset_service_snapshot();
        self.busy_status(if install {
            t(
                "正在安装自动应用服务…",
                "Installing the auto-apply service…",
            )
        } else {
            t(
                "正在停止并卸载服务，请稍候…",
                "Stopping and removing the service…",
            )
        });
        self.record(&self.status.clone());
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        let demo = self.demo;
        thread::spawn(move || {
            if demo {
                let _ = tx.send(Event::AutoApply(
                    Ok(t("演示模式：未修改系统。", "Demo mode: no system changes.").into()),
                    Ok(service::State {
                        installed: install,
                        enabled: install,
                        ..Default::default()
                    }),
                ));
                return;
            }
            let result = if install {
                service::install()
            } else {
                service::uninstall()
            };
            let _ = tx.send(Event::AutoApply(result, service::state()));
        });
    }
    fn update_service(&mut self) {
        if self.receiver.is_some() {
            return;
        }
        self.confirm_service_update = false;
        self.busy_status(t(
            "正在更新自动应用服务…",
            "Updating the auto-apply service…",
        ));
        self.record("用户确认更新自动应用服务副本");
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(Event::AutoApply(service::reinstall(), service::state()));
        });
    }
    fn check_updates(&mut self, manual: bool) {
        if self.update_receiver.is_some() { return; }
        self.manual_update = manual;
        self.update_status = t("正在检查更新…", "Checking for updates…").into();
        let (tx, rx) = mpsc::channel();
        self.update_receiver = Some(rx);
        let demo = self.demo;
        thread::spawn(move || { let _ = tx.send(if demo { Ok(None) } else { updater::check() }); });
    }
    fn finish_update(&mut self, result: Result<Option<updater::Release>, String>) {
        let notice = match result {
            Ok(release) => {
                self.update_status = release.as_ref().map(|r| format!("{} {}", t("发现新版本", "New version available:"), r.version))
                    .unwrap_or_else(|| t("当前已是最新版本。", "You're up to date.").into());
                self.available_update = release;
                Ok(self.update_status.clone())
            }
            Err(error) => {
                self.update_status = format!("{}\n{error}", t("检查更新失败，请稍后重试。", "Could not check for updates. Please try again later."));
                Err(self.update_status.clone())
            }
        };
        self.record(&format!("{}{}", if notice.is_err() { "[WARN] " } else { "" }, self.update_status));
        if self.manual_update { self.settings.notice = Some(notice); }
        self.manual_update = false;
    }
    fn poll_update(&mut self) {
        match self.update_receiver.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(result)) => {
                self.update_receiver = None;
                self.finish_update(result);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.update_receiver = None;
                self.finish_update(Err(t("检查更新线程意外结束。", "The update check stopped unexpectedly.").into()));
            }
            _ => {}
        }
    }
    fn update_auto_state(&mut self, result: protocol::Result<service::State>) {
        match result {
            Ok(state) => {
                self.auto_state = Some(state);
                self.auto_error = None;
            }
            Err(e) => {
                if self.auto_error.as_ref() != Some(&e) {
                    self.record(&format!("[ERROR] 读取服务状态失败：{e}"));
                }
                self.auto_state = None;
                self.auto_error = Some(e);
            }
        }
    }
    fn start(&mut self, adapter: Adapter, set_us: bool) {
        if self.receiver.is_some() {
            return;
        }
        self.confirm = None;
        self.record(&format!(
            "{}：{} [{}]",
            if set_us {
                t("用户确认设置 US", "User confirmed setting US")
            } else {
                t("查询状态", "Query status")
            },
            adapter.name,
            adapter.id
        ));
        self.busy_status(if set_us {
            t("正在设置 US 并复查…", "Setting US and verifying…")
        } else {
            t("正在读取设备状态…", "Reading adapter status…")
        });
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        let demo = self.demo;
        thread::spawn(move || {
            let result = if demo {
                Report {
                status: Ok(protocol::Status { country: if set_us { "US" } else { "CN" }.into(),
                    info: if set_us { "[6G Info]\n6G Support (domain:05), due to REGU_RSN_MANUAL" }
                    else { "[6G Info]\n6G NOT Support\n[Hint] domain (00), country (CN), wwsku rsn REGU_RSN_11D" }.into() }),
                logs: vec!["演示数据：未调用设备 API。".into()], uncertain: false,
            }
            } else {
                backend::operate(&adapter, set_us)
            };
            let _ = tx.send(Event::Operation(result));
        });
    }
    fn select(&mut self, selected: Option<usize>) {
        if self.receiver.is_some() || self.selected == selected {
            return;
        }
        self.selected = selected;
        self.device_status = None;
        self.confirm = None;
        self.busy_status(t("请选择适配器。", "Select an adapter."));
        if let Some(adapter) = selected.and_then(|i| self.adapters.get(i)).cloned() {
            if adapter.supported() {
                self.start(adapter, false);
            } else {
                self.failure(
                    t(
                        "驱动不受支持，无法读取。",
                        "Unsupported driver; unable to read status.",
                    ),
                    adapter.compatibility.unwrap_err(),
                    false,
                );
            }
        }
    }
    fn save(&mut self) {
        if self.export_receiver.is_some() { return; }
        let text = format!(
                "\u{feff}{}\n{}\n{}\n{}\n{}",
                t(
                    "Steam Frame 6 GHz 日志（UTC）",
                    "Steam Frame 6 GHz Log (UTC)"
                ),
                t("程序日志", "Program log"),
                self.log,
                t("服务日志", "Service log"),
                self.service_log
            );
        let dir = self.log_dir.clone();
        let filter = t("日志", "Log").to_owned();
        let (tx, rx) = mpsc::channel();
        self.export_receiver = Some(rx);
        thread::spawn(move || {
            let result = std::panic::catch_unwind(|| {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter(filter.as_str(), &["txt"])
                    .set_file_name("steam-frame-log.txt");
                if let Some(dir) = dir { dialog = dialog.set_directory(dir); }
                if let Some(path) = dialog.save_file() {
                    fs::write(&path, text).map_err(|e| e.to_string())?;
                    Ok(Some(path))
                } else { Ok(None) }
            }).unwrap_or_else(|_| Err("保存对话框意外终止".into()));
            let _ = tx.send(result);
        });
    }
    fn poll_export(&mut self) {
        match self.export_receiver.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(Ok(Some(path)))) => {
                self.export_receiver = None;
                self.record(&format!("已导出全部来源日志：{}", path.display()));
            }
            Some(Ok(Ok(None))) => { self.export_receiver = None; }
            Some(Ok(Err(error))) => {
                self.export_receiver = None;
                self.failure(t("日志导出失败。", "Could not export the log."), error, true);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.export_receiver = None;
                self.failure(t("日志导出失败。", "Could not export the log."),
                    "保存对话框线程意外结束".into(), true);
            }
            _ => {}
        }
    }
    fn poll(&mut self) {
        match self.receiver.as_ref().map(|r| r.try_recv()) {
            Some(Ok(event)) => {
                self.receiver = None;
                match event {
                    Event::Devices(result, auto) => {
                        self.refreshing = false;
                        self.update_auto_state(auto);
                        match result {
                            Ok(items) => {
                                self.busy_status(if items.is_empty() {
                                    t(
                                        "未发现适配器。请插入设备，然后刷新。",
                                        "No adapter found. Plug it in and refresh.",
                                    )
                                } else {
                                    t(
                                        "请选择适配器，选中后自动读取状态。",
                                        "Select an adapter to read its status.",
                                    )
                                });
                                for a in &items {
                                    self.record(&format!(
                                        "发现 {} [{}] / {} / {:?}",
                                        a.name, a.id, a.pnp, a.compatibility
                                    ));
                                }
                                self.adapters = items;
                                if self.adapters.len() == 1 {
                                    self.select(Some(0));
                                }
                            }
                            Err(e) => self.failure(
                                t(
                                    "无法读取适配器，请查看日志。",
                                    "Could not read the adapter. See the log.",
                                ),
                                e,
                                false,
                            ),
                        }
                    }
                    Event::AutoApply(result, state) => {
                        self.reset_service_snapshot();
                        self.update_auto_state(state);
                        match result {
                            Ok(message) => {
                                self.record(&message);
                                self.busy_status(if self.auto_state.is_some_and(|s| s.installed) {
                                    t(
                                        "自动应用已安装，执行结果见下方服务日志。",
                                        "Auto-apply is installed. See the service log below.",
                                    )
                                } else {
                                    t(
                                        "自动应用已关闭，日志已保留。",
                                        "Auto-apply is off. Logs were kept.",
                                    )
                                });
                            }
                            Err(e) => {
                                let summary = if e.starts_with("权限不足") {
                                    t(
                                        "权限不足，请以管理员身份重新运行。",
                                        "Access denied. Run as administrator.",
                                    )
                                } else {
                                    t("自动应用配置失败。", "Auto-apply configuration failed.")
                                };
                                self.failure(summary, e, true);
                            }
                        }
                    }
                    Event::Operation(report) => {
                        for line in report.logs {
                            self.record(&line);
                        }
                        self.uncertain = report.uncertain;
                        match report.status {
                            Ok(status) => {
                                self.record(&format!(
                                    "{}\n{}",
                                    status_summary(&status),
                                    status.info
                                ));
                                self.device_status = Some(status);
                                self.busy_status(t("设备状态已更新。", "Adapter status updated."));
                            }
                            Err(e) => {
                                self.device_status = None;
                                self.failure(
                                    t(
                                        "设备操作未成功，请查看日志。",
                                        "Adapter operation failed. See the log.",
                                    ),
                                    e,
                                    false,
                                );
                            }
                        }
                    }
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.receiver = None;
                self.refreshing = false;
                self.uncertain = true;
                self.failure(
                    t(
                        "操作中断，设备状态未知。",
                        "Operation interrupted; adapter status unknown.",
                    ),
                    "工作线程意外终止；不自动重试。".into(),
                    true,
                );
            }
            _ => {}
        }
    }
    fn apply_snapshot(&mut self, snapshot: ServiceSnapshot) {
        if snapshot.epoch != self.service_epoch {
            return;
        }
        let finished = self.auto_state.is_some_and(|s| s.running)
            && snapshot.state.as_ref().is_ok_and(|s| !s.running);
        self.update_auto_state(snapshot.state);
        match snapshot.logs {
            Ok((revision, text, dir)) => {
                self.service_revision = Some(revision);
                self.log_dir = Some(dir);
                if let Some(text) = text {
                    self.service_log = text;
                    self.logs_dirty = true;
                }
                self.log_error = None;
            }
            Err(e) => {
                if self.log_error.as_ref() != Some(&e) {
                    self.record(&format!("[ERROR] 读取服务日志失败：{e}"));
                }
                self.log_error = Some(e); // Keep the last good log snapshot visible.
            }
        }
        if finished
            && self.receiver.is_none()
            && !self.confirming()
            && let Some(adapter) = self.selected.and_then(|i| self.adapters.get(i)).cloned()
            && adapter.supported()
        {
            self.start(adapter, false);
        }
    }
    fn poll_service(&mut self) {
        if self.demo {
            return;
        }
        match self.service_receiver.as_ref().map(|r| r.try_recv()) {
            Some(Ok(snapshot)) => {
                self.service_receiver = None;
                self.apply_snapshot(snapshot);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.service_receiver = None;
                let message = "服务日志读取线程意外终止".to_string();
                if self.log_error.as_ref() != Some(&message) {
                    self.record(&format!("[ERROR] {message}"));
                }
                self.log_error = Some(message);
            }
            _ => {}
        }
        if self.service_receiver.is_none() && Instant::now() >= self.next_service_refresh {
            self.next_service_refresh = Instant::now() + Duration::from_secs(2);
            let (tx, rx) = mpsc::channel();
            self.service_receiver = Some(rx);
            let previous = self.service_revision;
            let epoch = self.service_epoch;
            thread::spawn(move || {
                let state = service::state();
                let logs = (|| {
                    let dir = service::log_directory()?;
                    let revision = logs::revision(&dir)?;
                    let text = if previous == Some(revision) {
                        None
                    } else {
                        Some(service::read_log()?)
                    };
                    Ok((revision, text, dir))
                })();
                let _ = tx.send(ServiceSnapshot { epoch, state, logs });
            });
        }
    }
    fn confirming(&self) -> bool {
        self.confirm.is_some()
            || self.confirm_auto.is_some()
            || self.confirm_service_update
            || self.confirm_update
            || self.error_popup.is_some()
    }
    fn rebuild_logs(&mut self) {
        if !self.logs_dirty {
            return;
        }
        self.rows = ui_log::merge(&self.log, &self.service_log);
        self.visible_rows = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                self.filter.matches(row.source) && (!self.errors_only || row.level == Level::Error)
            })
            .map(|(i, _)| i)
            .collect();
        self.logs_dirty = false;
    }
}

fn adapter_label(adapter: &Adapter, all: &[Adapter]) -> String {
    let name = if !adapter.name.contains("模拟") {
        t("Steam Frame USB 适配器", "Steam Frame USB adapter")
    } else {
        t(
            "Steam Frame USB 适配器（演示）",
            "Steam Frame USB adapter (demo)",
        )
    };
    if all.len() > 1 {
        format!(
            "{name} · {}",
            &adapter.id[adapter.id.len().saturating_sub(8)..]
        )
    } else {
        name.into()
    }
}
fn six_status(status: &protocol::Status) -> &'static str {
    if status
        .info
        .lines()
        .any(|l| l.trim().starts_with("6G NOT Support"))
    {
        t("不可用", "Unavailable")
    } else if status
        .info
        .lines()
        .any(|l| l.trim().starts_with("6G Support (domain:"))
    {
        t("可用", "Available")
    } else {
        t("未知（见日志）", "Unknown (see log)")
    }
}
fn status_summary(status: &protocol::Status) -> String {
    if i18n::is_english() {
        format!(
            "Country: {}     6 GHz: {}",
            status.country,
            six_status(status)
        )
    } else {
        format!("国家：{}     6 GHz：{}", status.country, six_status(status))
    }
}
fn demo_adapter() -> Adapter {
    Adapter {
        guid: windows_sys::core::GUID::from_u128(0),
        id: "DEMO-ONLY-NOT-A-REAL-INTERFACE".into(),
        name: "Valve USB Adapter（模拟）".into(),
        pnp: "DEMO — 不访问硬件".into(),
        service: "模拟".into(),
        compatibility: Ok("演示设备；非真实兼容性检查".into()),
        unverified_driver: false,
    }
}

include!("gpui_main_view.rs");

#[cfg(test)]
mod state_tests {
    use super::{App, Dashboard, Event, MainModal, Page, demo_adapter};
    use crate::{service, ui_log};
    use crate::ui_log::Filter;
    use std::sync::mpsc;

    #[test]
    fn manual_updates_report_latest_failure_and_new_release() {
        let mut app = App::empty(true);
        app.manual_update = true;
        app.finish_update(Ok(None));
        assert!(app.settings.notice.as_ref().unwrap().is_ok());
        app.manual_update = true;
        app.finish_update(Err("offline".into()));
        assert!(app.settings.notice.as_ref().unwrap().is_err());
        app.manual_update = true;
        app.finish_update(Ok(Some(crate::updater::Release {
            version: "9.0.0".parse().unwrap(), page: "https://github.com/toorux/steam-frame-6ghz-tool/releases/tag/v9.0.0".into(),
        })));
        assert!(app.settings.notice.as_ref().unwrap().is_ok());
        assert_eq!(app.available_update.unwrap().version.to_string(), "9.0.0");
    }

    #[test]
    fn one_adapter_is_selected_and_queried() {
        let mut app = App::empty(true);
        let (tx, rx) = mpsc::channel();
        app.receiver = Some(rx);
        tx.send(Event::Devices(Ok(vec![demo_adapter()]), Ok(service::State::default()))).unwrap();
        app.poll();
        assert_eq!(app.selected, Some(0));
        assert!(app.receiver.is_some(), "selection should launch exactly one status query");
    }

    #[test]
    fn canceled_confirmation_does_not_issue_device_operation() {
        let mut app = App::empty(true);
        app.adapters.push(demo_adapter());
        app.selected = Some(0);
        let mut view = Dashboard { data: app, modal: Some(MainModal::SetUs), page: Page::Local, frame: None, adapter_menu: false, log_scroll: gpui_kit::ScrollHandle::new() };
        view.modal = None;
        assert!(view.data.receiver.is_none());
        assert!(view.data.device_status.is_none());
    }

    #[test]
    fn service_and_error_filters_do_not_hide_retained_logs() {
        let mut app = App::empty(true);
        app.log = "[2026-09-28T00:00:00Z] normal\n".into();
        app.service_log = "[2026-09-28T00:00:01Z] [ERROR] failed\n".into();
        app.filter = Filter::Service;
        app.errors_only = true;
        app.rebuild_logs();
        assert_eq!(app.visible_rows.len(), 1);
        assert_eq!(app.rows[app.visible_rows[0]].source, ui_log::Source::Service);
        assert!(app.log.contains("normal"));
    }
}
