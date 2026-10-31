//! The MCP tools: render GPUI code in the viewer and report what happened.

use anyhow::{Context as _, Result, bail};
use easel_protocol::{
    Anchor, Artifact, Attachment, AttachmentKind, ChatReport, Client, FrameSize, Mode, Request,
    Response, Status, Summary,
};
use serde_json::{Value, json};
use std::{
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const CHEAT_SHEET: &str = "\
Renders GPUI (Rust, gpui-pre 0.3.8: the crates.io `gpui-pre` package, imported as `gpui`) UI code as a \
live artifact in an Easel mod pane beside this terminal in Herdr, or in the standalone viewer outside Herdr.
The mod uses Herdr's existing APIs; no Easel-specific Herdr build is required. Its gpui-kit bridge \
may reject some interpreted builders; auto mode then offers compilation. PNG capture requires the standalone viewer.

`source` can be a bare block ending in an element expression, functions with an entry point named \
`render`/`preview` (or a single zero-argument fn), an `impl Render for View` (field values are taken \
from a `View { .. }` literal, e.g. in `main`), or a complete program with `fn main`. Calling again \
with the same `id` (or `title`) updates the artifact in place.

Declarative code is interpreted instantly and safely. The interpreter supports:
- elements: div(), img(\"path or https url\"), svg().path(\"icon.svg\"); text children are strings
- every gpui `Styled` method (flex, gap_4, p_2, w(px(..)), size_full, rounded_md, bg, text_color, \
text_xl, font_weight(FontWeight::BOLD), shadow_md, shadow(vec![BoxShadow{..}]), cursor_pointer, …)
- values: rgb(0xRRGGBB), rgba(0xRRGGBBAA), hsla(h, s, l, a), gpui::red() etc., \
Hsla::from(..).opacity(..), px(1.), rems(1.), relative(0.5), auto()
- .child, .children(iter), .when/.when_some/.when_else/.map, .id(..), .hover(|s| ..), .active, \
.group/.group_hover, .overflow_y_scroll() (after .id)
- Rust: let/mut, consts, fns, structs, enums, RenderOnce components, closures, if/match/for/while, \
ranges, iterator chains (map/filter/enumerate/zip/rev/take/…), format!, vec!, tuples, arithmetic
Write valid Rust: f32 literals need a dot (`px(4.)`), and rgb(..) returns Rgba, which has no `.opacity()`.

gpui-pre differs from gpui 0.2: `flex_grow`/`flex_shrink` take the factor (`.flex_grow_1()` or \
`.flex_grow(1.)`), `BoxShadow { .. }` has an `inset: bool` field, `cursor_none` is gone, focus handles take \
`.focus(window, cx)`, and a `fn main` starts the app with `gpui_platform::application().run(..)` (not \
`Application::new()`), with `cx.on_window_closed(|cx, _window_id| ..)`.

Each call adds a new version as a frame on the viewer's canvas. The viewer has a chat with the user: they \
mark up frames (markers with notes, drawings, sticky notes) and send messages with those as attachments \
(see await_user_message); you answer with send_chat_message.

Event handlers, cx/window calls, entities, animations, canvas, and gradients aren't interpreted. In `auto` \
mode such code is compiled instead: the user must approve running it in the viewer (it runs sandboxed in \
its own window), and rustc errors are reported back with snippet line numbers.";

pub fn list() -> Value {
    json!([
        {
            "name": "render_gpui",
            "description": CHEAT_SHEET,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "GPUI Rust code" },
                    "title": { "type": "string", "description": "Shown in the viewer" },
                    "id": { "type": "string", "description": "Artifact to update; defaults to a slug of the title" },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "interpret", "compile"],
                        "description": "auto (default) interprets and compiles only if needed"
                    },
                    "wait_seconds": {
                        "type": "integer",
                        "description": "How long to wait for approval and the build when compiling (default 90)"
                    },
                    "preset": {
                        "type": "string",
                        "enum": ["hug", "desktop", "laptop", "tablet", "mobile"],
                        "description": "Frame size: hug (default) sizes to the content; desktop 1280x800, laptop 1440x900, tablet 834x1112, mobile 390x844. Use a fixed size for layouts with size_full()"
                    },
                    "width": { "type": "number", "description": "Frame width in px (overrides the preset)" },
                    "height": { "type": "number", "description": "Frame height in px (overrides the preset)" }
                },
                "required": ["source"]
            }
        },
        {
            "name": "get_gpui_artifact",
            "description": "Get an artifact's status (e.g. build result, exit code and stderr of a compiled run), optionally waiting while it builds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "wait_seconds": { "type": "integer", "description": "Wait this long for a pending build (default 0)" },
                    "include_source": { "type": "boolean" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "list_gpui_artifacts",
            "description": "List the artifacts in the viewer with their versions and status.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "await_user_message",
            "description": "Wait for the user's next message in Easel's chat. Messages can carry numbered attachments: element markers with notes, drawings, and sticky notes. Markers include source anchors. The Herdr mod sends drawing coordinates as text; the standalone viewer can include annotated frame images. Use after rendering when the user wants to review, and keep calling it to continue the conversation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "wait_seconds": { "type": "integer", "description": "How long to wait (default 300)" }
                }
            }
        },
        {
            "name": "send_chat_message",
            "description": "Post a message to the user in the viewer's chat, e.g. what you changed in response to their message. Refer to attachments by number.",
            "inputSchema": {
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }
        },
        {
            "name": "export_gpui_png",
            "description": "Capture an artifact's frame (latest version unless `version` is given) as a PNG with its annotations. Requires the standalone viewer; the Herdr mod does not expose frame capture. Returns the image and saved file path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "version": { "type": "integer" }
                },
                "required": ["id"]
            }
        }
    ])
}

/// MCP server state: the project whose viewer it talks to, and which user
/// messages Claude has already seen.
pub struct State {
    project: PathBuf,
    /// The newest user message already shown to Claude. Until the first
    /// report, the viewer decides: it remembers what earlier sessions saw.
    seen_message: Option<u64>,
}

impl Default for State {
    fn default() -> Self {
        let dir = std::env::var_os("GPUI_ARTIFACTS_PROJECT")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| ".".into());
        Self { project: easel_protocol::project_root(&dir), seen_message: None }
    }
}

pub struct ToolResult {
    pub content: Vec<Value>,
    pub is_error: bool,
}

impl ToolResult {
    fn text(text: impl Into<String>, is_error: bool) -> Self {
        Self { content: vec![json!({ "type": "text", "text": text.into() })], is_error }
    }
}

/// Runs a tool. Feedback the user sent since Claude last looked is attached
/// to every result, so it isn't missed even if Claude never waits for it.
pub fn call(state: &mut State, name: &str, args: &Value) -> Result<ToolResult> {
    let mut result = match name {
        "render_gpui" => render(&state.project, args).map(|(text, is_error)| ToolResult::text(text, is_error))?,
        "get_gpui_artifact" => {
            let id = str_arg(args, "id").context("`id` is required")?;
            let wait = Duration::from_secs(args["wait_seconds"].as_u64().unwrap_or(0));
            let (summary, source) = wait_for(&mut connect(&state.project)?, id, wait)?;
            let (mut text, is_error) = describe(&summary);
            if args["include_source"].as_bool() == Some(true) {
                text.push_str(&format!("\n\nSource:\n```rust\n{source}\n```"));
            }
            ToolResult::text(text, is_error)
        }
        "list_gpui_artifacts" => {
            let Response::List { artifacts } = connect(&state.project)?.request(&Request::List)? else {
                bail!("unexpected response from the viewer");
            };
            if artifacts.is_empty() {
                ToolResult::text("No artifacts yet.", false)
            } else {
                let lines: Vec<String> = artifacts
                    .iter()
                    .map(|a| format!("- `{}` \"{}\" v{} ({:?}): {}", a.id, a.title, a.version, a.mode, status_word(&a.status)))
                    .collect();
                ToolResult::text(lines.join("\n"), false)
            }
        }
        "await_user_message" => {
            let wait = Duration::from_secs(args["wait_seconds"].as_u64().unwrap_or(300));
            let deadline = Instant::now() + wait;
            let mut client = connect(&state.project)?;
            loop {
                let report = chat(&mut client, state.seen_message, true)?;
                if !report.messages.is_empty() {
                    return Ok(chat_result(state, report));
                }
                // Nothing undelivered, so everything up to `latest` is seen.
                state.seen_message = Some(state.seen_message.unwrap_or(0).max(report.latest));
                if Instant::now() >= deadline {
                    let drafts = report.drafts.len();
                    return Ok(ToolResult::text(
                        format!(
                            "No message within {}s ({drafts} attachment(s) are on the canvas but not sent yet). \
                             Call await_user_message again to keep waiting.",
                            wait.as_secs()
                        ),
                        false,
                    ));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        "send_chat_message" => {
            let text = str_arg(args, "text").context("`text` is required")?.to_string();
            match connect(&state.project)?.request(&Request::Say { text })? {
                Response::Done => ToolResult::text("Posted to the chat.", false),
                Response::Error { message } => ToolResult::text(message, true),
                _ => bail!("unexpected response from the viewer"),
            }
        }
        "export_gpui_png" => {
            let id = str_arg(args, "id").context("`id` is required")?.to_string();
            let version = args["version"].as_u64().map(|v| v as u32);
            match connect(&state.project)?.request(&Request::Export { id: id.clone(), version })? {
                Response::Image { path } => {
                    let mut result = ToolResult::text(format!("PNG of `{id}` saved to {}", path.display()), false);
                    result.content.extend(image_block(&path));
                    result
                }
                Response::Error { message } => ToolResult::text(message, true),
                _ => bail!("unexpected response from the viewer"),
            }
        }
        other => bail!("unknown tool `{other}`"),
    };
    if name != "await_user_message"
        && let Ok(mut client) = Client::connect(&easel_protocol::socket_path(&state.project))
        && let Ok(report) = chat(&mut client, state.seen_message, false)
        && (!report.messages.is_empty() || !report.drafts.is_empty())
    {
        result.content.extend(chat_result(state, report).content);
    }
    Ok(result)
}

fn chat(client: &mut Client, after: Option<u64>, listening: bool) -> Result<ChatReport> {
    match client.request(&Request::Chat { after, listening })? {
        Response::Chat { report } => Ok(report),
        Response::Error { message } => bail!(message),
        _ => bail!("unexpected response from the viewer"),
    }
}

fn anchor_text(anchor: &Anchor) -> String {
    let mut text = anchor.element.clone();
    if let Some(line) = anchor.line {
        text.push_str(&format!(" at line {line}"));
        if !anchor.source_line.is_empty() {
            text.push_str(&format!(" (`{}`)", anchor.source_line));
        }
    }
    if !anchor.text.is_empty() {
        text.push_str(&format!(", containing \"{}\"", anchor.text));
    }
    text
}

/// Formats new user messages (and unsent attachments) for Claude and marks
/// the messages as seen.
fn chat_result(state: &mut State, report: ChatReport) -> ToolResult {
    let mut content = Vec::new();
    for message in &report.messages {
        let mut text = String::from("## Message from the user in the GPUI viewer\n");
        if message.text.is_empty() {
            text.push_str("(no text, only attachments)\n");
        } else {
            text.push_str(&format!("{}\n", message.text));
        }
        if !message.attachments.is_empty() {
            text.push_str("\nAttachments:\n");
            for attachment in &message.attachments {
                text.push_str(&format!("{}\n", attachment_line(attachment)));
            }
        }
        content.push(json!({ "type": "text", "text": text }));
        for capture in &message.captures {
            content.push(json!({
                "type": "text",
                "text": format!("Annotated frame `{}` v{}:", capture.artifact_id, capture.version)
            }));
            content.extend(image_block(&capture.path));
        }
    }
    if !report.messages.is_empty() {
        content.push(json!({
            "type": "text",
            "text": "Line numbers are in the artifact's source. Answer with send_chat_message, and render changes with \
                     render_gpui using the same id (it adds a new version)."
        }));
    }
    if !report.drafts.is_empty() {
        let mut text = format!(
            "\nThe user has {} attachment(s) on the canvas that they haven't sent yet (they may still change):\n",
            report.drafts.len()
        );
        for attachment in &report.drafts {
            text.push_str(&format!("{}\n", attachment_line(attachment)));
        }
        content.push(json!({ "type": "text", "text": text }));
    }
    state.seen_message = Some(state.seen_message.unwrap_or(0).max(report.latest));
    ToolResult { content, is_error: false }
}

fn attachment_line(attachment: &Attachment) -> String {
    let target = match (&attachment.artifact_id, attachment.version) {
        (Some(id), Some(v)) => format!("`{id}` v{v}"),
        (Some(id), None) => format!("`{id}`"),
        _ => "the canvas".into(),
    };
    let n = attachment.number;
    match &attachment.kind {
        AttachmentKind::Marker { note, anchor } => {
            let on = anchor.as_ref().map_or(String::new(), |a| format!(" on {}", anchor_text(a)));
            let note = if note.is_empty() { String::new() } else { format!(": \"{note}\"") };
            format!("{n}. Marker on {target}{on}{note}")
        }
        AttachmentKind::Drawing { shape, anchor } => {
            let near = match (shape.as_str(), anchor) {
                ("arrow", Some(a)) => format!(", pointing at {}", anchor_text(a)),
                (_, Some(a)) => format!(", around {}", anchor_text(a)),
                _ => String::new(),
            };
            let article = if shape.starts_with(['a', 'e', 'i', 'o', 'u']) { "An" } else { "A" };
            format!("{n}. {article} {shape} drawn on {target}{near}")
        }
        AttachmentKind::Note { text } => format!("{n}. Sticky note near {target}: \"{text}\""),
    }
}

/// An MCP image content block for a PNG, if it can be read.
fn image_block(path: &std::path::Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    Some(json!({ "type": "image", "data": base64(&bytes), "mimeType": "image/png" }))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().filter(|s| !s.trim().is_empty())
}

fn render(project: &Path, args: &Value) -> Result<(String, bool)> {
    let source = str_arg(args, "source").context("`source` is required")?;
    let title = str_arg(args, "title").unwrap_or("Untitled");
    let id = str_arg(args, "id").map(str::to_string).unwrap_or_else(|| slug(title));
    let mode = match args["mode"].as_str() {
        None | Some("auto") => Mode::Auto,
        Some("interpret") => Mode::Interpret,
        Some("compile") => Mode::Compile,
        Some(other) => bail!("unknown mode `{other}`"),
    };
    let wait = Duration::from_secs(args["wait_seconds"].as_u64().unwrap_or(90));

    let mut client = match connect(project) {
        Ok(client) => client,
        Err(err) => return Ok(check_without_viewer(source, &err)),
    };
    let mut frame = match args["preset"].as_str() {
        Some(name) => FrameSize::preset(name).with_context(|| format!("unknown preset `{name}`"))?,
        None => FrameSize::default(),
    };
    frame.width = args["width"].as_f64().map(|w| w as f32).or(frame.width);
    frame.height = args["height"].as_f64().map(|h| h as f32).or(frame.height);
    let artifact = Artifact {
        id: id.clone(),
        title: title.to_string(),
        source: source.to_string(),
        mode,
        frame,
    };
    let Response::Artifact { summary, .. } = client.request(&Request::Upsert { artifact })? else {
        bail!("unexpected response from the viewer");
    };
    if !summary.status.is_pending() {
        return Ok(describe(&summary));
    }
    // Compiling: wait for the user's decision and the build.
    let (summary, _) = wait_for(&mut client, &id, wait)?;
    Ok(describe(&summary))
}

/// Without a viewer, still tell Claude whether the code interprets.
fn check_without_viewer(source: &str, err: &anyhow::Error) -> (String, bool) {
    let check = match easel_interp::interpret(source) {
        Ok(_) => "The interpreter accepts this code.".to_string(),
        Err(diagnostics) => {
            let list: Vec<String> = diagnostics.iter().map(|d| format!("- {d}")).collect();
            format!("Interpreter diagnostics:\n{}", list.join("\n"))
        }
    };
    (format!("The viewer isn't available ({err:#}), so nothing was shown.\n{check}"), true)
}

/// Polls an artifact until it's no longer pending or `wait` elapses.
fn wait_for(client: &mut Client, id: &str, wait: Duration) -> Result<(Summary, String)> {
    let deadline = Instant::now() + wait;
    loop {
        let (summary, source) = match client.request(&Request::Get { id: id.to_string() })? {
            Response::Artifact { summary, source } => (summary, source),
            Response::Error { message } => bail!(message),
            _ => bail!("unexpected response from the viewer"),
        };
        if !summary.status.is_pending() || Instant::now() >= deadline {
            return Ok((summary, source));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn status_word(status: &Status) -> &'static str {
    match status {
        Status::Rendered => "rendered (interpreted)",
        Status::Invalid { .. } => "has errors",
        Status::AwaitingApproval { .. } => "waiting for the user to approve compiling",
        Status::Building => "building",
        Status::Running => "running (compiled)",
        Status::BuildFailed { .. } => "build failed",
        Status::Exited { .. } => "exited",
        Status::Declined => "declined by the user",
    }
}

/// A report for Claude, and whether it's an error.
fn describe(summary: &Summary) -> (String, bool) {
    let name = format!("`{}` (\"{}\", v{})", summary.id, summary.title, summary.version);
    match &summary.status {
        Status::Rendered => (format!("Rendered {name} in the viewer with the interpreter."), false),
        Status::Invalid { diagnostics } => {
            let list: Vec<String> = diagnostics.iter().map(|d| format!("- {d}")).collect();
            let hint = if summary.mode == Mode::Interpret {
                " (mode is `interpret`; use `auto` to compile unsupported code)"
            } else {
                ""
            };
            (format!("{name} has errors{hint}:\n{}", list.join("\n")), true)
        }
        Status::AwaitingApproval { reasons } => {
            let list: Vec<String> = reasons.iter().map(|d| format!("- {d}")).collect();
            let why = if list.is_empty() { String::new() } else { format!(" because the interpreter can't run:\n{}\n", list.join("\n")) };
            (
                format!(
                    "{name} needs compiling{why}\nIt's waiting for the user to click Build & Run in the viewer. \
                     Check back with get_gpui_artifact (use wait_seconds to wait for the build)."
                ),
                false,
            )
        }
        Status::Building => (format!("{name} is still building. Check back with get_gpui_artifact."), false),
        Status::Running => (format!("{name} compiled and is running in its own window."), false),
        Status::BuildFailed { errors } => {
            let mut text = format!("{name} failed to compile. Line numbers refer to your snippet:\n");
            for error in errors {
                text.push_str(if error.rendered.is_empty() { &error.message } else { &error.rendered });
                text.push('\n');
            }
            (truncate(text, 6000), true)
        }
        Status::Exited { code, stderr_tail } => {
            let failed = code.is_some_and(|c| c != 0);
            let mut text = match code {
                Some(code) => format!("{name} ran and exited with code {code}."),
                None => format!("{name} was stopped."),
            };
            if !stderr_tail.trim().is_empty() {
                text.push_str(&format!("\nstderr (tail):\n{}", truncate(stderr_tail.clone(), 3000)));
            }
            (text, failed)
        }
        Status::Declined => (format!("The user declined to run {name}."), false),
    }
}

fn truncate(mut text: String, max: usize) -> String {
    if text.len() > max {
        let cut = (0..=max).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
        text.truncate(cut);
        text.push_str("\n… (truncated)");
    }
    text
}

fn slug(title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    if slug.is_empty() { "artifact".into() } else { slug }
}

/// Reuses the project's socket, else opens a pane beside this terminal in
/// Herdr. Outside Herdr (or with an explicit viewer override), starts the viewer.
fn connect(project: &Path) -> Result<Client> {
    let socket = easel_protocol::socket_path(project);
    if let Ok(client) = Client::connect(&socket) {
        // A mod backend can briefly outlive its pane. Reopen it when the
        // previous copy closed; native viewers continue using their own UI.
        if socket.with_extension("mod.sock").exists() {
            if std::env::var_os("GPUI_ARTIFACTS_VIEWER").is_some() {
                bail!("This project is open in the Easel mod. Close its pane and wait about 30 seconds for its backend to exit before opening the standalone viewer.");
            }
            let helper = mod_helper_binary();
            let attached = Command::new(&helper).args(["call"]).arg(project)
                .arg(r#"{"action":"ping"}"#).stdin(Stdio::null()).output().ok()
                .filter(|r| r.status.success()).and_then(|r| serde_json::from_slice::<Value>(&r.stdout).ok())
                .is_some_and(|v| v["attached"] == true);
            if !attached {
                let targets = host_targets();
                let herdr = std::env::var_os("GPUI_ARTIFACTS_HERDR").unwrap_or_else(|| "herdr-app".into());
                if !open_in_herdr(project, &herdr, &targets)? {
                    bail!("The Easel mod backend is still running without a pane. Reopen its mod pane in Herdr, or wait about 30 seconds for it to exit before opening the standalone viewer.");
                }
            }
        }
        return Ok(client);
    }
    if std::env::var_os("GPUI_ARTIFACTS_VIEWER").is_none() {
        let targets = host_targets();
        let herdr = std::env::var_os("GPUI_ARTIFACTS_HERDR").unwrap_or_else(|| "herdr-app".into());
        if open_in_herdr(project, &herdr, &targets)? {
            return wait_for_socket(&socket, "opened an Easel pane in Herdr");
        }
    }
    let viewer = viewer_binary();
    let log = log_file();
    let mut child = Command::new(&viewer)
        .arg("--project")
        .arg(project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log.map_or_else(Stdio::null, Stdio::from))
        .process_group(0) // outlive the MCP server
        .spawn()
        .with_context(|| format!("starting the viewer ({})", viewer.display()))?;
    // Reap it if it closes first, so it doesn't linger as a zombie.
    std::thread::spawn(move || child.wait());
    wait_for_socket(&socket, "started the viewer")
}

fn host_targets() -> Vec<String> {
    ["HC_TERMINAL_ID", "REX_BLOCK"].into_iter()
        .filter_map(|key| std::env::var(key).ok()).filter(|s| !s.is_empty()).collect()
}

fn mod_helper_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("GPUI_ARTIFACTS_MOD_HELPER") { return path.into(); }
    if let Some(sibling) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.join("gpui-artifacts-mod")))
        && sibling.is_file() { return sibling; }
    "gpui-artifacts-mod".into()
}

/// Bundle the mod with the MCP binary, but preserve an installed personal copy.
#[cfg(not(test))]
fn install_mod() -> Result<()> {
    let home = std::env::var_os("HERDR_APP_HOME").map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(|p| PathBuf::from(p).join("herdr-app")))
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config/herdr-app")))
        .context("cannot locate Herdr's mod directory; set HERDR_APP_HOME")?;
    let dir = home.join("mods/easel");
    std::fs::create_dir_all(&dir)?;
    for (name, source) in [
        ("mod.json", include_str!("../../../mods/easel/mod.json")),
        ("main.js", include_str!("../../../mods/easel/main.js")),
        ("renderer.js", include_str!("../../../mods/easel/renderer.js")),
    ] {
        use std::io::Write;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(name)) {
            Ok(mut file) => file.write_all(source.as_bytes())?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn wait_for_socket(socket: &Path, action: &str) -> Result<Client> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(client) = Client::connect(socket) {
            return Ok(client);
        }
    }
    bail!("{action}, but could not connect to {}. Check herdr-app status or the viewer log.", socket.display())
}

/// Only an explicitly identified, visible terminal may choose the pane. Never
/// use the person's current focus: it may belong to a different agent/project.
fn terminal_in_window(list: &Value, targets: &[String]) -> Option<String> {
    let terminals: Vec<&Value> = list["spaces"].as_array().into_iter().flatten()
        .flat_map(|space| space["tabs"].as_array().into_iter().flatten())
        .flat_map(|tab| tab["terminals"].as_array().into_iter().flatten()).collect();
    // Prefer exact addresses, including hc's explicit machine/address values.
    for target in targets {
        if let Some(pane) = terminals.iter().find(|t| t["terminal"].as_str() == Some(target.as_str()))
            .and_then(|t| t["pane"].as_str()) { return Some(pane.to_owned()); }
    }
    // A Rex process knows its raw REX_BLOCK; the stock hc bridge qualifies it
    // as machine/block. Accept that only when it identifies one pane uniquely.
    for target in targets {
        let matches: Vec<&str> = terminals.iter().filter(|t| t["terminal"].as_str()
            .and_then(|id| id.split_once('/').map(|(_, local)| local)) == Some(target.as_str()))
            .filter_map(|t| t["pane"].as_str()).collect();
        if matches.len() == 1 { return Some(matches[0].to_owned()); }
    }
    None
}

fn open_in_herdr(project: &Path, binary: &std::ffi::OsStr, targets: &[String]) -> Result<bool> {
    if targets.is_empty() { return Ok(false); }
    let listing = match Command::new(binary).arg("ls").stdin(Stdio::null()).output() {
        Ok(output) if output.status.success() => output,
        _ => return Ok(false), // No running Herdr: retain the standalone workflow.
    };
    let list: Value = serde_json::from_slice(&listing.stdout).context("reading herdr-app ls")?;
    let Some(pane) = terminal_in_window(&list, targets) else { return Ok(false); };
    // Tests use a fake CLI; installing is exercised by the real integration.
    #[cfg(not(test))]
    install_mod()?;
    // The MCP server's project can be an agent worktree even when its shell
    // stays on main. Always pass it instead of inferring it from the pane cwd.
    let args = json!({ "pane": pane, "direction": "right", "mod": "easel", "props": { "project": project, "helper": mod_helper_binary() } });
    // New mod folders are discovered asynchronously by Herdr. Retry only the
    // explicit missing-mod rejection: it has not created a pane, so it is safe.
    // Never retry other errors or a successful response (that could duplicate it).
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let output = Command::new(binary).args(["do", "pane.split", &args.to_string()])
            .stdin(Stdio::null()).output().context("asking Herdr to open Easel")?;
        let reply = serde_json::from_slice::<Value>(&output.stdout).ok();
        let missing = reply.as_ref().and_then(|r| r["error"].as_str())
            .is_some_and(|e| e == "no mod \"easel\" in mods/");
        if missing && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if !output.status.success() {
            bail!("Herdr could not open the Easel mod: {} {}. Install gpui-artifacts-mod, or set GPUI_ARTIFACTS_VIEWER for a standalone window.",
                String::from_utf8_lossy(&output.stdout).trim(), String::from_utf8_lossy(&output.stderr).trim());
        }
        let reply = reply.context("reading pane.split reply")?;
        if reply["pane"].as_str().is_none() { bail!("Herdr did not return an Easel pane: {reply}"); }
        break;
    }
    Ok(true)
}

fn viewer_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("GPUI_ARTIFACTS_VIEWER") {
        return path.into();
    }
    // Installed or built next to this binary.
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("gpui-artifacts-viewer")))
        .filter(|path| path.exists())
        .unwrap_or_else(|| "gpui-artifacts-viewer".into())
}

fn log_file() -> Option<std::fs::File> {
    let dir = artifact_protocol_cache_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::File::create(dir.join("viewer.log")).ok()
}

fn artifact_protocol_cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .map(|cache| cache.join("gpui-artifacts"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_uses_the_callers_terminal_not_the_active_tab() {
        let list = json!({"spaces":[{"tabs":[
            {"active":true,"terminals":[{"terminal":"t9","pane":"p9"}]},
            {"active":false,"terminals":[{"terminal":"t2","pane":"p2"},{"terminal":"block:rex","pane":"p3"}]}
        ]}]});
        assert_eq!(terminal_in_window(&list, &["t2".into()]), Some("p2".into()));
        assert_eq!(terminal_in_window(&list, &["missing".into(), "block:rex".into()]), Some("p3".into()));
        assert_eq!(terminal_in_window(&list, &["hidden-child".into()]), None);
        assert_eq!(terminal_in_window(&list, &[]), None);
    }

    #[test]
    fn rex_bridge_addresses_resolve_only_when_the_caller_is_unambiguous() {
        let list = json!({"spaces":[{"tabs":[{"terminals":[
            {"terminal":"rex/block:one","pane":"rex/p1"},
            {"terminal":"other/block:two","pane":"other/p2"},
            {"terminal":"rex/block:two","pane":"rex/p2"}
        ]}]}]});
        assert_eq!(terminal_in_window(&list, &["block:one".into()]), Some("rex/p1".into()));
        assert_eq!(terminal_in_window(&list, &["block:two".into()]), None);
        assert_eq!(terminal_in_window(&list, &["rex/block:two".into()]), Some("rex/p2".into()));
    }

    #[test]
    fn host_open_passes_explicit_worktree_and_checks_errors() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("easel-host-test-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("herdr-test");
        let script = r#"#!/bin/sh
if [ "$1" = ls ]; then
  printf '%s\n' '{"spaces":[{"tabs":[{"terminals":[{"terminal":"t2","pane":"p2"}]}]}]}'
else
  printf '%s\n' "$@" > "$0.args"
  printf '%s\n' '{"pane":"e1"}'
fi
"#;
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let project = Path::new("/worktrees/feature with spaces; $(no-shell)");
        assert!(open_in_herdr(project, binary.as_os_str(), &["t2".into()]).unwrap());
        let calls = std::fs::read_to_string(dir.join("herdr-test.args")).unwrap();
        let args: Vec<_> = calls.lines().collect();
        assert_eq!(&args[..2], &["do", "pane.split"]);
        assert_eq!(serde_json::from_str::<Value>(args[2]).unwrap(), json!({"pane":"p2","direction":"right","mod":"easel","props":{"project":project,"helper":mod_helper_binary()}}));
        std::fs::remove_file(dir.join("herdr-test.args")).unwrap();
        assert!(!open_in_herdr(project, binary.as_os_str(), &["other".into()]).unwrap());
        assert!(!dir.join("herdr-test.args").exists());
        std::fs::write(&binary, script.replace("'{\"pane\":\"e1\"}'", "'{\"error\":\"unsupported command\"}'; exit 1")).unwrap();
        assert!(open_in_herdr(project, binary.as_os_str(), &["t2".into()]).unwrap_err().to_string().contains("unsupported command"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn first_open_waits_for_the_new_mod_folder_without_duplicating_panes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("easel-first-open-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("herdr-test");
        std::fs::write(&binary, r#"#!/bin/sh
if [ "$1" = ls ]; then
  printf '%s\n' '{"spaces":[{"tabs":[{"terminals":[{"terminal":"t2","pane":"p2"}]}]}]}'
elif [ ! -e "$0.discovered" ]; then
  touch "$0.discovered"
  printf '%s\n' '{"error":"no mod \"easel\" in mods/"}'
  exit 1
else
  printf '%s\n' created >> "$0.created"
  printf '%s\n' '{"pane":"e1"}'
fi
"#).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(open_in_herdr(Path::new("/tmp/project"), binary.as_os_str(), &["t2".into()]).unwrap());
        assert_eq!(std::fs::read_to_string(dir.join("herdr-test.created")).unwrap(), "created\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Settings Card (v2)"), "settings-card-v2");
        assert_eq!(slug("!!!"), "artifact");
    }
}
