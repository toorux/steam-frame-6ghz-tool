use gpui_kit::{prelude::*, *};
use gpui_kit::component::{Disableable, Icon, Sizable, button::{Button, ButtonVariants}, checkbox::Checkbox, input::{Input, InputState}, scroll::ScrollableElement};

const INK: u32 = 0x1f3445;
const MUTED: u32 = 0x526c7c;
const BORDER: u32 = 0x718c9b;
const ERROR: u32 = 0xb42318;
const LINK: u32 = 0x176b91;

pub(crate) struct FrameView {
    workflow: FrameUi,
    ip: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    result_scroll: ScrollHandle,
    export_receiver: Option<Receiver<Result<Option<std::path::PathBuf>, String>>>,
    service_export_receiver: Option<Receiver<Result<Option<std::path::PathBuf>, String>>>,
}

impl FrameView {
    pub(crate) fn new(demo: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| loop {
            let Some(this) = this.upgrade() else { break };
            let interval = this.update(cx, |view, cx| {
                let previous = view.workflow.lines.len();
                view.workflow.poll();
                view.poll_exports();
                if previous != view.workflow.lines.len() { view.result_scroll.scroll_to_bottom(); }
                share_logs(view.workflow.take_log());
                cx.notify();
                if view.workflow.scan.is_some() { std::time::Duration::from_millis(16) }
                else { std::time::Duration::from_millis(180) }
            });
            cx.background_executor().timer(interval).await;
        }).detach();
        Self {
            workflow: FrameUi::new(demo),
            ip: cx.new(|cx| InputState::new(window, cx)),
            username: cx.new(|cx| InputState::new(window, cx).default_value("steamos")),
            password: cx.new(|cx| InputState::new(window, cx).masked(true)),
            result_scroll: ScrollHandle::new(),
            export_receiver: None,
            service_export_receiver: None,
        }
    }
    fn sync_fields(&mut self, cx: &App) {
        self.workflow.ip = self.ip.read(cx).value().to_string();
        self.workflow.username = self.username.read(cx).value().to_string();
        self.workflow.password = self.password.read(cx).unmask_value().to_string();
    }
    fn clear_passwords(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.password.update(cx, |field, cx| field.set_value("", window, cx));
    }
    fn choose_ip(&mut self, ip: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.workflow.busy() { return; }
        self.ip.update(cx, |field, cx| field.set_value(ip, window, cx));
        self.workflow.preview = None;
        cx.notify();
    }
    fn run_probe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workflow.busy() { return; }
        self.sync_fields(cx);
        self.clear_passwords(window, cx);
        self.workflow.probe();
        cx.notify();
    }
    fn run_execute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.workflow.trust_new { return; }
        self.clear_passwords(window, cx);
        self.workflow.execute();
        cx.notify();
    }
    pub(crate) fn cancel_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A pending handshake may finish after leaving the page; discard its reply.
        if !self.workflow.password.is_empty() { self.workflow.receiver = None; }
        self.workflow.preview = None;
        self.workflow.password.zeroize();
        if !self.workflow.busy() { self.workflow.status.clear(); }
        self.clear_passwords(window, cx);
        cx.notify();
    }
    pub(crate) fn language_changed(&mut self, cx: &mut Context<Self>) {
        self.workflow.relocalize_status();
        cx.notify();
    }
    fn export_log(&mut self) {
        if self.export_receiver.is_some() { return; }
        let filter = t("日志", "Log").to_owned();
        let text = format!("\u{feff}{}\n", self.workflow.lines.join("\n"));
        let (tx, rx) = std::sync::mpsc::channel();
        self.export_receiver = Some(rx);
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(|| {
                if let Some(path) = rfd::FileDialog::new().add_filter(filter.as_str(), &["txt"])
                    .set_file_name("steam-frame-headset-log.txt").save_file() {
                    std::fs::write(&path, text).map_err(|e| e.to_string())?;
                    Ok(Some(path))
                } else { Ok(None) }
            }).unwrap_or_else(|_| Err("保存对话框意外终止".into()));
            let _ = tx.send(result);
        });
    }
    fn export_service_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workflow.busy() || self.workflow.demo || self.service_export_receiver.is_some() { return; }
        self.sync_fields(cx);
        self.clear_passwords(window, cx);
        let filter = t("日志", "Log").to_owned();
        let (tx, rx) = std::sync::mpsc::channel();
        self.service_export_receiver = Some(rx);
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(|| Ok(rfd::FileDialog::new()
                .add_filter(filter.as_str(), &["txt"])
                .set_file_name("steam-frame-headset-service-log.txt").save_file()))
                .unwrap_or_else(|_| Err("保存对话框意外终止".into()));
            let _ = tx.send(result);
        });
    }
    fn poll_exports(&mut self) {
        let export = self.export_receiver.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("保存对话框线程意外结束".into())),
        });
        if let Some(result) = export {
            self.export_receiver = None;
            if let Err(error) = result {
                self.workflow.error_dialog = Some(format!("{} {error}", t("日志导出失败。", "Could not export the log.")));
                self.workflow.result = Some(frame::SetupResult::Failed);
                let line = format!("[ERROR] {} {error}", t("日志导出失败。", "Could not export the log."));
                self.workflow.lines.push(line.clone());
                self.workflow.exported.push(line);
            }
        }
        let service_export = self.service_export_receiver.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("保存对话框线程意外结束".into())),
        });
        if let Some(result) = service_export {
            self.service_export_receiver = None;
            match result {
                Ok(Some(path)) => {
                    if self.workflow.busy() {
                        self.workflow.password.zeroize();
                        self.workflow.service_export_error(t("当前操作尚未完成，请稍后重试导出。", "An operation is still running; retry the export later.").into());
                    } else {
                        self.workflow.export_service_log(path);
                    }
                }
                Ok(None) => self.workflow.password.zeroize(),
                Err(error) => {
                    self.workflow.password.zeroize();
                    self.workflow.service_export_error(format!("{} {error}", t("日志导出失败。", "Could not export the log.")));
                }
            }
        }
    }
    fn card() -> Div {
        div().w_full().rounded_lg().border_1().border_color(rgb(BORDER)).bg(rgb(0xffffff))
            .p_4().flex().flex_col().gap_3()
    }
    fn label(text: impl Into<SharedString>) -> Div { div().text_color(rgb(INK)).child(text.into()) }
    fn muted(text: impl Into<SharedString>) -> Div { div().text_color(rgb(MUTED)).text_sm().child(text.into()) }
    fn help_link(button: Button) -> Button { button.link().small().text_color(rgb(LINK)) }

}

impl Render for FrameView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.workflow.busy() || self.service_export_receiver.is_some();
        let scan_running = self.workflow.scan.is_some();
        let scan_progress = self.workflow.scan.as_ref().map(|s| format!("{}/{}", s.progress.load(Ordering::Relaxed), s.total)).unwrap_or_default();
        let mut candidates = div().flex().flex_col().gap_1();
        for candidate in &self.workflow.candidates {
            let ip = candidate.ip.to_string();
            let label = format!("frame · {} · {}", ip, if candidate.ssh_open { t("SSH 可连接", "SSH ready") } else { t("SSH 未连接", "SSH not available") });
            candidates = candidates.child(Button::new(format!("candidate-{ip}")).label(label).h(px(32.)).disabled(busy)
                .on_click(cx.listener(move |this, _, window, cx| this.choose_ip(ip.clone(), window, cx))));
        }
        if self.workflow.candidates.is_empty() {
            candidates = candidates.child(Self::muted(t("未发现 Frame；可直接填写 IP。", "No Frame found; enter its IP directly.")));
        }
        let workflow_status = self.workflow.status.clone();
        let preview = self.workflow.preview.as_ref().map(|(ip, user, probe)| (ip.to_string(), user.clone(), probe.fingerprint.clone(), probe.previous.clone()));
        let refresh_angle = if scan_running {
            (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_millis() % 840) as f32 * std::f32::consts::TAU / 840.0
        } else { 0.0 };
        let scan_header = div().flex().items_center().justify_between()
            .child(div().flex().items_center().gap_2()
                .child(Self::label(t("连接 Frame", "Connect to Frame")).text_lg())
                .when(scan_running, |row| row
                    .child(Self::muted(format!("{} {scan_progress}", t("扫描中", "Scanning"))))
                    .child(Self::help_link(Button::new("cancel-scan").label(t("取消", "Cancel")))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(scan) = &this.workflow.scan { scan.cancel.store(true, Ordering::Relaxed); }
                            cx.notify();
                        }))))
                .when(self.workflow.large_scan, |row| row
                    .child(Self::muted(t("网段较大，扫描可能较慢。", "Large subnet; scanning may take time.")))
                    .child(Self::help_link(Button::new("large-scan").label(t("继续扫描", "Scan anyway")))
                        .on_click(cx.listener(|this, _, _, cx| { this.workflow.start_scan(); cx.notify(); })))))
            .child(Button::new("scan").disabled(busy).icon(Icon::default().data(include_bytes!("../assets/refresh.svg")).rotate(radians(refresh_angle))).ghost()
                .accessibility_label(t("重新扫描", "Rescan"))
                .tooltip(t("重新扫描", "Rescan"))
                .on_click(cx.listener(|this, _, _, cx| { this.workflow.scan(); cx.notify(); })));
        let body = div().id("frame-content").flex_shrink_0().max_h(px(570.)).min_w(px(0.)).overflow_y_scroll()
            .text_color(rgb(INK)).flex().flex_col().items_center().px_4().pt_4().pb_3().gap_3()
            .child(Self::card().max_w(px(760.)).flex_shrink_0().gap_1()
                .child(scan_header)
                .child(div().h(px(122.)).flex_shrink_0().w_full().rounded_md().border_1().border_color(rgb(0xd8e4ea))
                    .bg(rgb(0xf7fafc)).p_2().overflow_y_scrollbar().child(candidates)))
            .child(Self::card().max_w(px(760.)).flex_shrink_0().gap_1()
                .child(Self::label(t("Frame IP 地址", "Frame IP address")))
                .child(Input::new(&self.ip).id("frame-ip").w_full().disabled(busy))
                .child(div().flex().child(Self::help_link(Button::new("ip-help").label(t("如何查看 Frame IP", "Find your Frame's IP address")))
                    .on_click(cx.listener(|_, _, _, cx| cx.open_url(t(frame::IP_HELP, frame::IP_HELP_EN))))))
                .child(div().flex().gap_3()
                    .child(div().flex_1().min_w(px(0.)).flex().flex_col().gap_1().child(Self::label(t("用户名", "Username"))).child(Input::new(&self.username).id("username").w_full().disabled(busy)))
                    .child(div().flex_1().min_w(px(0.)).flex().flex_col().gap_1().child(Self::label(t("SSH 密码", "SSH password"))).child(Input::new(&self.password).id("ssh-password").w_full().disabled(busy))))
                .child(Self::muted(t("程序不会收集、保存或上传你的密码；仅在本机内存用于本次 SSH / sudo 验证，不写入日志。", "Your password is not collected, saved, or uploaded; it is only used in memory for this SSH / sudo session and is never logged.")))
                .child(Self::muted(t("将在头显安装自动维护：开机或区域变化时按需恢复 US。", "Installs headset auto-maintenance to restore US after startup or regulatory changes.")))
                .child(div().flex().justify_between()
                    .child(Self::help_link(Button::new("ssh-help").label(t("如何开启 SSH", "How to enable SSH")))
                        .on_click(cx.listener(|_, _, _, cx| cx.open_url(t(frame::SSH_HELP, frame::SSH_HELP_EN)))))
                    .child(div().flex().items_center().gap_2()
                        .child(Button::new("export-service-log").label(t("导出头显服务日志", "Export service log"))
                            .ghost().small().text_color(rgb(MUTED)).disabled(busy || self.workflow.demo)
                            .on_click(cx.listener(|this, _, window, cx| this.export_service_log(window, cx))))
                        .child(Button::new("probe").label(t("连接并设置", "Connect and set up")).primary().disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| { if !this.workflow.busy() && !this.workflow.demo { this.run_probe(window, cx); } }))))));
        let lines = crate::gpui_log::text("headset-log-text", self.workflow.lines.iter()
            .map(|line| (line.clone(), crate::ui_log::level(line) == crate::ui_log::Level::Error)));
        let mut page = div().size_full().relative().bg(rgb(0xf7fafc)).flex().flex_col()
            .child(body)
            .child(div().flex_1().min_h(px(0.)).px_4().pb_4().flex()
                .child(Self::card().flex_1().min_h(px(0.))
                    .child(div().flex().items_center().justify_between()
                        .child(Self::label(t("执行日志", "Execution log")).text_lg())
                        .child(Button::new("export-headset-log").label(t("导出日志", "Export log")).ghost().small().text_color(rgb(MUTED))
                            .on_click(cx.listener(|this, _, _, cx| { this.export_log(); cx.notify(); }))))
                    .when(!self.workflow.status_error && !workflow_status.is_empty(), |card| card.child(Self::muted(workflow_status)))
                    .child(crate::gpui_log::pane("frame-results", &self.result_scroll, lines))));
        if let Some((ip, user, fingerprint, previous)) = preview {
            let changed = previous.as_ref().is_some_and(|old| old != &fingerprint);
            let needs_trust = previous.is_none() || changed;
            let confirmation = Self::card().w(px(670.)).p_6().gap_5()
                .child(Self::label(if changed { t("SSH 主机指纹已更改", "SSH host key changed") } else { t("信任此头显？", "Trust this headset?") }).text_xl())
                .child(Self::label(format!("{}: {user}@{ip}", t("目标", "Target"))))
                .when(changed, |card| card.child(Self::muted(format!("{}: {}", t("原指纹", "Previous fingerprint"), previous.as_deref().unwrap_or_default()))))
                .child(Self::label(format!("{}: {fingerprint}", t("当前指纹", "Current fingerprint"))))
                .when(needs_trust, |card| card.child(Checkbox::new("trust-fingerprint")
                    .label(if changed { t("我已通过可信渠道核对新指纹，确认重新信任此头显", "I verified the new fingerprint through a trusted source and trust this headset again") } else { t("首次连接：我已核对并信任此指纹", "First connection: I verified and trust this fingerprint") })
                    .checked(self.workflow.trust_new)
                    .on_change(cx.listener(|this, value, _, cx| { this.workflow.trust_new = *value; cx.notify(); }))))
                .child(Self::muted(if changed { t("指纹变化可能是重装系统，也可能是连接到了其他设备。核对前不会发送密码。", "A changed key may mean the headset was reinstalled—or that this is another device. No password will be sent before you confirm.") } else { t("请通过可信渠道核对指纹。继续后将设置 US 并安装自动维护服务；不会自动重启。", "Verify this fingerprint through a trusted source. Continuing will set US and install automatic maintenance without restarting your headset.") }))
                .child(div().flex().justify_end().gap_2()
                    .child(Button::new("cancel-preview").label(t("取消", "Cancel"))
                        .on_click(cx.listener(|this, _, window, cx| this.cancel_preview(window, cx))))
                    .child(Button::new("execute").label(if changed { t("重新信任并继续", "Trust new key and continue") } else { t("信任并继续", "Trust and continue") }).primary()
                        .disabled(!self.workflow.trust_new)
                        .on_click(cx.listener(|this, _, window, cx| this.run_execute(window, cx)))));
            page = page.child(div().absolute().inset_0().occlude().bg(rgba(0x1f3445b0)).flex().items_center().justify_center().child(confirmation));
        }
        if let Some(error) = self.workflow.error_dialog.clone() {
            let (title, color) = match self.workflow.result {
                Some(frame::SetupResult::AlreadySet) => (t("已经是 US", "Already set to US"), INK),
                Some(frame::SetupResult::Success) => (t("设置成功", "Setup complete"), INK),
                Some(frame::SetupResult::NeedsRestart) => (t("请重启头显测试", "Restart and test your headset"), 0x8a4b08),
                Some(frame::SetupResult::Partial) => (t("设置部分完成", "Setup partially completed"), ERROR),
                _ => (t("操作未完成", "Unable to complete setup"), ERROR),
            };
            page = page.child(div().absolute().inset_0().occlude().bg(rgba(0x1f3445b0)).flex().items_center().justify_center()
                .child(Self::card().w(px(520.)).p_6().gap_5()
                    .child(Self::label(title).text_xl().text_color(rgb(color)))
                    .child(Self::label(error))
                    .child(div().flex().justify_end().child(Button::new("dismiss-frame-error").label(t("知道了", "OK")).primary()
                        .on_click(cx.listener(|this, _, _, cx| { this.workflow.error_dialog = None; cx.notify(); }))))));
        }
        if let Some(message) = self.workflow.service_export_dialog.clone() {
            page = page.child(div().absolute().inset_0().occlude().bg(rgba(0x1f3445b0)).flex().items_center().justify_center()
                .child(Self::card().w(px(520.)).p_6().gap_5()
                    .child(Self::label(t("头显服务日志", "Headset service log")).text_xl())
                    .child(Self::label(message))
                    .child(div().flex().justify_end().child(Button::new("dismiss-service-export").label(t("知道了", "OK")).primary()
                        .on_click(cx.listener(|this, _, _, cx| { this.workflow.service_export_dialog = None; cx.notify(); }))))));
        }
        page
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::FrameView;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, TestAppContext, px, size};

    #[gpui_kit::test]
    fn cancel_clears_masked_ssh_input(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(860.), px(760.)), |window, cx| FrameView::new(true, window, cx));
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("ssh-password", cx);
            window.input("private-ssh", cx);
        }).unwrap();
        handle.update(cx, |view, window, cx| {
            view.sync_fields(cx);
            assert_eq!(view.workflow.password, "private-ssh");
            view.cancel_preview(window, cx);
            assert!(view.workflow.password.is_empty());
            assert!(view.password.read(cx).unmask_value().is_empty());
        }).unwrap();
    }
}
