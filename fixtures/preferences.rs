const CRUST: u32 = 0x11111b;
const BASE: u32 = 0x1e1e2e;
const SURFACE: u32 = 0x313244;
const TEXT: u32 = 0xcdd6f4;
const SUBTEXT: u32 = 0xa6adc8;
const ACCENT: u32 = 0xcba6f7;

fn toggle(on: bool) -> impl IntoElement {
    div()
        .flex_none()
        .w(px(36.))
        .h(px(20.))
        .p(px(2.))
        .rounded_full()
        .flex()
        .when(on, |el| el.justify_end().bg(rgb(ACCENT)))
        .when(!on, |el| el.justify_start().bg(rgb(SURFACE)))
        .child(div().size(px(16.)).rounded_full().bg(rgb(if on { BASE } else { SUBTEXT })))
}

fn setting(title: &str, detail: &str, on: bool, last: bool) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .py_3()
        .when(!last, |el| el.border_b_1().border_color(rgb(SURFACE)))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_sm().text_color(rgb(TEXT)).child(title.to_string()))
                .child(div().text_xs().text_color(rgb(SUBTEXT)).child(detail.to_string())),
        )
        .child(toggle(on))
}

fn segmented(options: &[&str], selected: usize) -> impl IntoElement {
    div()
        .flex()
        .p_1()
        .gap_1()
        .rounded_lg()
        .bg(rgb(CRUST))
        .children(options.iter().enumerate().map(|(i, label)| {
            div()
                .flex_1()
                .py_1p5()
                .rounded_md()
                .text_center()
                .text_sm()
                .when(i == selected, |el| el.bg(rgb(SURFACE)).text_color(rgb(TEXT)))
                .when(i != selected, |el| el.text_color(rgb(SUBTEXT)))
                .child(label.to_string())
        }))
}

fn render() -> impl IntoElement {
    let settings = [
        ("Desktop notifications", "Show a banner when a build finishes", true),
        ("Sound effects", "Play a chime when something fails", false),
        ("Auto-update", "Install updates while idle", true),
    ];
    let count = settings.len();

    div()
        .w(px(380.))
        .flex()
        .flex_col()
        .gap_5()
        .p_5()
        .rounded_xl()
        .bg(rgb(BASE))
        .border_1()
        .border_color(rgb(SURFACE))
        .shadow_lg()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(rgb(TEXT)).child("Preferences"))
                .child(div().text_sm().text_color(rgb(SUBTEXT)).child("Changes apply to every workspace.")),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(rgb(SUBTEXT)).child("THEME"))
                .child(segmented(&["System", "Light", "Dark"], 2)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(rgb(SUBTEXT)).child("GENERAL"))
                .children(
                    settings
                        .iter()
                        .enumerate()
                        .map(|(i, (title, detail, on))| setting(title, detail, *on, i == count - 1)),
                ),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(div().px_3().py_1p5().rounded_md().text_sm().text_color(rgb(SUBTEXT)).child("Cancel"))
                .child(
                    div()
                        .id("save")
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .text_sm()
                        .bg(rgb(ACCENT))
                        .text_color(rgb(CRUST))
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.85))
                        .child("Save"),
                ),
        )
}
