// A full program: the interpreter renders `impl Render for Dashboard`, taking
// `self` from the struct literal in `main`.
use gpui::{
    App, Bounds, Context, IntoElement, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};

const BG: u32 = 0x11111b;
const SURFACE: u32 = 0x1e1e2e;
const MUTED: u32 = 0x9399b2;

#[derive(Clone, Copy, PartialEq)]
enum Trend {
    Up,
    Down,
    Flat,
}

#[derive(Clone, Copy)]
struct Metric {
    label: &'static str,
    value: f64,
    trend: Trend,
}

#[derive(IntoElement)]
struct StatTile {
    metric: Metric,
}

impl RenderOnce for StatTile {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let (arrow, color) = match self.metric.trend {
            Trend::Up => ("▲", rgb(0xa6e3a1)),
            Trend::Down => ("▼", rgb(0xf38ba8)),
            Trend::Flat => ("■", rgb(MUTED)),
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_4()
            .flex_1()
            .rounded_lg()
            .bg(rgb(SURFACE))
            .child(div().text_sm().text_color(rgb(MUTED)).child(self.metric.label))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(div().text_2xl().child(format!("{:.1}k", self.metric.value / 1000.)))
                    .child(div().text_color(color).child(arrow)),
            )
    }
}

fn section_title(title: &str) -> impl IntoElement {
    div().text_xs().text_color(rgb(MUTED)).child(title.to_uppercase())
}

struct Dashboard {
    title: String,
    metrics: Vec<Metric>,
    alerts: usize,
}

impl Render for Dashboard {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = div().flex().flex_col().gap_1();
        for (i, name) in ["api", "worker", "scheduler"].iter().enumerate() {
            rows = rows.child(
                div()
                    .flex()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .when(i % 2 == 0, |this| this.bg(rgb(SURFACE)))
                    .child(*name)
                    .child(if i == 1 { "degraded" } else { "healthy" }),
            );
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .bg(rgb(BG))
            .text_color(rgb(0xcdd6f4))
            .child(div().text_xl().child(self.title.clone()))
            .when(self.alerts > 0, |this| {
                this.child(
                    div()
                        .p_2()
                        .rounded_md()
                        .bg(gpui::red().opacity(0.2))
                        .child(format!("{} active alert{}", self.alerts, if self.alerts == 1 { "" } else { "s" })),
                )
            })
            .child(section_title("Traffic"))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .children(self.metrics.iter().map(|m| StatTile { metric: *m })),
            )
            .child(section_title("Services"))
            .child(rows)
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(720.), px(480.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| Dashboard {
                    title: "Production overview".into(),
                    metrics: vec![
                        Metric { label: "Requests", value: 18_400., trend: Trend::Up },
                        Metric { label: "Errors", value: 1_250., trend: Trend::Down },
                        Metric { label: "Latency p95", value: 3_100., trend: Trend::Flat },
                    ],
                    alerts: 2,
                })
            },
        )
        .unwrap();
    });
}
