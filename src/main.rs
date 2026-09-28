#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
compile_error!("This application targets x86_64 Windows only.");

mod backend;
mod frame;
mod frame_ui;
mod i18n;
mod logs;
mod protocol;
mod service;
mod ui_log;
mod updater;
use backend::{Adapter, Report};
use base64::Engine as _;
use eframe::egui::{self, Color32, RichText};
use i18n::t;
use std::{
    fs,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};
use ui_log::{Filter, Level};

const INK: Color32 = Color32::from_rgb(30, 41, 59);
const MUTED: Color32 = Color32::from_rgb(100, 116, 139);
const BLUE: Color32 = Color32::from_rgb(37, 99, 235);
const RED: Color32 = Color32::from_rgb(185, 28, 28);
const GREEN: Color32 = Color32::from_rgb(21, 128, 61);
const AMBER: Color32 = Color32::from_rgb(161, 98, 7);
const BORDER: Color32 = Color32::from_rgb(226, 232, 240);

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
    device_status: Option<protocol::Status>,
    status: String,
    status_level: Level,
    log: String,
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
    frame_ui: Option<frame_ui::FrameUi>,
    github_mark: Option<egui::TextureHandle>,
    update_receiver: Option<Receiver<Result<Option<updater::Release>, String>>>,
    available_update: Option<updater::Release>,
}
impl App {
    fn empty(demo: bool) -> Self {
        Self {
            adapters: vec![],
            selected: None,
            receiver: None,
            device_status: None,
            status: t("请选择适配器。", "Select an adapter.").into(),
            status_level: Level::Info,
            log: String::new(),
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
            frame_ui: None,
            github_mark: None,
            update_receiver: None,
            available_update: None,
        }
    }
    fn new(cc: &eframe::CreationContext<'_>, demo: bool) -> Self {
        setup_style(&cc.egui_ctx);
        let mut app = Self::empty(demo);
        app.github_mark = Some(load_github_mark(&cc.egui_ctx));
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
            let (tx, rx) = mpsc::channel();
            app.update_receiver = Some(rx);
            thread::spawn(move || {
                let _ = tx.send(updater::check());
            });
        }
        app
    }
    fn record(&mut self, text: &str) {
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
    fn poll_update(&mut self) {
        match self.update_receiver.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(result)) => {
                self.update_receiver = None;
                match result {
                    Ok(release) => {
                        self.available_update = release;
                    }
                    Err(e) => self.record(&format!("[WARN] 检查更新失败，不影响本地功能：{e}")),
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.update_receiver = None;
                self.record("[WARN] 检查更新线程意外结束，不影响本地功能。");
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
        let mut dialog = rfd::FileDialog::new()
            .add_filter(t("日志", "Log"), &["txt"])
            .set_file_name("steam-frame-log.txt");
        if let Some(dir) = &self.log_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.save_file() {
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
            match fs::write(&path, text) {
                Ok(()) => self.record(&format!("已导出全部来源日志：{}", path.display())),
                Err(e) => self.failure(
                    t("日志导出失败。", "Could not export the log."),
                    e.to_string(),
                    true,
                ),
            }
        }
    }
    fn poll(&mut self) {
        match self.receiver.as_ref().map(|r| r.try_recv()) {
            Some(Ok(event)) => {
                self.receiver = None;
                match event {
                    Event::Devices(result, auto) => {
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
    fn device_card(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("适配器", "Adapter")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            self.receiver.is_none() && !self.confirming(),
                            egui::Button::new(t("刷新设备", "Refresh")),
                        )
                        .clicked()
                    {
                        self.refresh();
                    }
                });
            });
            ui.add_space(8.0);
            let mut choice = self.selected;
            ui.add_enabled_ui(self.receiver.is_none() && !self.confirming(), |ui| {
                egui::ComboBox::from_id_salt("adapter")
                    .width(ui.available_width())
                    .wrap_mode(egui::TextWrapMode::Truncate)
                    .selected_text(
                        self.selected
                            .and_then(|i| self.adapters.get(i))
                            .map(|a| adapter_label(a, &self.adapters))
                            .unwrap_or_else(|| {
                                if self.adapters.is_empty() {
                                    t("未发现 Steam Frame 适配器", "No Steam Frame adapter found").into()
                                } else {
                                    t("选择适配器", "Select an adapter").into()
                                }
                            }),
                    )
                    .show_ui(ui, |ui| {
                        for (i, a) in self.adapters.iter().enumerate() {
                            ui.selectable_value(
                                &mut choice,
                                Some(i),
                                adapter_label(a, &self.adapters),
                            );
                        }
                    });
            });
            self.select(choice);
            let selected = self.selected.and_then(|i| self.adapters.get(i)).cloned();
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("国家", "Country")).color(MUTED));
                pill(
                    ui,
                    self.device_status.as_ref().map_or("—", |s| &s.country),
                    INK,
                );
                ui.add_space(12.0);
                ui.label(RichText::new("6 GHz").color(MUTED));
                let six = self
                    .device_status
                    .as_ref()
                    .map(six_status)
                    .unwrap_or(t("未读取", "Not read"));
                pill(
                    ui,
                    six,
                    match six {
                        "可用" | "Available" => GREEN,
                        "不可用" | "Unavailable" => AMBER,
                        _ => MUTED,
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let allowed = selected.as_ref().is_some_and(Adapter::supported)
                        && self.device_status.is_some()
                        && self.receiver.is_none()
                        && !self.confirming()
                        && !self.uncertain;
                    if primary_button(ui, t("设置 US", "Set US"), allowed).clicked() {
                        self.confirm = selected.clone();
                    }
                });
            });
            if self
                .device_status
                .as_ref()
                .is_some_and(|s| !s.country_known())
            {
                ui.add_space(6.0);
                ui.colored_label(
                    AMBER,
                    t("原国家码未知（00 00）。确认后只发送一次并严格复查；自动应用也会按相同规则处理。", "Original country code is unknown (00 00). One setting request will be sent and verified; auto-apply follows the same rule."),
                );
            }
            if selected
                .as_ref()
                .is_some_and(|a| a.supported() && a.unverified_driver)
            {
                ui.add_space(6.0);
                ui.colored_label(AMBER, t("此驱动版本未验证，仍可操作。", "This driver version is unverified; operation is still allowed."));
            }
            if self.uncertain {
                ui.add_space(6.0);
                ui.colored_label(RED, t("结果不确定。请刷新复查，暂不重复设置。", "Result uncertain. Refresh to verify; do not repeat the setting yet."));
            }
        });
    }
    fn auto_card(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("自动应用", "Auto-apply")).strong());
                let (text, color) = match self.auto_state {
                    Some(s) if s.paused => (t("已暂停", "Paused"), RED),
                    Some(s) if s.running => (t("正在处理", "Running"), BLUE),
                    Some(s) if s.installed && !s.enabled => (t("已停用", "Disabled"), AMBER),
                    Some(s) if s.installed && s.exit_code != 0 => (t("上次执行失败", "Last run failed"), RED),
                    Some(s) if s.installed => (t("已开启", "Enabled"), GREEN),
                    Some(_) => (t("未开启", "Off"), MUTED),
                    None => (t("状态未知", "Unknown"), MUTED),
                };
                pill(ui, text, color);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(state) = self.auto_state {
                        if state.installed
                            && state.needs_update
                            && ui
                                .add_enabled(
                                    !state.running
                                        && !state.paused
                                        && self.receiver.is_none()
                                        && !self.confirming(),
                                    egui::Button::new(
                                        RichText::new(t("更新服务", "Update service")).color(Color32::WHITE),
                                    )
                                    .fill(BLUE),
                                )
                                .clicked()
                        {
                            self.confirm_service_update = true;
                        }
                        if ui
                            .add_enabled(
                                self.receiver.is_none() && !self.confirming(),
                                egui::Button::new(if state.installed {
                                    t("关闭并卸载", "Disable and remove")
                                } else {
                                    t("开启自动应用", "Enable auto-apply")
                                }),
                            )
                            .clicked()
                        {
                            self.confirm_auto = Some(!state.installed);
                        }
                    } else {
                        ui.add_enabled(false, egui::Button::new(t("开启自动应用", "Enable auto-apply")));
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(t("开机、插拔时按需恢复；处理结束后退出。", "Restores on startup or reconnect, then exits."))
                    .small()
                    .color(MUTED),
            );
            if let Some(error) = &self.auto_error {
                ui.colored_label(RED, if i18n::is_english() { "Could not read service status. See the log." } else { error });
            }
            if self.auto_state.is_some_and(|s| s.paused) {
                ui.colored_label(RED, t("请检查日志并手动复查，再卸载、重新开启。", "Check the log and verify manually, then reinstall the service."));
            }
            if self.auto_state.is_some_and(|s| s.needs_update) {
                ui.colored_label(
                    AMBER,
                    t("已安装的服务副本与当前程序不同；更新主程序不会自动更新服务。", "The installed service differs from this program; updating the app does not update the service."),
                );
            }
        });
    }
    fn log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("执行日志", "Execution log"))
                    .strong()
                    .size(16.0),
            );
            ui.label(
                RichText::new(t("UTC · 自动更新", "UTC · auto-refresh"))
                    .small()
                    .color(MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.confirming(),
                        egui::Button::new(t("导出日志", "Export log")),
                    )
                    .on_hover_text(t(
                        "导出程序及服务日志，不受当前筛选影响",
                        "Export program and service logs, regardless of the current filter",
                    ))
                    .clicked()
                {
                    self.save();
                }
            });
        });
        ui.horizontal(|ui| {
            for (filter, label) in [
                (Filter::All, t("全部", "All")),
                (Filter::Program, t("程序", "Program")),
                (Filter::Service, t("服务", "Service")),
            ] {
                if ui
                    .selectable_value(&mut self.filter, filter, label)
                    .changed()
                {
                    self.logs_dirty = true;
                }
            }
            ui.add_space(12.0);
            if ui
                .checkbox(&mut self.errors_only, t("仅错误", "Errors only"))
                .changed()
            {
                self.logs_dirty = true;
            }
        });
        if let Some(error) = &self.log_error {
            ui.colored_label(
                RED,
                if i18n::is_english() {
                    format!("Could not refresh log: {error} (previous content retained)")
                } else {
                    format!("日志未能更新：{error}（保留上次内容）")
                },
            );
        }
        self.rebuild_logs();
        egui::Frame::new()
            .fill(Color32::WHITE)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(8)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let height = ui.available_height().max(100.0) - 4.0;
                if self.visible_rows.is_empty() {
                    ui.set_min_height(height);
                    ui.label(
                        RichText::new(if self.errors_only {
                            t("暂无错误记录。", "No errors yet.")
                        } else {
                            t(
                                "暂无日志，新的记录会自动显示。",
                                "No log entries yet. New entries appear automatically.",
                            )
                        })
                        .color(MUTED),
                    );
                } else {
                    let row_height = 21.0;
                    egui::ScrollArea::both()
                        .id_salt(("execution-log-scroll", self.filter as u8, self.errors_only))
                        .auto_shrink([false, false])
                        .max_height(height)
                        .stick_to_bottom(true)
                        .show_rows(ui, row_height, self.visible_rows.len(), |ui, range| {
                            for index in range {
                                let row = &self.rows[self.visible_rows[index]];
                                let color = match row.level {
                                    Level::Error => RED,
                                    Level::Warning => AMBER,
                                    Level::Info => INK,
                                };
                                let time = row.time.get(11..23).unwrap_or("            ");
                                let severity = if row.level == Level::Error {
                                    t("错误 ", "Error ")
                                } else if row.level == Level::Warning {
                                    t("提示 ", "Notice ")
                                } else {
                                    ""
                                };
                                ui.allocate_ui_with_layout(
                                    egui::vec2(ui.available_width(), row_height),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(format!(
                                                    "{time}  {}  {severity}{}",
                                                    t(
                                                        row.source.label(),
                                                        match row.source {
                                                            ui_log::Source::Program => "Program",
                                                            ui_log::Source::Service => "Service",
                                                        }
                                                    ),
                                                    row.text
                                                ))
                                                .font(egui::FontId::monospace(13.0))
                                                .color(color),
                                            )
                                            .wrap_mode(egui::TextWrapMode::Extend)
                                            .selectable(true),
                                        )
                                    },
                                )
                                .inner
                                .on_hover_text(&row.time);
                            }
                        });
                }
            });
    }
    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(adapter) = self.confirm.clone() {
            egui::Modal::new(egui::Id::new("confirm-us")).frame(card().inner_margin(24.0)).show(ctx, |ui| {
                ui.set_width(420.0); ui.heading(t("设置为 US？", "Set to US?")); ui.add_space(8.0);
                ui.label(adapter_label(&adapter, &self.adapters));
                if self.device_status.as_ref().is_some_and(|s| !s.country_known()) {
                    ui.colored_label(AMBER, t("原国家码未知。确认后仅尝试一次，驱动回复和状态复查均通过才判定成功。", "Original country code unknown. Only one attempt; success requires a valid driver reply and verification."));
                }
                ui.label(t("改变适配器运行时策略，可能启用 6 GHz 或短暂影响连接。只提交一次并复查，不自动回滚。", "Changes the adapter's runtime policy. It may enable 6 GHz or briefly disrupt the connection. Sends one request and verifies; no automatic rollback."));
                if adapter.unverified_driver { ui.colored_label(AMBER, t("此驱动版本未验证，可能不兼容。", "This driver version is unverified and may be incompatible.")); }
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary_button(ui, t("确认设置", "Confirm"), true).clicked() { self.start(adapter, true); }
                    if ui.add_sized([96.0, 36.0], egui::Button::new(t("取消", "Cancel"))).clicked() { self.confirm = None; }
                });
            });
        }
        if let Some(install) = self.confirm_auto {
            egui::Modal::new(egui::Id::new("confirm-auto")).frame(card().inner_margin(24.0)).show(ctx, |ui| {
                ui.set_width(420.0); ui.heading(if install { t("开启自动应用？", "Enable auto-apply?") } else { t("关闭并卸载？", "Disable and remove?") }); ui.add_space(8.0);
                ui.label(if install {
                    t("安装 Windows 服务，对本机所有 Steam Frame 适配器自动设置 US。开启后立即检查，之后在开机与插拔时触发。", "Install a Windows service to set US on all local Steam Frame adapters. Checks now, then on startup and reconnect.")
                } else { t("停止并移除服务和安装副本。保留 logs 目录，不撤销当前 US 设置。", "Stop and remove the service and its copy. Logs remain; the current US setting is not undone.") });
                if install { ui.colored_label(AMBER, t("需管理员权限；未验证驱动也会尝试操作。", "Administrator access required; unverified drivers will also be tried.")); }
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary_button(ui, if install { t("确认开启", "Enable") } else { t("确认卸载", "Remove") }, true).clicked() { self.set_auto(install); }
                    if ui.add_sized([96.0, 36.0], egui::Button::new(t("取消", "Cancel"))).clicked() { self.confirm_auto = None; }
                });
            });
        }
        if self.confirm_service_update {
            egui::Modal::new(egui::Id::new("confirm-service-update"))
                .frame(card().inner_margin(24.0))
                .show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.heading(t("更新自动应用服务？", "Update auto-apply service?"));
                    ui.label(t("停止并移除旧服务，再安装当前程序副本。保留执行日志；需管理员权限。", "Remove the old service and install a copy of this program. Logs are kept; administrator access required."));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if primary_button(ui, t("确认更新", "Update"), true).clicked() {
                            self.update_service();
                        }
                        if ui.button(t("取消", "Cancel")).clicked() {
                            self.confirm_service_update = false;
                        }
                    });
                });
        }
        if self.confirm_update
            && let Some(release) = self.available_update.clone()
        {
            egui::Modal::new(egui::Id::new("confirm-program-update"))
                .frame(card().inner_margin(24.0))
                .show(ctx, |ui| {
                    ui.set_width(430.0);
                    ui.heading(if i18n::is_english() { format!("New version v{} available", release.version) } else { format!("发现新版本 v{}", release.version) });
                    ui.label(t("请在 Release 页面下载新版程序。关闭本程序后，在安装目录替换旧 EXE。", "Download the new version from its Release page. Close this app, then replace the EXE in its folder."));
                    ui.label(t("已安装的自动应用服务不会同步更新；替换后可在主窗口点击“更新服务”。", "The installed auto-apply service is not updated with the app. After replacing the EXE, click Update service."));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if primary_button(ui, t("打开 Release 页面", "Open Release page"), true).clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&release.page));
                        }
                        if ui.button(t("打开安装目录", "Open app folder")).clicked() {
                            match std::env::current_exe()
                                .ok()
                                .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
                            {
                                Some(dir) => {
                                    if let Err(e) =
                                        std::process::Command::new("explorer.exe").arg(dir).spawn()
                                    {
                                        self.failure(t("无法打开安装目录。", "Could not open the app folder."), e.to_string(), true);
                                    }
                                }
                                None => self.failure(
                                    t("无法打开安装目录。", "Could not open the app folder."),
                                    t("无法取得当前程序所在目录。", "Could not locate this program.").into(),
                                    true,
                                ),
                            }
                        }
                        if ui.button(t("稍后", "Later")).clicked() {
                            self.confirm_update = false;
                        }
                    });
                });
        }
        if let Some(error) = self.error_popup.clone() {
            egui::Modal::new(egui::Id::new("error-popup"))
                .frame(card().inner_margin(24.0))
                .show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.colored_label(
                        RED,
                        RichText::new(t("操作未完成", "Operation incomplete")).heading(),
                    );
                    ui.add_space(8.0);
                    if i18n::is_english() {
                        ui.colored_label(RED, "See the diagnostic details below:");
                    }
                    ui.colored_label(RED, error);
                    ui.add_space(12.0);
                    if ui.button(t("知道了", "OK")).clicked() {
                        self.error_popup = None;
                    }
                });
        }
    }
    fn draw(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(246, 248, 251))
                    .inner_margin(20.0),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Steam Frame").size(24.0).strong().color(INK));
                    ui.label(
                        RichText::new(format!(
                            "{} · v{}",
                            t("6 GHz 设置工具", "6 GHz Tool"),
                            env!("CARGO_PKG_VERSION")
                        ))
                        .size(15.0)
                        .color(MUTED),
                    );
                    if self.demo {
                        pill(ui, t("演示", "Demo"), AMBER);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if github_button(ui, self.github_mark.as_ref()).clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(frame::REPO));
                        }
                        if ui
                            .add(egui::Button::new("🌐").min_size(egui::vec2(36.0, 36.0)))
                            .on_hover_text(t("切换为英语", "Switch to Chinese"))
                            .clicked()
                        {
                            i18n::toggle();
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(
                                t("Steam Frame 6 GHz 设置工具", "Steam Frame 6 GHz Tool").into(),
                            ));
                            self.status = t("已切换为中文。", "Switched to English.").into();
                            self.status_level = Level::Info;
                        }
                        if primary_button(ui, t("头显设置", "Headset settings"), true).clicked()
                            && self.frame_ui.is_none()
                        {
                            self.frame_ui = Some(frame_ui::FrameUi::new(self.demo));
                        }
                    });
                });
                if let Some(release) = &self.available_update {
                    ui.horizontal(|ui| {
                        ui.label(if i18n::is_english() {
                            format!("New version v{} available", release.version)
                        } else {
                            format!("发现新版本 v{}", release.version)
                        });
                        if ui
                            .add_enabled(
                                self.frame_ui.is_none(),
                                egui::Button::new(t("更新程序", "Get update")),
                            )
                            .clicked()
                        {
                            self.confirm_update = true;
                        }
                    });
                }
                ui.add_space(14.0);
                self.device_card(ui);
                ui.add_space(10.0);
                self.auto_card(ui);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if self.receiver.is_some() {
                        ui.spinner();
                    }
                    ui.label(RichText::new(&self.status).color(
                        if self.status_level == Level::Error {
                            RED
                        } else {
                            MUTED
                        },
                    ));
                });
                ui.add_space(12.0);
                self.log_panel(ui);
            });
        self.dialogs(ctx);
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll_update();
        self.poll_service();
        self.poll();
        if (self.receiver.is_some() || self.frame_ui.as_ref().is_some_and(|ui| ui.busy()))
            && ctx.input(|i| i.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.status = t(
                "操作尚未结束，请稍候再关闭窗口。",
                "Operation still running. Wait before closing.",
            )
            .into();
        }
        self.draw(ctx);
        if let Some(child) = &mut self.frame_ui {
            let close = ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("frame-settings"),
                egui::ViewportBuilder::default()
                    .with_title(t(
                        "Steam Frame · 头显设置",
                        "Steam Frame · Headset settings",
                    ))
                    .with_inner_size([680.0, 650.0])
                    .with_min_inner_size([560.0, 520.0]),
                |child_ctx, _| child.draw(child_ctx),
            );
            let lines = child.take_log();
            for line in lines {
                self.record(&format!("{}：{line}", t("头显", "Headset")));
            }
            if close {
                self.frame_ui = None;
            }
        }
        ctx.request_repaint_after(
            if self.receiver.is_some()
                || self.service_receiver.is_some()
                || self.update_receiver.is_some()
                || self.frame_ui.is_some()
            {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(2)
            },
        );
    }
}
fn setup_style(ctx: &egui::Context) {
    // This UI has a light palette. Pin it before applying the style: otherwise
    // the first OS theme event selects a different, uncustomized style.
    ctx.set_theme(egui::Theme::Light);
    let mut fonts = egui::FontDefinitions::default();
    for file in ["msyh.ttc", "simhei.ttf", "simsun.ttc"] {
        if let Ok(bytes) = fs::read(backend::windows_dir().join("Fonts").join(file)) {
            fonts
                .font_data
                .insert("Chinese".into(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "Chinese".into());
            fonts
                .families
                .get_mut(&egui::FontFamily::Monospace)
                .unwrap()
                .push("Chinese".into());
            break;
        }
    }
    ctx.set_fonts(fonts);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.override_text_color = Some(INK);
    style.visuals.selection.bg_fill = Color32::from_rgb(219, 234, 254);
    style.visuals.selection.stroke.color = BLUE;
    for (widget, fill, border) in [
        (&mut style.visuals.widgets.inactive, Color32::WHITE, BORDER),
        (
            &mut style.visuals.widgets.hovered,
            Color32::from_rgb(239, 246, 255),
            Color32::from_rgb(147, 197, 253),
        ),
        (
            &mut style.visuals.widgets.active,
            Color32::from_rgb(219, 234, 254),
            BLUE,
        ),
        (
            &mut style.visuals.widgets.open,
            Color32::from_rgb(239, 246, 255),
            BLUE,
        ),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = egui::Stroke::new(1.0, border);
        widget.fg_stroke.color = INK;
        widget.corner_radius = egui::CornerRadius::same(7);
        widget.expansion = 0.0;
    }
    style.visuals.window_corner_radius = egui::CornerRadius::same(10);
    style.visuals.window_fill = Color32::WHITE;
    style.visuals.window_stroke = egui::Stroke::new(1.0, BORDER);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size.y = 36.0;
    style.spacing.menu_margin = egui::Margin::same(6);
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    ctx.set_style(style);
}
fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(10)
        .inner_margin(16.0)
}
fn load_github_mark(ctx: &egui::Context) -> egui::TextureHandle {
    // GitHub's mark, downscaled from github.githubassets.com/images/modules/logos_page/GitHub-Mark.png.
    let png = base64::engine::general_purpose::STANDARD
        .decode(
            include_str!("github_mark.png.b64")
                .lines()
                .collect::<String>(),
        )
        .expect("embedded GitHub mark is valid base64");
    let rgba = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .expect("embedded GitHub mark is a valid PNG")
        .to_rgba8();
    let pixels = egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    );
    ctx.load_texture("github-mark", pixels, egui::TextureOptions::LINEAR)
}
fn github_button(ui: &mut egui::Ui, mark: Option<&egui::TextureHandle>) -> egui::Response {
    let button = if let Some(mark) = mark {
        egui::Button::new(egui::Image::new(mark).fit_to_exact_size(egui::vec2(20.0, 20.0)))
    } else {
        egui::Button::new("GitHub")
    };
    ui.add(button.min_size(egui::vec2(36.0, 36.0)))
        .on_hover_text(t("打开 GitHub 仓库", "Open GitHub repository"))
}
fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    ui.scope(|ui| {
        // Use normal widget states so hover, keyboard focus and disabled feedback remain native.
        let visuals = ui.visuals_mut();
        visuals.override_text_color = Some(Color32::WHITE);
        for (widget, fill) in [
            (&mut visuals.widgets.inactive, BLUE),
            (&mut visuals.widgets.hovered, Color32::from_rgb(29, 78, 216)),
            (&mut visuals.widgets.active, Color32::from_rgb(30, 64, 175)),
        ] {
            widget.weak_bg_fill = fill;
            widget.bg_fill = fill;
            widget.bg_stroke = egui::Stroke::new(1.0, fill);
            widget.fg_stroke.color = Color32::WHITE;
        }
        ui.add_enabled(
            enabled,
            egui::Button::new(text).min_size(egui::vec2(96.0, 36.0)),
        )
    })
    .inner
}
fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.09))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(13.0));
        });
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
fn main() -> eframe::Result {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--service"] {
        if service::dispatch().is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }
    if args == ["--list"] {
        match backend::enumerate() {
            Ok(items) => {
                for a in items {
                    println!("{} | {} | {} | {:?}", a.id, a.name, a.pnp, a.compatibility);
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    if !args.is_empty() && args != ["--demo"] {
        rfd::MessageDialog::new().set_title(t("参数错误", "Invalid arguments"))
            .set_description(t("支持无参数启动、--demo 演示、--list 只读枚举；--service 仅由 Windows 服务管理器调用。", "Start without arguments, or use --demo / --list. --service is reserved for Windows Service Manager.")).show();
        return Ok(());
    }
    let demo = args == ["--demo"];
    eframe::run_native(
        t("Steam Frame 6 GHz 设置工具", "Steam Frame 6 GHz Tool"),
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_icon(egui::IconData::default())
                .with_inner_size([820.0, 720.0])
                .with_min_inner_size([680.0, 620.0]),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(App::new(cc, demo)))),
    )
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    #[test]
    fn embedded_github_mark_loads() {
        let mark = load_github_mark(&egui::Context::default());
        assert_eq!(mark.size(), [48, 48]);
    }
    #[test]
    fn auto_apply_requires_explicit_action_and_demo_never_installs() {
        let mut app = App::empty(true);
        app.auto_state = Some(service::State::default());
        app.poll();
        assert!(!app.auto_state.unwrap().installed);
        assert!(app.receiver.is_none());
        for install in [true, false] {
            app.confirm_auto = Some(install);
            app.set_auto(install);
            let event = app
                .receiver
                .take()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            let Event::AutoApply(message, state) = event else {
                panic!("expected demo action")
            };
            assert!(message.unwrap().contains("未修改系统"));
            assert_eq!(state.unwrap().installed, install);
            assert!(app.confirm_auto.is_none());
        }
        let (tx, rx) = mpsc::channel();
        app.receiver = Some(rx);
        let message = "权限不足：请以管理员身份运行".to_string();
        tx.send(Event::AutoApply(
            Err(message.clone()),
            Ok(service::State::default()),
        ))
        .unwrap();
        app.poll();
        assert_eq!(app.error_popup.as_deref(), Some(message.as_str()));
        assert!(app.status.contains("管理员"));
        assert_eq!(app.status_level, Level::Error);
        assert!(app.log.contains(&message));
        app.rebuild_logs();
        assert!(app.rows.iter().any(|r| r.level == Level::Error));
    }
    #[test]
    fn selection_queries_once_without_setting_us() {
        for count in 0..=2 {
            let (tx, rx) = mpsc::channel();
            let mut app = App::empty(true);
            app.receiver = Some(rx);
            let items = (0..count)
                .map(|i| {
                    let mut a = demo_adapter();
                    a.id = format!("DEMO-{i}");
                    a.unverified_driver = true;
                    a
                })
                .collect();
            tx.send(Event::Devices(Ok(items), Ok(service::State::default())))
                .unwrap();
            app.poll();
            assert_eq!(app.selected, (count == 1).then_some(0));
            assert_eq!(app.receiver.is_some(), count == 1);
            if count == 0 {
                continue;
            }
            if count == 2 {
                app.select(Some(1));
            }
            let chosen = app.selected;
            app.select(Some(0));
            assert_eq!(app.selected, chosen);
            let event = app
                .receiver
                .take()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            let Event::Operation(report) = event else {
                panic!("expected status query")
            };
            assert_eq!(report.status.unwrap().country, "CN");
            app.select(chosen);
            assert!(app.receiver.is_none());
            if count == 2 {
                app.select(Some(0));
                assert!(
                    app.receiver
                        .take()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(2))
                        .is_ok()
                );
                app.adapters[1].compatibility = Err("unsupported driver".into());
                app.select(Some(1));
                assert!(app.receiver.is_none());
                assert!(app.status.contains("不受支持"));
            }
        }
    }
    #[test]
    fn service_logs_refresh_without_duplicates_and_errors_do_not_clear_history() {
        let mut app = App::empty(true);
        app.record("program entry");
        let text = "[2026-09-26T08:00:00.000Z] [ERROR] service failure\n";
        app.apply_snapshot(ServiceSnapshot {
            epoch: 0,
            state: Ok(service::State::default()),
            logs: Ok((1, Some(text.into()), PathBuf::from(r"F:\tools\logs"))),
        });
        app.apply_snapshot(ServiceSnapshot {
            epoch: 0,
            state: Ok(service::State::default()),
            logs: Ok((1, None, PathBuf::from(r"F:\tools\logs"))),
        });
        assert_eq!(app.service_log, text);
        app.filter = Filter::Service;
        app.errors_only = true;
        app.rebuild_logs();
        assert_eq!(app.visible_rows.len(), 1);
        assert_eq!(app.rows[app.visible_rows[0]].level, Level::Error);
        app.apply_snapshot(ServiceSnapshot {
            epoch: 0,
            state: Ok(service::State::default()),
            logs: Err("denied".into()),
        });
        assert_eq!(app.service_log, text);
        assert!(app.log_error.is_some());
        let previous = app.log.clone();
        app.apply_snapshot(ServiceSnapshot {
            epoch: 0,
            state: Ok(service::State::default()),
            logs: Err("denied".into()),
        });
        assert_eq!(app.log, previous);
        app.service_epoch = 1;
        app.apply_snapshot(ServiceSnapshot {
            epoch: 0,
            state: Ok(service::State::default()),
            logs: Ok((2, Some("stale".into()), PathBuf::new())),
        });
        assert_eq!(app.service_log, text);
    }
    #[test]
    fn short_status_keeps_unknown_distinct() {
        for (info, expected) in [
            ("6G NOT Support", "不可用"),
            ("6G Support (domain:05)", "可用"),
            ("Platform not support 6G", "未知（见日志）"),
        ] {
            assert_eq!(
                status_summary(&protocol::Status {
                    country: "US".into(),
                    info: info.into()
                }),
                format!("国家：US     6 GHz：{expected}")
            );
        }
    }
    #[test]
    fn log_text_is_painted_inside_visible_clip() {
        let ctx = egui::Context::default();
        setup_style(&ctx);
        let mut app = App::empty(true);
        for _ in 0..100 {
            app.record("VISIBLE_LOG_SENTINEL");
        }
        app.service_log = "[2026-09-26T08:00:00.000Z] [ERROR] VISIBLE_LOG_SENTINEL".into();
        for (filter, errors_only) in [
            (Filter::All, false),
            (Filter::Program, false),
            (Filter::Service, true),
        ] {
            app.filter = filter;
            app.errors_only = errors_only;
            app.logs_dirty = true;
            for _ in 0..3 {
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(820.0, 720.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                );
                let found = output.shapes.iter().any(|clipped| {
                    if let egui::epaint::Shape::Text(text) = &clipped.shape {
                        text.galley.text().contains("VISIBLE_LOG_SENTINEL")
                            && clipped
                                .clip_rect
                                .contains_rect(text.galley.rect.translate(text.pos.to_vec2()))
                            && (!errors_only
                                || text
                                    .galley
                                    .job
                                    .sections
                                    .iter()
                                    .any(|s| s.format.color == RED))
                    } else {
                        false
                    }
                });
                assert!(found, "log text missing or outside viewport");
            }
        }
    }
    #[test]
    fn custom_controls_survive_first_system_theme_event() {
        let ctx = egui::Context::default();
        setup_style(&ctx);
        let _ = ctx.run(
            egui::RawInput {
                system_theme: Some(egui::Theme::Light),
                ..Default::default()
            },
            |ctx| {
                assert_eq!(ctx.style().spacing.interact_size.y, 36.0);
                assert_eq!(
                    ctx.style().visuals.widgets.inactive.weak_bg_fill,
                    Color32::WHITE
                );
            },
        );
    }
    #[test]
    fn layout_renders_small_window_and_error_dialog_without_device_calls() {
        let ctx = egui::Context::default();
        setup_style(&ctx);
        let mut app = App::empty(true);
        app.adapters = vec![demo_adapter()];
        app.selected = Some(0);
        app.auto_state = Some(service::State::default());
        app.failure("权限不足", "请以管理员身份运行".into(), true);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(680.0, 620.0),
                )),
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        );
        assert!(!output.shapes.is_empty());
        assert!(app.receiver.is_none());
        assert!(app.service_receiver.is_none());
    }
}
