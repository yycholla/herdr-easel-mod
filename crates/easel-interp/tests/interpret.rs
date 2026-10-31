use easel_interp::{
    DiagnosticKind, interpret,
    ir::{Call, Color, Element, ElementKind, Length, Node, Value},
};

fn fixture(name: &str) -> String {
    let path = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn root(source: &str) -> Element {
    match interpret(source) {
        Ok(program) => match program.root {
            Node::Element(el) => el,
            other => panic!("expected an element, got {other:?}"),
        },
        Err(diags) => panic!("interpretation failed:\n{}", render_diags(&diags)),
    }
}

fn render_diags(diags: &[easel_interp::Diagnostic]) -> String {
    diags.iter().map(|d| format!("  {d}\n")).collect()
}

fn texts(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::Text { text } => out.push(text.clone()),
        Node::Element(el) => el.children.iter().for_each(|child| texts(child, out)),
    }
}

fn all_text(el: &Element) -> Vec<String> {
    let mut out = Vec::new();
    texts(&Node::Element(el.clone()), &mut out);
    out
}

fn call<'a>(el: &'a Element, method: &str) -> &'a Call {
    el.calls
        .iter()
        .find(|c| c.method == method)
        .unwrap_or_else(|| panic!("no `.{method}()` call in {:?}", el.calls))
}

fn errors(source: &str) -> Vec<easel_interp::Diagnostic> {
    interpret(source).expect_err("expected diagnostics")
}

// ---- fixtures ----

#[test]
fn card_fixture() {
    let card = root(&fixture("card.rs"));
    assert_eq!(card.kind, ElementKind::Div);
    assert_eq!(call(&card, "w").args, [Value::Length(Length::Px(360.))]);
    assert_eq!(call(&card, "bg").args, [Value::Color(Color::Rgb(0x1e1e2e))]);
    assert_eq!(
        all_text(&card),
        [
            "Ada Lovelace",
            "Analytical Engine enthusiast",
            "128",
            "Posts",
            "4096",
            "Followers",
            "12",
            "Following",
            "Follow"
        ]
    );

    let Node::Element(button) = card.children.last().unwrap() else {
        panic!()
    };
    assert_eq!(call(button, "id").args, [Value::Str("follow".into())]);
    let Value::Style(hover) = &call(button, "hover").args[0] else {
        panic!()
    };
    assert_eq!(
        hover,
        &[Call {
            method: "bg".into(),
            args: vec![Value::Color(Color::Rgb(0x6d28d9))]
        }]
    );
}

#[test]
fn dashboard_fixture() {
    let program =
        interpret(&fixture("dashboard.rs")).unwrap_or_else(|d| panic!("{}", render_diags(&d)));
    assert_eq!(program.entry, "impl Render for Dashboard");
    let Node::Element(dash) = program.root else {
        panic!()
    };
    assert_eq!(
        all_text(&dash),
        [
            "Production overview",
            "2 active alerts",
            "TRAFFIC",
            "Requests",
            "18.4k",
            "▲",
            "Errors",
            "1.2k",
            "▼",
            "Latency p95",
            "3.1k",
            "■",
            "SERVICES",
            "api",
            "healthy",
            "worker",
            "degraded",
            "scheduler",
            "healthy",
        ]
    );
    // `.bg(gpui::red().opacity(0.2))` on the alert banner.
    let Node::Element(alert) = &dash.children[1] else {
        panic!()
    };
    assert_eq!(
        call(alert, "bg").args,
        [Value::Color(Color::Opacity {
            base: Box::new(Color::Named("red".into())),
            factor: 0.2
        })]
    );
    // Striped rows from the `for` loop with `.when(i % 2 == 0, ..)`.
    let Node::Element(rows) = dash.children.last().unwrap() else {
        panic!()
    };
    let striped: Vec<bool> = rows
        .children
        .iter()
        .map(|row| matches!(row, Node::Element(el) if el.has_call("bg")))
        .collect();
    assert_eq!(striped, [true, false, true]);
}

#[test]
fn counter_fixture_needs_compiled_mode() {
    let diags = errors(&fixture("counter.rs"));
    assert!(
        diags.iter().all(|d| d.kind == DiagnosticKind::Unsupported),
        "{}",
        render_diags(&diags)
    );
    let messages: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("`.on_click(..)`")),
        "{messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("`cx.listener(..)`")),
        "{messages:?}"
    );
}

// ---- entry points ----

#[test]
fn bare_expression() {
    let el = root("div().flex().child(\"hi\")");
    assert_eq!(
        el.calls,
        [Call {
            method: "flex".into(),
            args: vec![]
        }]
    );
    assert_eq!(all_text(&el), ["hi"]);
}

#[test]
fn text_root() {
    let program = interpret("format!(\"{}+{}\", 1, 2)").unwrap();
    assert_eq!(program.root, Node::Text { text: "1+2".into() });
}

#[test]
fn named_entry_function_and_helpers() {
    let el = root(
        "fn badge(label: &str, color: u32) -> impl IntoElement {
            div().px_2().bg(rgb(color)).child(label.to_string())
        }
        fn preview() -> impl IntoElement {
            div().child(badge(\"new\", 0x22c55e)).child(badge(\"beta\", 0xeab308))
        }",
    );
    assert_eq!(all_text(&el), ["new", "beta"]);
}

#[test]
fn ambiguous_entry_is_reported() {
    let diags =
        errors("fn a() -> impl IntoElement { div() }\nfn b() -> impl IntoElement { div() }");
    assert!(
        diags[0].message.contains("name the entry point"),
        "{}",
        diags[0]
    );
}

#[test]
fn render_once_builder_methods() {
    let el = root(
        "#[derive(IntoElement)]
        struct Chip { label: SharedString, selected: bool }
        impl Chip {
            fn new(label: &str) -> Self { Self { label: label.into(), selected: false } }
            fn selected(mut self, selected: bool) -> Self { self.selected = selected; self }
        }
        impl RenderOnce for Chip {
            fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
                div().when(self.selected, |d| d.border_1()).child(self.label)
            }
        }
        fn render() -> impl IntoElement {
            div().child(Chip::new(\"a\")).child(Chip::new(\"b\").selected(true))
        }",
    );
    let chips: Vec<bool> = el
        .children
        .iter()
        .map(|c| matches!(c, Node::Element(e) if e.has_call("border_1")))
        .collect();
    assert_eq!(chips, [false, true]);
    assert_eq!(all_text(&el), ["a", "b"]);
}

// ---- language features ----

#[test]
fn match_with_guards_ranges_and_options() {
    let el = root(
        "let label = |n: i32| match n {
            0 => \"zero\".to_string(),
            1..=9 => \"small\".to_string(),
            n if n < 0 => \"negative\".to_string(),
            _ => format!(\"big {n}\"),
        };
        let first = match Some(3) { Some(x) => x * 2, None => 0 };
        div().children([0, 5, -1, 50].iter().map(|n| label(*n))).child(first.to_string())",
    );
    assert_eq!(all_text(&el), ["zero", "small", "negative", "big 50", "6"]);
}

#[test]
fn loops_and_compound_assignment() {
    let el = root(
        "let mut list = div();
        let mut total = 0;
        for i in 1..=3 {
            total += i;
            list = list.child(format!(\"{i}:{total}\"));
        }
        let mut n = 0;
        while n < 4 { n += 2; }
        list.child(n.to_string())",
    );
    assert_eq!(all_text(&el), ["1:1", "2:3", "3:6", "4"]);
}

#[test]
fn format_specs() {
    let program = interpret(r#"format!("[{:>4}] [{:<3}] [{:^5}] [{:03}] [{:.2}] [{:?}] [{x}]", 7, "a", "mid", 5, 3.14159, "q", x = 1)"#)
        .unwrap();
    assert_eq!(
        program.root,
        Node::Text {
            text: r#"[   7] [a  ] [ mid ] [005] [3.14] ["q"] [1]"#.into()
        }
    );
}

#[test]
fn integer_and_float_arithmetic_follow_rust() {
    let program = interpret("format!(\"{} {} {}\", 7 / 2, 7.0 / 2.0, (3 as f32) * 1.5)").unwrap();
    assert_eq!(
        program.root,
        Node::Text {
            text: "3 3.5 4.5".into()
        }
    );
}

#[test]
fn custom_shadows() {
    let el = root(
        "div().shadow(vec![BoxShadow {
            color: hsla(0., 0., 0., 0.25),
            offset: point(px(0.), px(4.)),
            blur_radius: px(12.),
            spread_radius: px(0.),
        }])",
    );
    let Value::Shadows(shadows) = &call(&el, "shadow").args[0] else {
        panic!()
    };
    assert_eq!(shadows[0].offset, (0., 4.));
    assert_eq!(shadows[0].blur_radius, 12.);
    assert!(!shadows[0].inset);
}

#[test]
fn inset_shadows() {
    let el = root(
        "div().shadow(vec![BoxShadow {
            color: hsla(0., 0., 0., 0.25),
            offset: point(px(0.), px(1.)),
            blur_radius: px(2.),
            spread_radius: px(0.),
            inset: true,
        }])",
    );
    let Value::Shadows(shadows) = &call(&el, "shadow").args[0] else {
        panic!()
    };
    assert!(shadows[0].inset);
}

// ---- diagnostics ----

#[test]
fn old_flex_grow_points_at_the_new_form() {
    let diags = errors("div().flex_grow()");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("`.flex_grow_1()`"), "{}", diags[0].message);
}

#[test]
fn wrong_argument_type_is_invalid_with_location() {
    let diags = errors("div()\n    .flex()\n    .bg(px(4.))");
    assert_eq!(diags.len(), 1);
    let d = &diags[0];
    assert_eq!(d.kind, DiagnosticKind::Invalid);
    assert_eq!((d.line, d.column), (3, 9));
    assert!(d.message.contains("`.bg(..)` expects a color"), "{d}");
}

#[test]
fn first_line_columns_are_not_shifted() {
    let diags = errors("div().nope()");
    assert_eq!((diags[0].line, diags[0].column), (1, 7));
}

#[test]
fn integer_lengths_get_a_hint() {
    let diags = errors("div().w(px(4))");
    assert!(diags[0].message.contains("px(4.)"), "{}", diags[0]);
}

#[test]
fn rgba_has_no_opacity() {
    let diags = errors("div().bg(rgb(0x112233).opacity(0.5))");
    assert!(diags[0].message.contains("Hsla::from"), "{}", diags[0]);
    root("div().bg(Hsla::from(rgb(0x112233)).opacity(0.5))");
}

#[test]
fn numbers_are_not_children() {
    let diags = errors("div().child(42)");
    assert_eq!(diags[0].kind, DiagnosticKind::Invalid);
    assert!(diags[0].message.contains("to_string"), "{}", diags[0]);
}

#[test]
fn stateful_methods_need_an_id() {
    assert!(
        errors("div().overflow_y_scroll()")[0]
            .message
            .contains("needs `.id(..)` first")
    );
    root("div().id(\"list\").overflow_y_scroll()");
}

#[test]
fn reports_every_problem_at_once() {
    let diags = errors("div().on_click(|_, _, _| {}).bg(1.0).child(missing)");
    let kinds: Vec<_> = diags.iter().map(|d| d.kind).collect();
    assert_eq!(
        kinds,
        [
            DiagnosticKind::Unsupported,
            DiagnosticKind::Invalid,
            DiagnosticKind::Invalid
        ]
    );
}

#[test]
fn syntax_errors_have_locations() {
    let diags = errors("div()\n  .flex(\n");
    assert_eq!(diags[0].kind, DiagnosticKind::Syntax);
    assert!(diags[0].line >= 2, "{}", diags[0]);
}

#[test]
fn ranges_are_bounded() {
    assert!(
        errors("div().children((0..1_000_000).map(|_| div()))")[0]
            .message
            .contains("limited")
    );
}

#[test]
fn recursion_is_bounded() {
    let diags = errors("fn render() -> impl IntoElement { render() }");
    assert!(diags[0].message.contains("deeper than"), "{}", diags[0]);
}

#[test]
fn ir_serializes() {
    let el = root(&fixture("card.rs"));
    let json = serde_json::to_string(&Node::Element(el.clone())).unwrap();
    let back: Node = serde_json::from_str(&json).unwrap();
    assert_eq!(back, Node::Element(el));
}

#[test]
fn elements_record_their_source_location() {
    let el = root("div()\n    .child(\n        div().child(\"x\"),\n    )");
    assert_eq!(el.source, Some(easel_interp::ir::SourceRef { line: 1, column: 1 }));
    let Node::Element(inner) = &el.children[0] else { panic!() };
    assert_eq!(inner.source, Some(easel_interp::ir::SourceRef { line: 3, column: 9 }));
    let node = Node::Element(el);
    assert_eq!(node.at(&[0, 0]), Some(&Node::Text { text: "x".into() }));
    assert_eq!(node.text_content(), "x");
}
