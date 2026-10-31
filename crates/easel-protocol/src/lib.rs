//! Messages between the MCP server and the viewer: newline-delimited JSON over
//! a Unix socket, one [`Response`] line per [`Request`] line.

use easel_interp::Diagnostic;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{ffi::OsStrExt, net::UnixStream},
    path::{Path, PathBuf},
    time::Duration,
};

/// Where a project keeps its canvas, in the project root.
pub const PROJECT_DIR: &str = ".gpui-artifacts";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub title: String,
    pub source: String,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub frame: FrameSize,
}

/// A frame's size on the canvas. Unset dimensions hug the content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameSize {
    pub width: Option<f32>,
    pub height: Option<f32>,
}

impl FrameSize {
    pub fn preset(name: &str) -> Option<Self> {
        let (w, h) = match name {
            "desktop" => (1280., 800.),
            "laptop" => (1440., 900.),
            "tablet" => (834., 1112.),
            "mobile" => (390., 844.),
            "hug" => return Some(Self::default()),
            _ => return None,
        };
        Some(Self { width: Some(w), height: Some(h) })
    }
}

/// Which element a piece of feedback is about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    /// Child indices from the root element.
    pub path: Vec<usize>,
    /// `div`, `img`, `svg` or `text`.
    pub element: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    /// The source line that created the element.
    pub source_line: String,
    /// Text inside the element, abbreviated.
    pub text: String,
}

/// Something drawn or placed on the canvas, sent with a chat message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    /// Shown on the canvas and the chip (①, ②, …); unique for the session.
    pub number: u64,
    pub artifact_id: Option<String>,
    pub version: Option<u32>,
    #[serde(flatten)]
    pub kind: AttachmentKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachmentKind {
    /// A marker dropped on an element, with an optional note.
    Marker { note: String, anchor: Option<Anchor> },
    /// A pen stroke, arrow or box; the capture shows it.
    Drawing {
        shape: String,
        /// The element under an arrow's tip, or a stroke's or box's middle.
        anchor: Option<Anchor>,
    },
    /// A sticky note on the canvas.
    Note { text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    User,
    Claude,
    /// Things that happened, e.g. a new version rendered.
    Event,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: u64,
    pub author: Author,
    pub text: String,
    pub attachments: Vec<Attachment>,
    /// Annotated frames for the attachments.
    pub captures: Vec<Capture>,
}

/// A PNG of a frame with its annotations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capture {
    pub artifact_id: String,
    pub version: u32,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChatReport {
    /// The newest user message that's ready (its captures are done).
    pub latest: u64,
    /// Ready user messages newer than the requested id.
    pub messages: Vec<ChatMessage>,
    /// Attachments on the canvas that haven't been sent yet.
    pub drafts: Vec<Attachment>,
}

/// How an artifact is turned into UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Interpret; compile only if the interpreter reports unsupported code.
    #[default]
    Auto,
    Interpret,
    Compile,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Interpreted and shown in the viewer.
    Rendered,
    /// The interpreter rejected the code and compiling wouldn't help (or the
    /// mode is `interpret`).
    Invalid { diagnostics: Vec<Diagnostic> },
    /// Needs compiling; waiting for the user to approve running it.
    AwaitingApproval { reasons: Vec<Diagnostic> },
    Building,
    /// Compiled and running in its own window.
    Running,
    BuildFailed { errors: Vec<CompilerMessage> },
    /// The compiled program exited or couldn't start.
    Exited { code: Option<i32>, stderr_tail: String },
    Declined,
}

impl Status {
    /// Whether the status will change without further input.
    pub fn is_pending(&self) -> bool {
        matches!(self, Status::AwaitingApproval { .. } | Status::Building)
    }
}

/// A rustc error or warning, trimmed for an LLM to act on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompilerMessage {
    pub level: String,
    pub message: String,
    /// Location in the submitted snippet, when it maps back to it.
    pub line: Option<usize>,
    pub column: Option<usize>,
    /// rustc's rendered output, with snippet line numbers.
    pub rendered: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Summary {
    pub id: String,
    pub title: String,
    pub version: u32,
    pub mode: Mode,
    pub status: Status,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// Creates or replaces an artifact and shows it.
    Upsert { artifact: Artifact },
    Get { id: String },
    List,
    /// User messages after `after`, plus unsent attachments. Without
    /// `after`, the messages no client has been given yet. `listening` means
    /// Claude is waiting for a message, which the chat shows.
    Chat {
        #[serde(default)]
        after: Option<u64>,
        #[serde(default)]
        listening: bool,
    },
    /// Claude posts to the chat.
    Say { text: String },
    /// Captures a frame (the latest version if unset) as a PNG.
    Export { id: String, version: Option<u32> },
    /// Brings the viewer's window to the front.
    Activate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Artifact { summary: Summary, source: String },
    List { artifacts: Vec<Summary> },
    Chat { report: ChatReport },
    Image { path: PathBuf },
    Done,
    Error { message: String },
}

/// The project a directory belongs to: the nearest ancestor that already has
/// a canvas, else the enclosing git repository, else the directory itself.
pub fn project_root(dir: &Path) -> PathBuf {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let found = dir
        .ancestors()
        .find(|d| d.join(PROJECT_DIR).is_dir())
        .or_else(|| dir.ancestors().find(|d| d.join(".git").exists()));
    found.map_or_else(|| dir.clone(), Path::to_path_buf)
}

/// One socket per project, so each project gets its own viewer:
/// `$XDG_RUNTIME_DIR/gpui-artifacts-<hash>.sock`.
pub fn socket_path(project: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("GPUI_ARTIFACTS_SOCKET") {
        return path.into();
    }
    // FNV-1a: stable across builds, unlike std's hasher, since the MCP server
    // and the viewer must agree.
    let hash = project.as_os_str().as_bytes().iter().fold(0xcbf29ce484222325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    let name = format!("gpui-artifacts-{hash:016x}.sock");
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join(name),
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            std::env::temp_dir().join(format!("{user}-{name}"))
        }
    }
}

/// A blocking connection to the viewer.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    pub fn connect(socket: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        // Captures wait for repaints and the compositor.
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        })
    }

    pub fn request(&mut self, request: &Request) -> std::io::Result<Response> {
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        let mut reply = String::new();
        if self.reader.read_line(&mut reply)? == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "viewer closed the connection"));
        }
        Ok(serde_json::from_str(&reply)?)
    }
}

/// Reads requests from one connection and writes `handle`'s responses.
pub fn serve_connection(stream: UnixStream, mut handle: impl FnMut(Request) -> Response) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handle(request),
            Err(err) => Response::Error {
                message: format!("bad request: {err}"),
            },
        };
        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        writer.write_all(out.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_over_a_socket_pair() {
        let (server, client) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            serve_connection(server, |request| match request {
                Request::Ping => Response::Pong,
                _ => Response::Error { message: "unexpected".into() },
            })
        });
        let mut client = Client {
            reader: BufReader::new(client.try_clone().unwrap()),
            writer: client,
        };
        assert!(matches!(client.request(&Request::Ping).unwrap(), Response::Pong));
        assert!(matches!(client.request(&Request::List).unwrap(), Response::Error { .. }));
        drop(client);
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn projects_resolve_to_their_canvas_or_repository() {
        let base = std::env::temp_dir().join(format!("gpui-artifacts-root-{}", std::process::id()));
        let nested = base.join("repo/crates/app");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(base.join("repo/.git")).unwrap();
        let base = base.canonicalize().unwrap();
        assert_eq!(project_root(&nested), base.join("repo"));
        // An existing canvas wins over the repository root.
        std::fs::create_dir_all(base.join("repo/crates").join(PROJECT_DIR)).unwrap();
        assert_eq!(project_root(&nested), base.join("repo/crates"));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn chat_requests_may_omit_after() {
        let request: Request = serde_json::from_str(r#"{"type":"chat"}"#).unwrap();
        assert!(matches!(request, Request::Chat { after: None, listening: false }));
    }

    #[test]
    fn mode_defaults_to_auto() {
        let artifact: Artifact = serde_json::from_str(r#"{"id":"a","title":"A","source":"div()"}"#).unwrap();
        assert_eq!(artifact.mode, Mode::Auto);
    }
}
