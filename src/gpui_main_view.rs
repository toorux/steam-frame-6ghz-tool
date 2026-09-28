use gpui_kit::{prelude::*, *};
use gpui_kit::component::{Disableable, Icon, Root, Sizable, TitleBar, button::{Button, ButtonVariants}, checkbox::Checkbox, scroll::{Scrollbar, ScrollbarMode}};
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
enum MainModal { SetUs, EnableAuto, DisableAuto, UpdateService, UpdateApp }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page { Local, Headset }

impl Dashboard {
    fn new(demo: bool, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| loop {
            let Some(this) = this.upgrade() else { break };
            let interval = this.update(cx, |view, cx| {
                view.data.poll();
                view.data.poll_service();
                view.data.poll_update();
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
        self.modal = None;
        cx.notify();
    }
    fn show_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(frame) = &self.frame {
            frame.update(cx, |view, cx| view.cancel_preview(window, cx));
        }
        self.page = Page::Local;
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
    fn log_panel(&self, cx: &mut Context<Self>) -> Div {
        let mut log_list = div().flex().flex_col().gap_1();
        for &index in self.data.visible_rows.iter().rev().take(300).rev() {
            let row = &self.data.rows[index];
            let color = if row.level == Level::Error { ERROR } else { INK };
            let source = if i18n::is_english() {
                if row.source == ui_log::Source::Service { "Service" } else { "App" }
            } else { row.source.label() };
            log_list = log_list.child(div().text_color(rgb(color)).text_sm()
                .child(format!("{}  {}  {}", ui_log::local_time(&row.time), source, row.text)));
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
            .child(div().relative().min_h(px(0.)).flex_1()
                .child(div().id("log-scroll").size_full().overflow_y_scroll().track_scroll(&self.log_scroll).child(log_list))
                .child(div().absolute().inset_0().child(Scrollbar::vertical(&self.log_scroll)
                    .mode(ScrollbarMode::Always).viewport_from_layout()
                    .styles(|styles| styles
                        .track(|track| track.bg(rgb(0xe6eef2).into()).width(px(8.)))
                        .thumb(|thumb| thumb.bg(rgb(0x718c9b)).width(px(8.)).radius(px(4.)))))))
    }
    fn modal_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        let modal = self.modal?;
        let (title, detail) = match modal {
            MainModal::SetUs => (t("设置 US？", "Set this adapter to US?"), t("只向当前适配器提交一次设置，然后复查状态。", "Send the setting once, then check the adapter again.")),
            MainModal::EnableAuto => (t("开启自动应用？", "Turn on automatic restore?"), t("安装 Windows 服务；需要管理员权限。", "This installs a Windows service and requires administrator access.")),
            MainModal::DisableAuto => (t("关闭自动应用？", "Turn off automatic restore?"), t("卸载服务，保留日志。", "The service will be removed; logs will be kept.")),
            MainModal::UpdateService => (t("更新服务副本？", "Update the background service?"), t("卸载并重新安装服务，保留日志；不会解除暂停保护。", "Reinstall the service without deleting logs or clearing a safety pause.")),
            MainModal::UpdateApp => (t("发现新版本", "New version available"), t("打开对应 Release 页面。下载后请自行替换程序。", "Open the Release page. Download and replace the app manually.")),
        };
        Some(div().absolute().inset_0().bg(rgba(0x1f3445b0)).flex().items_center().justify_center()
            .child(Self::card().w(px(470.)).gap_4()
                .child(Self::label(title).text_xl())
                .child(Self::muted(detail))
                .child(div().flex().gap_2().justify_end()
                    .child(Button::new("cancel-confirm").label(t("取消", "Cancel"))
                        .on_click(cx.listener(|this, _, _, cx| { this.modal = None; cx.notify(); })))
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
        let auto_label = if auto.installed { t("已开启", "Enabled") } else { t("未开启", "Off") };
        let service_detail = if auto.running { t("正在处理设备", "Processing an adapter") }
            else if auto.paused { t("已暂停，等待人工检查", "Paused for manual inspection") }
            else if auto.installed && !auto.enabled { t("已安装但未启用", "Installed but disabled") }
            else { t("开机、插拔时按需恢复；处理结束后退出。", "Restores on startup and reconnect, then exits.") };
        let titlebar = div().w_full().h(px(58.)).flex_shrink_0().bg(rgb(0xffffff))
            .flex().items_center()
            .child(div().id("chrome-drag").h_full().flex_1().flex().items_center().gap_3().pl_5()
                .child(img(Arc::new(Image::from_bytes(ImageFormat::Ico, include_bytes!("../assets/icon.ico").to_vec()))).size(px(34.)))
                .child(Self::label("Steam Frame 6GHz Tool").text_lg())
                .child(Self::muted(format!("v{}", env!("CARGO_PKG_VERSION"))))
                .window_control_area(WindowControlArea::Drag))
            .child(Button::new("language").icon(Icon::default().data(include_bytes!("../assets/languages.svg"))).ghost()
                .accessibility_label(t("切换语言", "Switch language"))
                .tooltip(t("切换语言", "Switch language"))
                .on_click(cx.listener(|this, _, _, cx| {
                    i18n::toggle();
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
                .child(Self::label(auto_label).text_color(rgb(if auto.paused { ERROR } else { INK })))
                .child(div().flex_1())
                .child(Button::new("toggle-auto").label(if auto.installed { t("关闭并卸载", "Turn off") } else { t("开启自动应用", "Turn on") })
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Some(if this.data.auto_state.is_some_and(|s| s.installed) { MainModal::DisableAuto } else { MainModal::EnableAuto }); cx.notify(); })))
                .when(auto.needs_update, |row| row.child(Button::new("update-service").label(t("更新服务", "Update service")).primary()
                    .on_click(cx.listener(|this, _, _, cx| { this.modal = Some(MainModal::UpdateService); cx.notify(); })))))
            .child(Self::muted(if auto.installed && auto.running { service_detail } else {
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
            titlebar: Some(TitlebarOptions { title: Some("Steam Frame 6GHz Tool".into()), ..TitleBar::title_bar_options() }),
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

    #[gpui_kit::test]
    fn cancel_does_not_start_a_device_operation(cx: &mut TestAppContext) {
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
