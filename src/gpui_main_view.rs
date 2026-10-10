use gpui_kit::{prelude::*, *};
use gpui_kit::component::{Disableable, Icon, Root, Sizable, TitleBar, button::{Button, ButtonVariants}, checkbox::Checkbox, scroll::ScrollbarMode};
use gpui_kit::component::theme::{Theme, ThemeMode};
use std::sync::Arc;

const INK: u32 = 0x1f3445;
const MUTED: u32 = 0x526c7c;
const BORDER: u32 = 0x718c9b;
const ERROR: u32 = 0xb42318;

pub(crate) struct Dashboard {
    data: App,
    modal: Option<MainModal>,
    page: Page,
    frame: Option<Entity<crate::gpui_frame::FrameView>>,
    adapter_menu: bool,
    log_scroll: ScrollHandle,
}

#[derive(Clone, Copy)]
enum MainModal { SetUs, EnableAuto, DisableAuto, UpdateService, UpdateApp, DisablePower }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page { Local, Headset, Settings }

impl Dashboard {
    fn new(demo: bool, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| loop {
            let Some(this) = this.upgrade() else { break };
            let interval = this.update(cx, |view, cx| {
                view.data.poll();
                view.data.poll_service();
                view.data.poll_update();
                if let Some(line) = view.data.settings.poll() { view.data.record(&line); }
                if let Some(line) = view.data.settings.poll_multilink() { view.data.record(&line); }
                for line in crate::gpui_frame::take_shared_logs() { view.data.record(&line); }
                if let Some(error) = view.data.error_popup.take() {
                    view.data.status = error;
                }
                cx.notify();
                if view.data.refreshing { Duration::from_millis(16) } else { Duration::from_millis(180) }
            });
            cx.background_executor().timer(interval).await;
        }).detach();
        Self { data: App::new(demo), modal: None, page: Page::Local, frame: None, adapter_menu: false, log_scroll: ScrollHandle::new() }
    }
    fn show_headset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.frame.is_none() {
            self.frame = Some(cx.new(|cx| crate::gpui_frame::FrameView::new(self.data.demo, window, cx)));
        }
        self.page = Page::Headset;
        self.adapter_menu = false;
        self.modal = None;
        cx.notify();
    }
    fn show_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(frame) = &self.frame {
            frame.update(cx, |view, cx| view.cancel_preview(window, cx));
        }
        self.page = Page::Local;
        self.adapter_menu = false;
        cx.notify();
    }
    fn clear_headset_secrets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(frame) = &self.frame {
            frame.update(cx, |view, cx| view.cancel_preview(window, cx));
        }
    }
    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.modal.take() else { return };
        match modal {
            MainModal::SetUs => {
                if let Some(adapter) = self.data.selected.and_then(|i| self.data.adapters.get(i)).cloned() {
                    self.data.start(adapter, true);
                }
            }
            MainModal::EnableAuto => self.data.set_auto(true),
            MainModal::DisableAuto => self.data.set_auto(false),
            MainModal::UpdateService => self.data.update_service(),
            MainModal::UpdateApp => {
                if let Some(release) = &self.data.available_update { cx.open_url(&release.page); }
            }
            MainModal::DisablePower => {
                let target = self.data.settings.target.take();
                self.data.settings.start(crate::settings::Action::DisablePower, target, self.data.demo);
            }
        }
        cx.notify();
    }
    fn card() -> Div {
        div().w_full().rounded_lg().border_1().border_color(rgb(BORDER))
            .bg(rgb(0xffffff)).p_4().flex().flex_col().gap_3()
    }
    fn label(text: impl Into<SharedString>) -> Div {
        div().text_color(rgb(INK)).child(text.into())
    }
    fn muted(text: impl Into<SharedString>) -> Div {
        div().text_color(rgb(MUTED)).text_sm().child(text.into())
    }
    fn settings_page(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        use crate::settings::Action;
        let busy = self.data.settings.busy();
        let multilink_busy = self.data.settings.multilink_busy();
        let adapter = self.data.selected.and_then(|i| self.data.adapters.get(i));
        let power = adapter.and_then(|a| self.data.settings.power.as_ref().filter(|(id, _)| *id == a.id)).map(|(_, enabled)| *enabled);
        let power_label = if busy { t("正在处理…", "Working…") } else { match power {
            Some(true) => t("节能已开启", "Power saving is on"),
            Some(false) => t("节能已关闭", "Power saving is off"),
            None => t("状态未读取或不可用", "Status unavailable"),
        }};
        let selected_adapter = adapter.map(|a| adapter_label(a, &self.data.adapters))
            .unwrap_or_else(|| t("请选择 USB 适配器", "Choose a USB adapter").into());
        let mut adapters = div().flex().flex_col().gap_1();
        for (index, item) in self.data.adapters.iter().enumerate() {
            let selected = self.data.selected == Some(index);
            let label = format!("{}{}", if selected { "● " } else { "○ " }, adapter_label(item, &self.data.adapters));
            adapters = adapters.child(Button::new(format!("settings-adapter-{index}"))
                .label(label).w_full().disabled(busy || self.data.receiver.is_some())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let adapter = this.data.adapters.get(index).cloned();
                    this.adapter_menu = false;
                    this.data.select(Some(index));
                    this.data.settings.start(Action::ReadPower, adapter, this.data.demo);
                    cx.notify();
                })));
        }
        if self.data.adapters.is_empty() {
            adapters = adapters.child(Self::muted(t("未发现适配器，请在本机设置中刷新设备。", "No adapter found. Refresh devices on the This PC tab.")));
        }
        div().id("settings-content").size_full().overflow_y_scroll().p_4().flex().flex_col().gap_3()
            .child(Self::card().flex_shrink_0()
                .child(Self::label(t("快捷方式", "Shortcuts")).text_lg())
                .child(Self::muted(t("为当前用户创建快捷方式，指向当前程序。创建后请保持程序位置不变。", "Create shortcuts for your Windows account, linked to this copy of the app. Keep the app in its current location.")))
                .child(div().flex().items_center().justify_between()
                    .child(Self::label(t("桌面图标", "Desktop shortcut")))
                    .child(Button::new("create-desktop").label(t("创建", "Create")).disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| { this.data.settings.start(Action::Desktop, None, this.data.demo); cx.notify(); }))))
                .child(div().flex().items_center().justify_between()
                    .child(Self::label(t("开始菜单快捷方式", "Start menu shortcut")))
                    .child(div().flex().gap_2()
                        .child(Button::new("create-start-menu").label(t("创建", "Create")).disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| { this.data.settings.start(Action::StartMenu, None, this.data.demo); cx.notify(); })))
                        .child(Button::new("remove-start-menu").label(t("卸载", "Remove")).disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| { this.data.settings.start(Action::RemoveStartMenu, None, this.data.demo); cx.notify(); }))))))
            .child(Self::card().flex_shrink_0()
                .child(div().flex().items_center().justify_between()
                    .child(Self::label(t("程序更新", "App updates")).text_lg())
                    .child(Button::new("check-updates").label(if self.data.update_receiver.is_some() { t("检查中…", "Checking…") } else { t("检查更新", "Check for updates") })
                        .disabled(self.data.update_receiver.is_some() || busy)
                        .on_click(cx.listener(|this, _, _, cx| { this.data.check_updates(true); cx.notify(); }))))
                .child(Self::muted(format!("{} v{}", t("当前版本", "Current version:"), env!("CARGO_PKG_VERSION"))))
                .when(!self.data.update_status.is_empty(), |card| card.child(Self::muted(self.data.update_status.clone())))
                .child(Self::muted(t("发现更新后，打开 Release 页面自行下载替换；不会自动覆盖程序。", "When an update is available, download it from the release page and replace the app manually.")))
                .when(self.data.available_update.is_some(), |card| card.child(Button::new("settings-release").label(t("打开 Release 页面 ↗", "Open release page ↗")).primary()
                    .on_click(cx.listener(|this, _, _, cx| { if let Some(release) = &this.data.available_update { cx.open_url(&release.page); } })))))
            .child(Self::card().flex_shrink_0()
                .child(Self::label(t("网卡节能", "Adapter power saving")).text_lg())
                .child(div().w_full().relative()
                    .child(Button::new("settings-adapter-select").label(format!("{selected_adapter}  ▾")).w_full()
                        .disabled(busy || self.data.receiver.is_some())
                        .on_click(cx.listener(|this, _, _, cx| { this.adapter_menu = !this.adapter_menu; cx.notify(); })))
                    .when(self.adapter_menu, |area| area.child(deferred(
                        div().absolute().top_full().left_0().w_full().mt_1().rounded_md()
                            .border_1().border_color(rgb(BORDER)).bg(rgb(0xffffff)).p_1().shadow_md()
                            .occlude().child(adapters)))))
                .child(Self::muted(t("对应设备管理器 → 网卡属性 → 电源管理中的“允许计算机关闭此设备以节约电源”。关闭后可能增加耗电，不会修改高级属性或全局电源计划。", "Controls ‘Allow the computer to turn off this device to save power’ in Device Manager → adapter Properties → Power Management. Turning it off may increase power usage; advanced properties and the power plan are unchanged.")))
                .child(div().flex().items_center().justify_between().gap_2()
                    .child(Self::label(power_label))
                    .child(div().flex().gap_2()
                        .child(Button::new("read-power").label(t("刷新状态", "Refresh status")).disabled(busy || adapter.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let adapter = this.data.selected.and_then(|i| this.data.adapters.get(i)).cloned();
                                this.data.settings.start(Action::ReadPower, adapter, this.data.demo); cx.notify();
                            })))
                        .child(Button::new("disable-power").label(t("关闭节能", "Turn off")).primary()
                            .disabled(busy || power != Some(true) || self.data.receiver.is_some())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.data.settings.busy() || this.data.receiver.is_some() { return; }
                                this.data.settings.target = this.data.selected.and_then(|i| this.data.adapters.get(i)).cloned();
                                this.modal = Some(MainModal::DisablePower); cx.notify();
                            })))))
                .when_some(self.data.settings.power_error.as_ref(), |card, error| card.child(Self::muted(error.clone()).text_color(rgb(ERROR)))))
            .child(Self::card().flex_shrink_0()
                .child(Self::label(t("多链路串流", "Multi-link streaming")).text_lg())
                .child(Self::muted(t("允许 SteamVR 同时使用 Frame USB 适配器和普通 Wi-Fi。请先关闭 SteamVR；修改前会备份配置，重新启动 SteamVR 后生效。", "Allows SteamVR to stream over both the Frame USB adapter and regular Wi-Fi. Close SteamVR before changing it; the previous config is backed up. Restart SteamVR to apply.")))
                .child(div().flex().items_center().justify_between().gap_2()
                    .child(Self::label(if multilink_busy { t("正在读取或设置…", "Working…") } else { match self.data.settings.multilink {
                        Some(true) => t("已保存：开启", "Saved: On"), Some(false) => t("已保存：关闭", "Saved: Off"), None => t("状态未读取或不可用", "Status unavailable"),
                    }}))
                    .child(div().flex().gap_2()
                        .child(Button::new("refresh-multilink").label(t("刷新状态", "Refresh status")).disabled(multilink_busy)
                            .on_click(cx.listener(|this, _, _, cx| { this.data.settings.start_multilink(None, this.data.demo); cx.notify(); })))
                        .child(Button::new("toggle-multilink").label(if self.data.settings.multilink == Some(true) { t("关闭", "Turn off") } else { t("开启", "Turn on") })
                            .primary().disabled(multilink_busy || self.data.settings.multilink.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(enabled) = this.data.settings.multilink {
                                    this.data.settings.start_multilink(Some(!enabled), this.data.demo); cx.notify();
                                }
                            }))))
                .when_some(self.data.settings.multilink_error.as_ref(), |card, error| card.child(Self::muted(error.clone()).text_color(rgb(ERROR))))))
    }
    fn log_panel(&self, cx: &mut Context<Self>) -> Div {
        let mut log_rows = Vec::new();
        for &index in &self.data.visible_rows {
            let row = &self.data.rows[index];
            let source = if i18n::is_english() {
                if row.source == ui_log::Source::Service { "Service" } else { "App" }
            } else { row.source.label() };
            log_rows.push((format!("{}  {}  {}", ui_log::local_time(&row.time), source, row.text), row.level == Level::Error));
        }
        let filter = |active| div().rounded_md().bg(rgb(if active { 0xffffff } else { 0xf0f4f7 }));
        Self::card().min_h(px(0.))
            .child(Self::label(t("运行记录", "Activity")).text_lg())
            .child(div().flex().items_center().gap_3()
                .child(div().rounded_md().bg(rgb(0xf0f4f7)).p_1().flex().gap_1()
                    .child(filter(self.data.filter == Filter::All).child(Button::new("all-logs").label(t("全部", "All")).ghost()
                        .on_click(cx.listener(|this, _, _, cx| { this.data.filter = Filter::All; this.data.logs_dirty = true; cx.notify(); }))))
                    .child(filter(self.data.filter == Filter::Program).child(Button::new("app-logs").label(t("程序", "App")).ghost()
                        .on_click(cx.listener(|this, _, _, cx| { this.data.filter = Filter::Program; this.data.logs_dirty = true; cx.notify(); }))))
                    .child(filter(self.data.filter == Filter::Service).child(Button::new("service-logs").label(t("服务", "Service")).ghost()
                        .on_click(cx.listener(|this, _, _, cx| { this.data.filter = Filter::Service; this.data.logs_dirty = true; cx.notify(); })))))
                .child(div().flex_1())
                .child(Checkbox::new("errors-only").label(t("仅错误", "Errors only")).small().text_color(rgb(MUTED))
                    .checked(self.data.errors_only)
                    .on_change(cx.listener(|this, checked, _, cx| { this.data.errors_only = *checked; this.data.logs_dirty = true; cx.notify(); })))
                .child(Button::new("export-log").label(t("导出日志", "Export log")).ghost().small().text_color(rgb(MUTED))
                    .on_click(cx.listener(|this, _, _, cx| { this.data.save(); cx.notify(); }))))
            .child(crate::gpui_log::pane("log-scroll", &self.log_scroll,
                crate::gpui_log::text("activity-text", log_rows)))
    }
    fn modal_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        if let Some(notice) = &self.data.settings.notice {
            let (message, failed) = match notice { Ok(message) => (message, false), Err(message) => (message, true) };
            return Some(div().absolute().inset_0().occlude().bg(rgba(0x1f3445b0)).flex().items_center().justify_center()
                .child(Self::card().w(px(500.)).gap_4()
                    .child(Self::label(if failed { t("操作未完成", "Unable to complete") } else { t("提示", "Done") }).text_xl())
                    .child(Self::label(message.clone()).text_color(rgb(if failed { ERROR } else { INK })))
                    .child(div().flex().justify_end().child(Button::new("dismiss-settings-notice").label(t("知道了", "OK")).primary()
                        .on_click(cx.listener(|this, _, _, cx| { this.data.settings.notice = None; cx.notify(); }))))));
        }
        let modal = self.modal?;
        let (title, detail) = match modal {
            MainModal::SetUs => (t("设置 US？", "Set this adapter to US?"), t("只向当前适配器提交一次设置，然后复查状态。", "Send the setting once, then check the adapter again.")),
            MainModal::EnableAuto => (t("开启自动应用？", "Turn on automatic restore?"), t("安装 Windows 服务；需要管理员权限。", "This installs a Windows service and requires administrator access.")),
            MainModal::DisableAuto => (t("关闭自动应用？", "Turn off automatic restore?"), t("卸载服务，保留日志。", "The service will be removed; logs will be kept.")),
            MainModal::UpdateService if self.data.auto_state.is_some_and(|s| s.needs_repair) => (t("修复自动应用服务？", "Repair automatic restore?"), t("服务程序副本缺失。核对服务归属后卸载并重装；不会接管未知文件或解除暂停保护。", "The service executable is missing. The app will verify the service, then remove and reinstall it without adopting unknown files or clearing a safety pause.")),
            MainModal::UpdateService => (t("更新服务副本？", "Update the background service?"), t("卸载并重新安装服务，保留日志；不会解除暂停保护。", "Reinstall the service without deleting logs or clearing a safety pause.")),
            MainModal::UpdateApp => (t("发现新版本", "New version available"), t("打开对应 Release 页面。下载后请自行替换程序。", "Open the Release page. Download and replace the app manually.")),
            MainModal::DisablePower => (t("关闭网卡节能？", "Disable adapter power saving?"), t("仅取消此网卡的“允许计算机关闭此设备以节约电源”。可能增加耗电；需要管理员权限。不修改其他网卡或全局电源计划。", "Turn off ‘Allow the computer to turn off this device to save power’ for this adapter only. This may increase power usage and requires administrator access. Other adapters and the power plan are unchanged.")),
        };
        Some(div().absolute().inset_0().occlude().bg(rgba(0x1f3445b0)).flex().items_center().justify_center()
            .child(Self::card().w(px(470.)).gap_4()
                .child(Self::label(title).text_xl())
                .child(Self::muted(detail))
                .when(matches!(modal, MainModal::DisablePower), |card| card.child(Self::muted(
                    self.data.settings.target.as_ref().map(|a| format!("{} · {}", a.name, a.id)).unwrap_or_default())))
                .child(div().flex().gap_2().justify_end()
                    .child(Button::new("cancel-confirm").label(t("取消", "Cancel"))
                        .on_click(cx.listener(|this, _, _, cx| { this.modal = None; this.data.settings.target = None; cx.notify(); })))
                    .child(Button::new("accept-confirm").label(t("确认", "Confirm")).primary()
                        .on_click(cx.listener(|this, _, _, cx| this.confirm(cx)))))))
    }
}

impl Render for Dashboard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.data.logs_dirty { self.log_scroll.scroll_to_bottom(); }
        self.data.rebuild_logs();
        let mut adapters = div().flex().flex_col().gap_1();
        for (index, adapter) in self.data.adapters.iter().enumerate() {
            let selected = self.data.selected == Some(index);
            let text = format!("{}{}", if selected { "● " } else { "○ " }, adapter_label(adapter, &self.data.adapters));
            adapters = adapters.child(Button::new(format!("adapter-{index}")).label(text).w_full()
                .on_click(cx.listener(move |this, _, _, cx| { this.data.select(Some(index)); this.adapter_menu = false; cx.notify(); })));
        }
        if self.data.adapters.is_empty() { adapters = adapters.child(Self::muted(t("未发现适配器。", "No adapter found."))); }
        let selected_adapter = self.data.selected.and_then(|i| self.data.adapters.get(i))
            .map(|adapter| adapter_label(adapter, &self.data.adapters))
            .unwrap_or_else(|| t("请选择 USB 适配器", "Choose a USB adapter").into());
        let state = self.data.device_status.as_ref().map(status_summary)
            .unwrap_or_else(|| t("国家：—　6 GHz：未读取", "Country: —   6 GHz: not read").into());
        let driver_notice = self.data.selected.and_then(|i| self.data.adapters.get(i))
            .filter(|adapter| adapter.unverified_driver)
            .map(|_| t("驱动版本未验证，结果可能不适用。", "Driver version is unverified; results may differ."));
        let refresh_angle = if self.data.refreshing {
            (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_millis() % 840) as f32 * std::f32::consts::TAU / 840.0
        } else { 0.0 };
        let auto = self.data.auto_state.unwrap_or_default();
        let auto_label = if auto.needs_repair { t("需要修复", "Needs repair") }
            else if auto.installed { t("已开启", "Enabled") } else { t("未开启", "Off") };
        let service_detail = if auto.needs_repair { t("服务程序副本缺失，自动应用不可用。", "The service executable is missing; automatic restore is unavailable.") }
            else if auto.running { t("正在处理设备", "Processing an adapter") }
            else if auto.paused { t("已暂停，等待人工检查", "Paused for manual inspection") }
            else if auto.installed && !auto.enabled { t("已安装但未启用", "Installed but disabled") }
            else { t("开机、插拔时按需恢复；处理结束后退出。", "Restores on startup and reconnect, then exits.") };
        let titlebar = div().w_full().h(px(58.)).flex_shrink_0().bg(rgb(0xffffff))
            .flex().items_center()
            .child(div().id("chrome-drag").h_full().flex_1().flex().items_center().gap_3().pl_5()
                .child(img(Arc::new(Image::from_bytes(ImageFormat::Ico, include_bytes!("../assets/icon.ico").to_vec()))).size(px(34.)))
                .child(Self::label(crate::WINDOW_TITLE).text_lg())
                .child(Self::muted(format!("v{}", env!("CARGO_PKG_VERSION"))))
                .window_control_area(WindowControlArea::Drag))
            .child(Button::new("language").icon(Icon::default().data(include_bytes!("../assets/languages.svg"))).ghost()
                .accessibility_label(t("切换语言", "Switch language"))
                .tooltip(t("切换语言", "Switch language"))
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Err(error) = i18n::toggle() {
                        this.data.record(&format!("[ERROR] 无法保存语言设置：{error}"));
                    }
                    if let Some(frame) = &this.frame { frame.update(cx, |view, cx| view.language_changed(cx)); }
                    cx.notify();
                })))
            .child(Button::new("github").icon(Icon::default().data(include_bytes!("../assets/github.svg"))).ghost()
                .accessibility_label("GitHub").tooltip("GitHub")
                .on_click(cx.listener(|_, _, _, cx| cx.open_url(crate::frame::REPO))))
            .child(div().w(px(12.)))
            .child(Button::new("chrome-minimize").label("−").ghost().on_click(|_, window, _| window.minimize_window()))
            .child(Button::new("chrome-close").label("×").ghost().on_click(|_, window, _| window.remove_window()))
            .child(div().w(px(8.)));
        let tab = |selected| div().h_full().px_2().flex().items_center()
            .border_b_2().border_color(rgb(if selected { 0x176b91 } else { 0xffffff }))
            .text_color(rgb(if selected { INK } else { MUTED }));
        let tabs = div().w_full().h(px(48.)).flex_shrink_0().bg(rgb(0xffffff))
            .border_b_1().border_color(rgb(0xd8e4ea)).px_5().flex().gap_2()
            .child(tab(self.page == Page::Local)
                .child(Button::new("local-tab").label(t("本机设置", "This PC")).ghost()
                    .on_click(cx.listener(|this, _, window, cx| this.show_local(window, cx)))))
            .child(tab(self.page == Page::Headset)
                .child(Button::new("headset-tab").label(t("头显设置", "Frame headset")).ghost()
                    .on_click(cx.listener(|this, _, window, cx| this.show_headset(window, cx)))))
            .child(tab(self.page == Page::Settings)
                .child(Button::new("settings-tab").label(t("其他设置", "More settings")).ghost()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.clear_headset_secrets(window, cx);
                        this.modal = None;
                        this.adapter_menu = false;
                        this.page = Page::Settings;
                        let adapter = this.data.selected.and_then(|i| this.data.adapters.get(i)).cloned();
                        this.data.settings.start(crate::settings::Action::ReadPower, adapter, this.data.demo);
                        this.data.settings.start_multilink(None, this.data.demo);
                        cx.notify();
                    }))))
            .child(div().h_full().px_2().flex().items_center()
                .child(Button::new("guide-tab").label(t("使用教程 ↗", "User guide ↗")).ghost()
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.open_url(if i18n::is_english() {
                            "https://github.com/toorux/steam-frame-6ghz-tool/blob/main/README.en.md"
                        } else {
                            "https://github.com/toorux/steam-frame-6ghz-tool/blob/main/README.md"
                        });
                    }))));
        let device = Self::card()
            .child(div().flex().justify_between().items_center()
                .child(Self::label(t("USB 适配器", "USB adapter")).text_lg())
                .child(Button::new("refresh").icon(Icon::default().data(include_bytes!("../assets/refresh.svg")).rotate(radians(refresh_angle))).ghost()
                    .accessibility_label(t("刷新设备", "Refresh devices"))
                    .tooltip(t("刷新设备", "Refresh devices"))
                    .on_click(cx.listener(|this, _, _, cx| { this.adapter_menu = false; this.data.refresh(); cx.notify(); }))))
            .child(div().w_full().relative()
                .child(Button::new("adapter-select").label(format!("{selected_adapter}  ▾")).w_full()
                    .on_click(cx.listener(|this, _, _, cx| { this.adapter_menu = !this.adapter_menu; cx.notify(); })))
                .when(self.adapter_menu, |area| area.child(deferred(
                    div().absolute().top_full().left_0().w_full().mt_1().rounded_md()
                        .border_1().border_color(rgb(BORDER)).bg(rgb(0xffffff)).p_1().shadow_md()
                        .occlude().child(adapters)))))
            .child(div().flex().items_center().justify_between().gap_3()
                .child(Self::label(state))
                .child(Button::new("set-us").label(t("设置 US", "Set US")).primary()
                    .disabled(self.data.selected.is_none() || self.data.receiver.is_some())
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.data.selected.is_some() && this.data.receiver.is_none() { this.modal = Some(MainModal::SetUs); cx.notify(); }
                    }))))
            .when_some(driver_notice, |card, notice| card.child(Self::muted(notice)));
        let automation = Self::card()
            .child(div().flex().items_center().gap_3()
                .child(Self::label(t("自动应用", "Automatic restore")).text_lg())
                .child(Self::label(auto_label).text_color(rgb(if auto.paused || auto.needs_repair { ERROR } else { INK })))
                .child(div().flex_1())
                .child(Button::new("toggle-auto").label(if auto.installed { t("关闭并卸载", "Turn off") } else { t("开启自动应用", "Turn on") })
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Some(if this.data.auto_state.is_some_and(|s| s.installed) { MainModal::DisableAuto } else { MainModal::EnableAuto }); cx.notify(); })))
                .when(auto.needs_update || auto.needs_repair, |row| row.child(Button::new("update-service").label(if auto.needs_repair { t("修复服务", "Repair service") } else { t("更新服务", "Update service") }).primary()
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Some(MainModal::UpdateService); cx.notify(); })))))
            .child(Self::muted(if auto.needs_repair || auto.installed && auto.running { service_detail } else {
                t("开启后，程序将会在必要时自动重设国家码，避免重启等情况导致国家码恢复", "When enabled, the service restores US when needed, including after a restart or reconnect.")
            }))
            .when(auto.exit_code != 0, |card| card.child(Self::muted(format!("{}: {}", t("服务退出码", "Service exit code"), auto.exit_code))))
            .when(auto.paused, |card| card.child(div().text_color(rgb(ERROR)).child(t("已暂停，请查看日志。", "Paused; check the log."))));
        let mut content = div().id("local-content").w_full().flex_1().min_h(px(0.)).overflow_y_scroll().p_4().flex().flex_col().gap_3()
            .child(device)
            .child(automation)
            .child(div().text_color(rgb(if self.data.status_level == Level::Error { ERROR } else { MUTED })).child(self.data.status.clone()))
            .child(self.log_panel(cx).h(px(390.)).flex_shrink_0());
        if let Some(release) = &self.data.available_update {
            content = content.child(Button::new("new-version").label(format!("{} {}", t("发现新版本", "New version"), release.version))
                .on_click(cx.listener(|this, _, _, cx| { this.modal = Some(MainModal::UpdateApp); cx.notify(); })));
        }
        let page = match self.page {
            Page::Local => div().size_full().child(content).into_any_element(),
            Page::Headset => div().size_full()
                .child(self.frame.as_ref().expect("headset page initialized").clone()).into_any_element(),
            Page::Settings => self.settings_page(cx).into_any_element(),
        };
        let mut view = div().size_full().relative().bg(rgb(0xf7fafc)).text_color(rgb(INK))
            .flex().flex_col()
            .child(titlebar)
            .child(tabs)
            .child(div().flex_1().min_h(px(0.)).child(page));
        if let Some(modal) = self.modal_view(cx) { view = view.child(modal); }
        view
    }
}

pub(crate) fn open(demo: bool) {
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Light, None, cx);
        let theme = Theme::global_mut(cx);
        let colors = &mut theme.colors;
        colors.button_primary = rgb(0x176b91).into();
        colors.button_primary_hover = rgb(0x125577).into();
        colors.button_primary_active = rgb(0x0f4663).into();
        colors.button_primary_foreground = rgb(0xffffff).into();
        colors.border = rgb(BORDER).into();
        theme.tokens = theme.colors.into();
        Theme::set_scrollbar_mode(ScrollbarMode::Always, cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(720.), px(840.)), cx))),
            is_resizable: false,
            titlebar: Some(TitlebarOptions { title: Some(crate::WINDOW_TITLE.into()), ..TitleBar::title_bar_options() }),
            ..TitleBar::window_options()
        };
        cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| Dashboard::new(demo, cx));
            let closing_view = view.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                if let Some(view) = closing_view.upgrade() {
                    view.update(cx, |view, cx| view.clear_headset_secrets(window, cx));
                }
                true
            });
            cx.new(|cx| Root::new(view, window, cx))
        }).expect("could not open GPUI window");
    });
}

#[cfg(test)]
mod interaction_tests {
    use super::{App, Dashboard, MainModal, Page, demo_adapter};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, ScrollHandle, TestAppContext, px, size};
    static UI_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[gpui_kit::test]
    fn settings_fit_both_languages_and_power_cancel_is_safe(cx: &mut TestAppContext) {
        let _lock = UI_TEST_LOCK.lock().unwrap();
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(720.), px(840.)), |_, _| {
            let mut data = App::empty(true);
            data.adapters.push(demo_adapter());
            data.selected = Some(0);
            data.settings.power = Some((demo_adapter().id, true));
            Dashboard { data, modal: None, page: Page::Settings, frame: None, adapter_menu: false, log_scroll: ScrollHandle::new() }
        });
        cx.update_window(handle.into(), |_, window, cx| {
            for _ in 0..2 {
                window.render_frame(cx);
                for id in ["settings-tab", "guide-tab", "create-desktop", "remove-start-menu", "check-updates", "settings-adapter-select", "disable-power"] {
                    let element = window.find(id);
                    assert!(element.visible(), "{id} must be visible");
                    assert!(element.bounds().right() <= px(720.), "{id} extends past the window");
                    assert!(element.bounds().bottom() <= px(840.), "{id} extends below the window");
                }
                window.click("language", cx);
            }
            window.click("disable-power", cx);
            window.click("cancel-confirm", cx);
        }).unwrap();
        handle.update(cx, |view, _, _| {
            assert!(view.modal.is_none());
            assert!(view.data.settings.target.is_none());
            assert!(!view.data.settings.busy());
            assert_eq!(view.data.settings.power.as_ref().map(|p| p.1), Some(true));
        }).unwrap();
    }

    #[gpui_kit::test]
    fn settings_adapter_selector_changes_the_target_and_refreshes_power(cx: &mut TestAppContext) {
        let _lock = UI_TEST_LOCK.lock().unwrap();
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(720.), px(840.)), |_, _| {
            let mut data = App::empty(true);
            let first = demo_adapter();
            let mut second = demo_adapter();
            second.id = "SECOND-DEMO-INTERFACE".into();
            second.name = "Second Steam Frame adapter".into();
            data.settings.power = Some((first.id.clone(), true));
            data.adapters = vec![first, second];
            data.selected = Some(0);
            Dashboard { data, modal: None, page: Page::Settings, frame: None, adapter_menu: false, log_scroll: ScrollHandle::new() }
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("settings-adapter-select", cx);
            window.render_frame(cx);
            assert!(window.find("settings-adapter-1").visible());
            window.click("settings-adapter-1", cx);
        }).unwrap();
        handle.update(cx, |view, _, _| {
            assert_eq!(view.data.selected, Some(1));
            assert!(!view.adapter_menu);
            assert!(view.data.settings.power.is_none());
        }).unwrap();
    }

    #[gpui_kit::test]
    fn cancel_does_not_start_a_device_operation(cx: &mut TestAppContext) {
        let _lock = UI_TEST_LOCK.lock().unwrap();
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(800.), px(700.)), |_, _| {
            let mut data = App::empty(true);
            data.adapters.push(demo_adapter());
            data.selected = Some(0);
            Dashboard { data, modal: Some(MainModal::SetUs), page: Page::Local, frame: None, adapter_menu: false, log_scroll: ScrollHandle::new() }
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("cancel-confirm", cx);
        }).unwrap();
        handle.update(cx, |view, _, _| {
            assert!(view.modal.is_none());
            assert!(view.data.receiver.is_none());
        }).unwrap();
    }

    #[gpui_kit::test]
    fn tabs_switch_within_one_window(cx: &mut TestAppContext) {
        let _lock = UI_TEST_LOCK.lock().unwrap();
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(720.), px(900.)), |_, _| Dashboard {
            data: App::empty(true), modal: None, page: Page::Local, frame: None, adapter_menu: false, log_scroll: ScrollHandle::new(),
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("headset-tab", cx);
        }).unwrap();
        handle.update(cx, |view, _, _| {
            assert_eq!(view.page, Page::Headset);
            assert!(view.frame.is_some());
        }).unwrap();
    }

    #[gpui_kit::test]
    fn one_adapter_still_opens_the_selector(cx: &mut TestAppContext) {
        let _lock = UI_TEST_LOCK.lock().unwrap();
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(720.), px(840.)), |_, _| {
            let mut data = App::empty(true);
            data.adapters.push(demo_adapter());
            data.selected = Some(0);
            Dashboard { data, modal: None, page: Page::Local, frame: None, adapter_menu: false, log_scroll: ScrollHandle::new() }
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("adapter-select", cx);
        }).unwrap();
        handle.update(cx, |view, _, _| assert!(view.adapter_menu)).unwrap();
    }
}
