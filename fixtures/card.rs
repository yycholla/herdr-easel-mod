// A bare block: `let` bindings followed by the element expression.
let name = "Ada Lovelace";
let stats = [("Posts", 128), ("Followers", 4_096), ("Following", 12)];
let accent = rgb(0x7c3aed);

div()
    .flex()
    .flex_col()
    .gap_4()
    .p_6()
    .w(px(360.))
    .rounded_xl()
    .bg(rgb(0x1e1e2e))
    .border_1()
    .border_color(rgb(0x313244))
    .shadow_lg()
    .text_color(rgb(0xcdd6f4))
    .child(
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().size_12().rounded_full().bg(accent))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).child(name))
                    .child(div().text_sm().text_color(rgb(0xa6adc8)).child("Analytical Engine enthusiast")),
            ),
    )
    .child(
        div().flex().justify_between().children(stats.iter().map(|(label, count)| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(format!("{count}")))
                .child(div().text_xs().text_color(rgb(0xa6adc8)).child(label.to_string()))
        })),
    )
    .child(
        div()
            .id("follow")
            .px_4()
            .py_2()
            .rounded_md()
            .bg(accent)
            .text_center()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(0x6d28d9)))
            .child("Follow"),
    )
