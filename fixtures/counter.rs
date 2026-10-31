// Interactive: needs a click handler and view state, so the interpreter
// reports it as unsupported and Auto mode compiles it instead.
use gpui::{
    App, Context, IntoElement, Window, WindowOptions, div, prelude::*, rgb,
};

struct Counter {
    count: i32,
}

impl Render for Counter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .gap_2()
            .p_4()
            .bg(rgb(0x1e1e2e))
            .text_color(rgb(0xcdd6f4))
            .child(format!("Count: {}", self.count))
            .child(
                div()
                    .id("increment")
                    .px_3()
                    .rounded_md()
                    .bg(rgb(0x89b4fa))
                    .child("+1")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.count += 1;
                        cx.notify();
                    })),
            )
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| Counter { count: 0 }))
            .unwrap();
    });
}
