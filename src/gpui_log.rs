//! One selectable document per log pane; selection, copy and scrolling belong to GPUI Kit.
use gpui_kit::{base::{TextView, TextViewStyle}, HighlightStyle, StyleRefinement, Styled, rgb};
use std::ops::Range;
use gpui_kit::{Div, ParentElement, ScrollHandle, StatefulInteractiveElement, InteractiveElement, div, px};
use gpui_kit::component::scroll::ScrollableElement;

pub fn pane(id: &'static str, scroll: &ScrollHandle, content: TextView) -> Div {
    // The native scrollbar overlays the fixed viewport, not its translated content.
    div().relative().min_h(px(0.)).flex_1()
        .child(div().id(id).size_full().pr_3().overflow_y_scroll().track_scroll(scroll).child(content))
        .vertical_scrollbar(scroll)
}

fn document(rows: impl IntoIterator<Item = (String, bool)>) -> (String, Vec<Range<usize>>) {
    let mut text = String::new();
    let mut errors = Vec::new();
    for (line, error) in rows {
        let start = text.len();
        text.push_str(&line);
        if error && text.len() > start { errors.push(start..text.len()); }
        text.push('\n');
    }
    // Fence literal log text, including arbitrary commands and markup, without interpreting it.
    let fence = "~".repeat(text.split(|c| c != '~').map(str::len).max().unwrap_or(0).max(2) + 1);
    (format!("{fence}\n{text}{fence}"), errors)
}

pub fn text(id: &'static str, rows: impl IntoIterator<Item = (String, bool)>) -> TextView {
    let (source, errors) = document(rows);
    TextView::markdown(id, source)
        .selectable(true)
        .selection_format(gpui_kit::base::text::SelectionFormat::Plain)
        .style(TextViewStyle::default()
            .with_foreground(rgb(0x1f3445).into())
            .with_code_background(rgb(0xffffff).into())
            .with_code_block(StyleRefinement::default().p_0().text_sm()))
        .code_block_highlighter(move |_| errors.iter().cloned().map(|range| (
            range, HighlightStyle { color: Some(rgb(0xb42318).into()), ..Default::default() }
        )).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{AppContext, TestAppContext};
    #[test]
    fn logs_are_literal_and_error_offsets_are_utf8_bytes() {
        let (source, errors) = document([("<tag> ~~~ `command`".into(), false), ("[ERROR] 错误".into(), true)]);
        assert!(source.starts_with("~~~~\n<tag> ~~~ `command`\n"));
        let body = source.strip_prefix("~~~~\n").unwrap();
        assert_eq!(&body[errors[0].clone()], "[ERROR] 错误");
    }

    #[gpui_kit::test]
    fn select_all_preserves_multiline_plain_text(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            let (source, _) = document([("first <&>".into(), false), ("[ERROR] 中文".into(), true)]);
            let state = cx.new(|cx| gpui_kit::base::text::TextViewState::markdown(&source, cx)
                .selection_format(gpui_kit::base::text::SelectionFormat::Plain));
            state.update(cx, |state, cx| {
                state.select_all(cx);
                assert_eq!(state.selected_text().trim_end(), "first <&>\n[ERROR] 中文");
            });
        });
    }

    #[gpui_kit::test]
    fn keyboard_select_all_and_copy(cx: &mut TestAppContext) {
        use gpui_kit::{Context, IntoElement, Modifiers, MouseButton, ParentElement, Render, Window, div, point, px};
        struct Pane;
        impl Render for Pane {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(gpui_kit::base::TextSelectionLayer)
                    .child(super::text("test-log", [("first <&>".into(), false), ("[ERROR] 中文".into(), true)]))
            }
        }
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, _| Pane);
        cx.update(|window, cx| { let _ = window.draw(cx); });
        cx.simulate_mouse_down(point(px(12.), px(8.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(point(px(12.), px(8.)), MouseButton::Left, Modifiers::default());
        cx.simulate_keystrokes("ctrl-a ctrl-c");
        assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).unwrap(), "first <&>\n[ERROR] 中文");
    }

    #[gpui_kit::test]
    fn scrollbar_stays_in_viewport_and_dragging_down_moves_forward(cx: &mut TestAppContext) {
        use gpui_kit::{Context, IntoElement, Modifiers, MouseButton, Render, Window, point};
        struct Pane { scroll: ScrollHandle }
        impl Render for Pane {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().w(px(300.)).h(px(200.)).flex().flex_col()
                    .child(super::pane("test-scroll", &self.scroll,
                        super::text("long-log", (0..100).map(|i| (format!("line {i}"), false)))))
            }
        }
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_kit::component::Theme::set_scrollbar_mode(gpui_kit::component::scroll::ScrollbarMode::Always, cx);
        });
        let scroll = ScrollHandle::new();
        let (_, cx) = cx.add_window_view(|_, _| Pane { scroll: scroll.clone() });
        cx.update(|window, cx| { let _ = window.draw(cx); });
        cx.update(|window, cx| { let _ = window.draw(cx); });
        assert!(scroll.max_offset().y > px(200.));
        let bounds = cx.debug_bounds("scrollbar-overlay").expect("native scrollbar layer");
        assert_eq!(bounds.size.height, px(200.));
        assert_eq!(bounds.size.width, px(300.));
        assert_eq!(scroll.offset().y, px(0.));
        cx.simulate_mouse_move(point(px(292.), px(10.)), None, Modifiers::default());
        cx.simulate_mouse_down(point(px(292.), px(10.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(point(px(292.), px(150.)), Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(point(px(292.), px(150.)), MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| { let _ = window.draw(cx); });
        assert!(scroll.offset().y < px(-1.), "dragging the thumb down must advance through the log");
        assert_eq!(cx.debug_bounds("scrollbar-overlay").unwrap(), bounds);
        let lower_offset = scroll.offset().y;
        cx.simulate_mouse_down(point(px(292.), px(150.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(point(px(292.), px(10.)), Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(point(px(292.), px(10.)), MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| { let _ = window.draw(cx); });
        assert!(scroll.offset().y > lower_offset, "dragging up must return toward earlier log entries: before {lower_offset:?}, after {:?}", scroll.offset());
        scroll.scroll_to_bottom();
        cx.update(|window, cx| { let _ = window.draw(cx); });
        assert_eq!(cx.debug_bounds("scrollbar-overlay").unwrap(), bounds);
    }
}
