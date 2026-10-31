//! Turns a snippet into a complete program for the sandbox crate.

use syn::{Fields, ImplItem, Item};

/// A generated `main.rs`.
#[derive(Debug)]
pub struct Wrapped {
    pub code: String,
    /// The `main.rs` line holding the snippet's first line.
    pub first_line: usize,
}

impl Wrapped {
    /// Maps a `main.rs` line back to the snippet, if it's inside it.
    pub fn snippet_line(&self, main_line: usize, snippet_lines: usize) -> Option<usize> {
        let line = main_line.checked_sub(self.first_line)? + 1;
        (line <= snippet_lines).then_some(line)
    }
}

const ENTRY_NAMES: &[&str] = &["render", "preview", "view", "ui", "app", "root"];

/// Wraps `source` (a block, functions, an `impl Render`, or a full program
/// with `fn main`) so it opens a window titled `title`.
pub fn wrap(source: &str, title: &str) -> Result<Wrapped, String> {
    let header = header(source);
    let first_line = header.lines().count() + 1;

    if let Ok(file) = syn::parse_file(source) {
        let fns: Vec<&syn::ItemFn> = file
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Fn(f) => Some(f),
                _ => None,
            })
            .collect();
        if fns.iter().any(|f| f.sig.ident == "main") {
            // A full program. It opens its own windows.
            return Ok(Wrapped {
                code: format!("{header}{source}\n{ASSETS}"),
                first_line,
            });
        }
        if let Some(view) = render_view(&file) {
            let code = format!("{header}{source}\n{}", main(title, &view));
            return Ok(Wrapped { code, first_line });
        }
        let entry = fns
            .iter()
            .find(|f| ENTRY_NAMES.contains(&f.sig.ident.to_string().as_str()))
            .or_else(|| {
                let zero_arg: Vec<_> = fns.iter().filter(|f| f.sig.inputs.is_empty()).collect();
                (zero_arg.len() == 1).then(|| zero_arg[0])
            });
        let Some(entry) = entry else {
            return Err("nothing to run: add `fn main`, an `impl Render`, or a `fn render() -> impl IntoElement`".into());
        };
        if !entry.sig.inputs.is_empty() {
            return Err(format!("the entry function `{}` must take no arguments", entry.sig.ident));
        }
        let code = format!(
            "{header}{source}\n\nstruct ArtifactRoot;\n\nimpl Render for ArtifactRoot {{\n    \
             fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {{\n        \
             {}()\n    }}\n}}\n{}",
            entry.sig.ident,
            main(title, "ArtifactRoot")
        );
        return Ok(Wrapped { code, first_line });
    }

    // A block: the body of `render`, with `window` and `cx` in scope.
    let open = "struct ArtifactRoot;\n\nimpl Render for ArtifactRoot {\n    \
                fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {\n";
    syn::parse_str::<syn::Block>(&format!("{{{source}\n}}")).map_err(|err| err.to_string())?;
    Ok(Wrapped {
        code: format!("{header}{open}{source}\n    }}\n}}\n{}", main(title, "ArtifactRoot")),
        first_line: first_line + open.lines().count(),
    })
}

fn header(source: &str) -> String {
    let mut header = String::new();
    // Inner attributes must come first, so only add ours if the snippet has none.
    if !source.contains("#![") {
        header.push_str("#![allow(unused, dead_code)]\n");
    }
    header.push_str("use gpui::{prelude::*, *};\n");
    header
}

/// The expression that constructs the snippet's `Render` view, if it has one.
fn render_view(file: &syn::File) -> Option<String> {
    let ty = file.items.iter().find_map(|item| match item {
        Item::Impl(imp) => {
            let (_, path, _) = imp.trait_.as_ref()?;
            if path.segments.last()?.ident != "Render" {
                return None;
            }
            let has_render = imp.items.iter().any(|i| matches!(i, ImplItem::Fn(f) if f.sig.ident == "render"));
            let syn::Type::Path(ty) = &*imp.self_ty else {
                return None;
            };
            has_render.then(|| ty.path.segments.last().unwrap().ident.to_string())
        }
        _ => None,
    })?;
    let fields = file.items.iter().find_map(|item| match item {
        Item::Struct(s) if s.ident == ty => Some(&s.fields),
        _ => None,
    });
    Some(match fields {
        Some(Fields::Unit) => ty,
        Some(Fields::Named(named)) if named.named.is_empty() => format!("{ty} {{}}"),
        // Views with state must implement `Default`; rustc says so otherwise.
        _ => format!("{ty}::default()"),
    })
}

fn main(title: &str, view: &str) -> String {
    format!(
        r#"
fn main() {{
    gpui_platform::application().with_assets(ArtifactAssets).run(|cx: &mut App| {{
        let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
        cx.open_window(
            WindowOptions {{
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {{
                    title: Some({title:?}.into()),
                    ..Default::default()
                }}),
                app_id: Some("gpui-artifact".into()),
                ..Default::default()
            }},
            |_, cx| cx.new(|_| {view}),
        )
        .unwrap();
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    }});
}}
{ASSETS}"#
    )
}

/// Loads `img`/`svg` paths relative to the working directory, matching the
/// viewer's interpreted rendering.
const ASSETS: &str = r#"
struct ArtifactAssets;

impl AssetSource for ArtifactAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        Ok(std::fs::read(path).ok().map(Into::into))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(std::fs::read_dir(path)
            .map(|dir| {
                dir.filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned().into())
                    .collect()
            })
            .unwrap_or_default())
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_a_block_and_maps_lines() {
        let wrapped = wrap("let x = 1;\ndiv()", "T").unwrap();
        let lines: Vec<_> = wrapped.code.lines().collect();
        assert_eq!(lines[wrapped.first_line - 1], "let x = 1;");
        assert_eq!(wrapped.snippet_line(wrapped.first_line + 1, 2), Some(2));
        assert_eq!(wrapped.snippet_line(wrapped.first_line + 2, 2), None);
        assert_eq!(wrapped.snippet_line(1, 2), None);
    }

    #[test]
    fn wraps_an_entry_function() {
        let wrapped = wrap("fn preview() -> impl IntoElement { div() }", "T").unwrap();
        assert!(wrapped.code.contains("preview()\n"));
        assert!(wrapped.code.contains("cx.new(|_| ArtifactRoot)"));
    }

    #[test]
    fn constructs_render_views() {
        let unit = wrap("struct V;\nimpl Render for V { fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement { div() } }", "T").unwrap();
        assert!(unit.code.contains("cx.new(|_| V)"));
        let stateful = wrap("struct V { n: i32 }\nimpl Render for V { fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement { div() } }", "T").unwrap();
        assert!(stateful.code.contains("V::default()"));
    }

    #[test]
    fn keeps_full_programs() {
        let wrapped = wrap("fn main() {}", "T").unwrap();
        assert!(wrapped.code.contains("fn main() {}"));
        assert!(!wrapped.code.contains("application().with_assets"));
    }

    #[test]
    fn escapes_titles() {
        let wrapped = wrap("div()", "say \"hi\"").unwrap();
        assert!(wrapped.code.contains(r#"Some("say \"hi\"".into())"#));
    }
}
