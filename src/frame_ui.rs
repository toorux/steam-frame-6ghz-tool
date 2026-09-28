use crate::frame::{self, Candidate, Credentials, Network, Outcome, Probe, Scan};
use crate::i18n::{self, t};
use crate::{BLUE, INK, MUTED, RED, card, primary_button};
use eframe::egui::{self, Color32, RichText};
use std::{
    net::Ipv4Addr,
    sync::atomic::Ordering,
    sync::mpsc::{self, Receiver},
    thread,
};
use zeroize::Zeroize;

const FIELD_HEIGHT: f32 = 36.0;

enum Event {
    Probe(Ipv4Addr, String, Result<Probe, String>),
    Execute(Outcome),
}
pub struct FrameUi {
    scan: Option<Scan>,
    networks: Vec<Network>,
    large_scan: bool,
    candidates: Vec<Candidate>,
    ip: String,
    username: String,
    password: String,
    sudo_password: String,
    receiver: Option<Receiver<Event>>,
    preview: Option<(Ipv4Addr, String, Probe)>,
    trust_new: bool,
    ip_help: bool,
    status: String,
    status_error: bool,
    lines: Vec<String>,
    exported: Vec<String>,
    demo: bool,
}
impl Drop for FrameUi {
    fn drop(&mut self) {
        self.password.zeroize();
        self.sudo_password.zeroize();
        if let Some(scan) = &self.scan {
            scan.cancel.store(true, Ordering::Relaxed);
        }
    }
}
impl FrameUi {
    pub fn new(demo: bool) -> Self {
        let mut this = Self {
            scan: None,
            networks: vec![],
            large_scan: false,
            candidates: vec![],
            ip: String::new(),
            username: "steamos".into(),
            password: String::new(),
            sudo_password: String::new(),
            receiver: None,
            preview: None,
            trust_new: false,
            ip_help: false,
            status: String::new(),
            status_error: false,
            lines: vec![],
            exported: vec![],
            demo,
        };
        if demo {
            this.status = t(
                "演示模式：不会连接网络或修改头显。",
                "Demo mode: no network connection or headset changes.",
            )
            .into();
        } else {
            this.scan();
        }
        this
    }
    fn scan(&mut self) {
        if let Some(old) = &self.scan {
            old.cancel.store(true, Ordering::Relaxed);
        }
        self.scan = None;
        self.candidates.clear();
        self.large_scan = false;
        self.status_error = false;
        match frame::networks() {
            Ok(nets) if nets.is_empty() => {
                self.status = t(
                    "未找到活动的局域网 IPv4 网段，请手动输入 Frame IP。",
                    "No active local IPv4 subnet found. Enter the Frame IP manually.",
                )
                .into()
            }
            Ok(nets) => {
                let count: u64 = nets.iter().map(|n| n.host_count()).sum();
                self.networks = nets;
                if count > 4096 {
                    self.large_scan = true;
                    self.status = if i18n::is_english() {
                        format!("This subnet has {count} addresses. Scanning may take a while.")
                    } else {
                        format!("当前网段包含 {count} 个地址，扫描可能较慢。请选择是否继续。")
                    };
                } else {
                    self.start_scan();
                }
            }
            Err(e) => {
                self.status = if i18n::is_english() {
                    format!("Scan unavailable: {e}. Enter the IP manually.")
                } else {
                    format!("扫描不可用：{e}。可手动输入 IP。")
                }
            }
        }
    }
    fn start_scan(&mut self) {
        self.large_scan = false;
        self.status_error = false;
        self.scan = Some(frame::start_scan(self.networks.clone()));
        self.status = t(
            "正在扫描局域网中名为 frame 的设备…",
            "Scanning the LAN for devices named frame…",
        )
        .into();
    }
    fn poll(&mut self) {
        if let Some(scan) = &self.scan {
            while let Ok(item) = scan.results.try_recv() {
                if let Some(old) = self.candidates.iter_mut().find(|old| old.ip == item.ip) {
                    old.ssh_open |= item.ssh_open;
                } else {
                    self.candidates.push(item);
                }
            }
            if scan.done.load(Ordering::Acquire) {
                let summary = if scan.cancel.load(Ordering::Relaxed) {
                    t(
                        "扫描已取消，可手动输入 IP。",
                        "Scan cancelled. Enter an IP manually.",
                    )
                    .into()
                } else if self.candidates.is_empty() {
                    t(
                        "未找到主机名为 frame 的设备，请手动输入 IP。",
                        "No device named frame found. Enter the IP manually.",
                    )
                    .into()
                } else if i18n::is_english() {
                    format!(
                        "Scan complete: {} candidate(s) found.",
                        self.candidates.len()
                    )
                } else {
                    format!("扫描完成：找到 {} 个候选设备。", self.candidates.len())
                };
                if self.receiver.is_none() && self.lines.is_empty() {
                    self.status = summary;
                }
                self.scan = None;
            }
        }
        match self.receiver.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(Event::Probe(ip, username, result))) => {
                self.receiver = None;
                match result {
                    Ok(probe)
                        if self.ip.trim() == ip.to_string() && self.username.trim() == username =>
                    {
                        self.trust_new = !probe.new_host;
                        self.preview = Some((ip, username, probe));
                        self.status_error = false;
                        self.status = t(
                            "请核对主机密钥和命令后确认。",
                            "Check the host key and commands before confirming.",
                        )
                        .into();
                    }
                    Ok(_) => self.error(
                        t(
                            "连接信息已变化，请重新确认。",
                            "Connection details changed. Please confirm again.",
                        )
                        .into(),
                    ),
                    Err(e) => self.error(e),
                }
            }
            Some(Ok(Event::Execute(result))) => {
                self.receiver = None;
                self.sudo_password.zeroize();
                self.status = if result.success {
                    t(
                        "头显设置完成；请稍后自行重启并复查。",
                        "Headset setup complete. Restart it yourself and verify again.",
                    )
                    .into()
                } else if result.sudo_auth_failed {
                    t("sudo 验证失败；如密码不同，请填写独立 sudo 密码，并重新输入 SSH 密码。", "sudo authentication failed. If its password differs, enter it separately and re-enter the SSH password.").into()
                } else {
                    t(
                        "操作未完成，请查看下方步骤。",
                        "Operation incomplete. Review the steps below.",
                    )
                    .into()
                };
                self.status_error = !result.success;
                for line in result.lines {
                    self.exported.push(line.clone());
                    self.lines.push(line);
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.receiver = None;
                self.error(
                    t(
                        "连接线程意外结束；结果未知，请在头显上检查。",
                        "Connection thread ended unexpectedly. Result unknown; check the headset.",
                    )
                    .into(),
                );
            }
            _ => {}
        }
    }
    fn error(&mut self, error: String) {
        self.status = if i18n::is_english() {
            "Headset operation failed. See the diagnostic log below.".into()
        } else {
            error.clone()
        };
        self.status_error = true;
        self.lines.push(format!("[ERROR] {error}"));
        self.exported.push(format!("[ERROR] {error}"));
    }
    fn probe(&mut self) {
        let Ok(ip) = self.ip.trim().parse::<Ipv4Addr>() else {
            self.error(t("请输入有效的 IPv4 地址。", "Enter a valid IPv4 address.").into());
            return;
        };
        if self.username.trim().is_empty() || self.password.is_empty() {
            self.error(
                t(
                    "请填写用户名和 SSH 密码。",
                    "Enter a username and SSH password.",
                )
                .into(),
            );
            return;
        }
        self.status = t(
            "正在读取 SSH 主机密钥；尚未发送密码…",
            "Reading SSH host key; password not sent yet…",
        )
        .into();
        self.status_error = false;
        let username = self.username.trim().to_owned();
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(Event::Probe(ip, username, frame::probe(ip)));
        });
    }
    fn execute(&mut self) {
        let Some((ip, username, preview)) = self.preview.take() else {
            return;
        };
        if self.ip.trim() != ip.to_string() || self.username.trim() != username {
            self.error(
                t(
                    "连接信息已变化，请重新确认。",
                    "Connection details changed. Please confirm again.",
                )
                .into(),
            );
            return;
        }
        let credentials = Credentials {
            username,
            password: std::mem::take(&mut self.password),
            sudo_password: std::mem::take(&mut self.sudo_password),
        };
        self.status = t(
            "正在通过 SSH 设置并复查…",
            "Applying settings over SSH and verifying…",
        )
        .into();
        self.status_error = false;
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(Event::Execute(frame::execute(
                ip,
                &preview.fingerprint,
                credentials,
            )));
        });
    }
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.exported)
    }
    pub fn busy(&self) -> bool {
        self.receiver.is_some()
    }
    fn device_card(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("发现头显", "Find headset")).size(16.0).strong().color(INK));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!self.demo && !self.busy(), egui::Button::new(t("重新扫描", "Scan again")))
                        .clicked()
                    {
                        self.scan();
                    }
                });
            });
            if let Some(scan) = &self.scan {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(if i18n::is_english() { format!("Scanning  {}/{}", scan.progress.load(Ordering::Relaxed), scan.total) } else { format!("正在扫描  {}/{}", scan.progress.load(Ordering::Relaxed), scan.total) });
                    if ui.small_button(t("取消", "Cancel")).clicked() {
                        scan.cancel.store(true, Ordering::Relaxed);
                    }
                });
            }
            if self.large_scan {
                ui.horizontal(|ui| {
                    ui.colored_label(MUTED, t("当前网段较大，扫描可能需要一些时间。", "Large subnet; scanning may take a while."));
                    if ui.button(t("继续扫描", "Continue scan")).clicked() {
                        self.start_scan();
                    }
                    if ui.button(t("手动输入", "Enter IP manually")).clicked() {
                        self.large_scan = false;
                    }
                });
            }
            if self.scan.is_none()
                && self.candidates.is_empty()
                && !self.large_scan
                && !self.status_error
                && self.receiver.is_none()
                && self.lines.is_empty()
                && !self.demo
            {
                ui.colored_label(MUTED, &self.status);
            }
            for candidate in &self.candidates {
                let label = format!(
                    "frame  ·  {}  ·  SSH {}",
                    candidate.ip,
                    if candidate.ssh_open {
                        t("可连接", "open")
                    } else {
                        t("不可连接", "closed")
                    }
                );
                if ui
                    .add_sized(
                        [ui.available_width(), 36.0],
                        egui::Button::selectable(self.ip == candidate.ip.to_string(), label),
                    )
                    .on_hover_text(t("仅按主机名筛选；执行前请核对 SSH 主机密钥指纹。", "Filtered by hostname only. Verify the SSH host key fingerprint before running commands."))
                    .clicked()
                {
                    self.ip = candidate.ip.to_string();
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("头显 IP 地址", "Headset IP address")).strong().color(INK));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.link(t("如何查看 Frame IP", "How to find the Frame IP")).clicked() {
                        self.ip_help = true;
                    }
                });
            });
            ui.add_enabled(
                !self.busy(),
                egui::TextEdit::singleline(&mut self.ip)
                    .desired_width(f32::INFINITY)
                    .min_size(egui::vec2(0.0, FIELD_HEIGHT))
                    .vertical_align(egui::Align::Center)
                    .hint_text(t("例如 192.168.1.20", "e.g. 192.168.1.20")),
            );
        });
    }
    fn credentials_card(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(t("连接信息", "Connection details")).size(16.0).strong().color(INK));
            ui.add_space(4.0);
            ui.columns(2, |columns| {
                columns[0].label(RichText::new(t("用户名", "Username")).strong().color(INK));
                columns[0].add_enabled(
                    !self.busy(),
                    egui::TextEdit::singleline(&mut self.username)
                        .desired_width(f32::INFINITY)
                        .min_size(egui::vec2(0.0, FIELD_HEIGHT))
                        .vertical_align(egui::Align::Center),
                );
                columns[1].label(RichText::new(t("SSH 密码", "SSH password")).strong().color(INK));
                columns[1].add_enabled(
                    !self.busy(),
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .desired_width(f32::INFINITY)
                        .min_size(egui::vec2(0.0, FIELD_HEIGHT))
                        .vertical_align(egui::Align::Center),
                );
            });
            ui.label(
                RichText::new(t("程序不会收集、保存或上传你的密码；仅在本机内存中用于本次 SSH／sudo 验证，不写入日志。", "Your password is not collected, saved, uploaded, or logged. It is used in memory for this SSH/sudo session only."))
                    .small()
                    .color(MUTED),
            );
            egui::CollapsingHeader::new(t("sudo 密码与 SSH 密码不同？", "Different sudo password?"))
                .show(ui, |ui| {
                    ui.add_enabled(
                        !self.busy(),
                        egui::TextEdit::singleline(&mut self.sudo_password)
                            .password(true)
                            .desired_width(f32::INFINITY)
                            .min_size(egui::vec2(0.0, FIELD_HEIGHT))
                            .vertical_align(egui::Align::Center)
                            .hint_text(t("可选：输入单独的 sudo 密码", "Optional: enter a separate sudo password")),
                    );
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.hyperlink_to(t("如何开启 SSH", "How to enable SSH"), if i18n::is_english() { frame::SSH_HELP_EN } else { frame::SSH_HELP });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary_button(ui, t("连接并预览命令", "Connect and preview commands"), !self.busy() && !self.demo).clicked()
                    {
                        self.probe();
                    }
                });
            });
        });
    }
    fn status_card(&self, ui: &mut egui::Ui) {
        if self.status.is_empty() && self.lines.is_empty() {
            return;
        }
        card()
            .inner_margin(egui::Margin::symmetric(16, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if self.receiver.is_some() {
                        ui.spinner();
                    }
                    if !self.status.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&self.status).color(if self.status_error {
                                    RED
                                } else {
                                    MUTED
                                }),
                            )
                            .wrap(),
                        );
                    }
                });
                if !self.lines.is_empty() {
                    egui::ScrollArea::vertical()
                        .max_height(150.0)
                        .show(ui, |ui| {
                            for line in &self.lines {
                                ui.colored_label(
                                    if line.starts_with("[ERROR]") {
                                        RED
                                    } else {
                                        INK
                                    },
                                    line,
                                );
                            }
                        });
                }
            });
    }
    fn page_content(&mut self, ui: &mut egui::Ui) {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("头显设置", "Headset settings"))
                    .size(24.0)
                    .strong()
                    .color(INK),
            );
            ui.label(
                RichText::new(t(
                    "连接 Frame，设置无线监管区域",
                    "Connect to Frame and set its wireless region",
                ))
                .size(14.0)
                .color(MUTED),
            );
        });
        ui.add_space(6.0);
        self.device_card(ui);
        ui.add_space(10.0);
        self.credentials_card(ui);
        if self.status_error || self.receiver.is_some() || !self.lines.is_empty() || self.demo {
            ui.add_space(10.0);
            self.status_card(ui);
        }
    }
    pub fn draw(&mut self, ctx: &egui::Context) -> bool {
        self.poll();
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && self.busy() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.status = t(
                "远程操作尚未结束，请稍候再关闭窗口。",
                "Remote operation still running. Wait before closing.",
            )
            .into();
        }
        let close = close_requested && !self.busy();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(246, 248, 251))
                    .inner_margin(20.0),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.page_content(ui));
            });
        if self.ip_help {
            egui::Modal::new(egui::Id::new("frame-ip-help"))
                .frame(card().inner_margin(24.0))
                .show(ctx, |ui| {
                    ui.set_width(360.0);
                    ui.label(
                        RichText::new(t("查看 Frame IP", "Find the Frame IP"))
                            .size(20.0)
                            .strong()
                            .color(INK),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        t("在 Frame 的 Wi-Fi 设置中，点开当前已连接的 Wi-Fi，即可查看 IP 地址。", "On Frame, open Wi-Fi settings and select the connected network to see its IP address."),
                    );
                    ui.add_space(16.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary_button(ui, t("知道了", "OK"), true).clicked() {
                            self.ip_help = false;
                        }
                    });
                });
        }
        if let Some((_, _, preview)) = &self.preview {
            let new_host = preview.new_host;
            let fingerprint = preview.fingerprint.clone();
            egui::Modal::new(egui::Id::new("frame-command-confirm"))
                .frame(card().inner_margin(20.0))
                .show(ctx, |ui| {
                ui.set_width(540.0);
                ui.label(RichText::new(t("确认头显设置", "Confirm headset setup")).size(21.0).strong().color(INK));
                ui.label(
                    RichText::new(t("请先确认连接目标和主机密钥，再执行远程命令。", "Verify the target and host key before running remote commands."))
                        .color(MUTED),
                );
                ui.add_space(12.0);
                egui::Frame::new()
                    .fill(Color32::from_rgb(246, 248, 251))
                    .corner_radius(8)
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(t("连接目标", "Target")).small().color(MUTED));
                        ui.label(
                            RichText::new(format!("{}@{}", self.username.trim(), self.ip.trim()))
                                .strong()
                                .color(INK),
                        );
                        ui.add_space(6.0);
                        ui.label(RichText::new(t("SSH 主机密钥指纹", "SSH host key fingerprint")).small().color(MUTED));
                        ui.add(
                            egui::Label::new(
                                RichText::new(&fingerprint)
                                    .monospace()
                                    .size(12.0)
                                    .color(INK),
                            )
                            .wrap()
                            .selectable(true),
                        );
                    });
                if new_host {
                    ui.add_space(8.0);
                    egui::Frame::new()
                        .fill(Color32::from_rgb(239, 246, 255))
                        .stroke(egui::Stroke::new(1.0, BLUE))
                        .corner_radius(8)
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.scope(|ui| {
                                let checked = self.trust_new;
                                let style = ui.style_mut();
                                style.spacing.icon_width = 22.0;
                                for visuals in [
                                    &mut style.visuals.widgets.inactive,
                                    &mut style.visuals.widgets.hovered,
                                    &mut style.visuals.widgets.active,
                                ] {
                                    visuals.bg_fill = if checked { BLUE } else { Color32::WHITE };
                                    visuals.bg_stroke = egui::Stroke::new(2.0, BLUE);
                                    visuals.fg_stroke = egui::Stroke::new(
                                        2.5,
                                        if checked { Color32::WHITE } else { BLUE },
                                    );
                                    visuals.corner_radius = egui::CornerRadius::same(4);
                                }
                                ui.add_sized(
                                    [ui.available_width(), 40.0],
                                    egui::Checkbox::new(
                                        &mut self.trust_new,
                                        RichText::new(t("首次连接：我已核对并信任上方指纹", "First connection: I verified and trust this fingerprint"))
                                            .strong()
                                            .color(INK),
                                    ),
                                );
                            });
                        });
                } else {
                    ui.add_space(6.0);
                    ui.label(RichText::new(t("主机密钥与之前信任的记录一致。", "Host key matches the trusted record.")).small().color(MUTED));
                }
                ui.add_space(12.0);
                ui.label(RichText::new(t("本次操作", "This operation")).strong().color(INK));
                ui.label(t("验证 sudo 权限，设置运行时 US，并检查永久配置与结果。", "Verify sudo access, set runtime US, and check the persistent setting and result."));
                ui.label(
                    RichText::new(t("若永久配置已是 US，不会重复修改文件；仍会重新应用一次运行时 US。不会自动重启。", "If the persistent setting is already US, the file is unchanged; runtime US is still applied once. No automatic reboot."))
                        .small()
                        .color(MUTED),
                );
                ui.add_space(8.0);
                ui.label(RichText::new(t("将执行的命令", "Commands to run")).strong().color(INK));
                egui::Frame::new()
                    .fill(Color32::from_rgb(248, 250, 252))
                    .stroke(egui::Stroke::new(1.0, crate::BORDER))
                    .corner_radius(7)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical().max_height(145.0).show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(t(frame::COMMANDS, frame::COMMANDS_EN))
                                        .monospace()
                                        .size(12.0)
                                        .color(INK),
                                )
                                .wrap()
                                .selectable(true),
                            );
                        });
                    });
                ui.label(
                    RichText::new(t("密码仅用于本次验证，不会出现在命令或日志中。", "The password is used only for this session and does not appear in commands or logs."))
                        .small()
                        .color(MUTED),
                );
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary_button(ui, t("确认执行", "Run commands"), self.trust_new).clicked() {
                        self.execute();
                    }
                    if ui.button(t("取消", "Cancel")).clicked() {
                        self.password.zeroize();
                        self.sudo_password.zeroize();
                        self.preview = None;
                    }
                });
            });
        }
        close
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_renders_for_first_and_returning_connection() {
        for new_host in [true, false] {
            let mut window = FrameUi::new(true);
            let ip = Ipv4Addr::new(192, 168, 5, 67);
            window.ip = ip.to_string();
            window.preview = Some((
                ip,
                "steamos".into(),
                Probe {
                    fingerprint: format!("SHA256:{}", "a".repeat(43)),
                    new_host,
                },
            ));
            window.trust_new = !new_host;
            let ctx = egui::Context::default();
            crate::setup_style(&ctx);
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(680.0, 650.0),
                    )),
                    ..Default::default()
                },
                |ctx| assert!(!window.draw(ctx)),
            );
            assert!(window.preview.is_some());
            assert_eq!(window.trust_new, !new_host);
        }
    }
    #[test]
    fn default_window_fits_without_scrolling() {
        let mut window = FrameUi::new(true);
        window.demo = false;
        window.status = "扫描完成：找到 1 个候选设备。".into();
        window.candidates.push(Candidate {
            ip: Ipv4Addr::new(192, 168, 5, 67),
            ssh_open: true,
        });
        window.status_error = true;
        window.status = "连接失败，请查看执行记录。".into();
        window.lines.push("[ERROR] 演示错误".into());
        let ctx = egui::Context::default();
        crate::setup_style(&ctx);
        let mut height = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(680.0, 650.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().inner_margin(20.0))
                    .show(ctx, |ui| {
                        let output = egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| window.page_content(ui));
                        height = Some((output.content_size.y, output.inner_rect.height()));
                    });
            },
        );
        let (content, available) = height.unwrap();
        assert!(
            content + 16.0 <= available,
            "page needs scrolling: content={content}, available={available}"
        );
    }
    #[test]
    fn demo_window_renders_without_scanning_or_retaining_password_after_close() {
        let mut window = FrameUi::new(true);
        window.ip_help = true;
        assert!(window.scan.is_none());
        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(560.0, 520.0),
                )),
                ..Default::default()
            },
            |ctx| assert!(!window.draw(ctx)),
        );
        window.password = "secret".into();
        drop(window);
    }
}
