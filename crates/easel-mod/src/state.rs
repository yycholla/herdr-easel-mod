use anyhow::{Context, Result, bail};
use easel_interp::{Diagnostic, DiagnosticKind, ir::Node};
use easel_protocol::{
    Anchor, Artifact, Attachment, AttachmentKind, Author, ChatMessage, ChatReport, Mode,
    PROJECT_DIR, Request, Response, Status, Summary,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Read,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

struct Frame {
    artifact: Artifact,
    version: u32,
    status: Status,
    tree: Option<Node>,
}

pub struct State {
    project: PathBuf,
    // Preserve fields written by the native viewer, including its annotations and camera.
    saved: Value,
    frames: Vec<Frame>,
    pub revision: u64,
    touched: Instant,
    leases: HashMap<String, Instant>,
    had_lease: bool,
}

fn evaluate(artifact: Artifact, version: u32) -> Frame {
    let (status, tree) = if artifact.mode == Mode::Compile {
        (Status::AwaitingApproval { reasons: vec![] }, None)
    } else {
        match easel_interp::interpret(&artifact.source) {
            Ok(program) => (Status::Rendered, Some(program.root)),
            Err(diagnostics) => {
                let unsupported = diagnostics
                    .iter()
                    .any(|d| d.kind == DiagnosticKind::Unsupported);
                let syntax = diagnostics.iter().any(|d| d.kind == DiagnosticKind::Syntax);
                if artifact.mode == Mode::Auto && unsupported && !syntax {
                    (
                        Status::AwaitingApproval {
                            reasons: diagnostics,
                        },
                        None,
                    )
                } else {
                    (Status::Invalid { diagnostics }, None)
                }
            }
        }
    };
    Frame {
        artifact,
        version,
        status,
        tree,
    }
}

impl State {
    pub fn load(project: &Path) -> Result<Self> {
        let path = project.join(PROJECT_DIR).join("state.json");
        let saved = match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str::<Value>(&text).context("reading saved Easel canvas")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({
                "format":1,"rows":[],"messages":[],"markers":[],"drawings":[],"notes":[],
                "next_number":0,"next_message":0,"delivered":0,"camera":null,"selected":null
            }),
            Err(e) => return Err(e.into()),
        };
        if !saved.is_object() || saved["format"].as_u64().is_none_or(|v| v > 1) {
            bail!("unsupported Easel state format");
        }
        let mut frames = Vec::new();
        for row in saved["rows"]
            .as_array()
            .context("saved rows must be an array")?
        {
            for f in row["frames"]
                .as_array()
                .context("saved frames must be an array")?
            {
                let relative = Path::new(f["source"].as_str().context("missing source path")?);
                if relative
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
                {
                    bail!("source path escapes canvas directory");
                }
                let artifact = Artifact {
                    id: row["id"].as_str().context("missing artifact id")?.into(),
                    title: f["title"].as_str().unwrap_or("Artifact").into(),
                    source: std::fs::read_to_string(project.join(PROJECT_DIR).join(relative))?,
                    mode: serde_json::from_value(f["mode"].clone())?,
                    frame: serde_json::from_value(f["frame"].clone())?,
                };
                frames.push(evaluate(
                    artifact,
                    f["version"]
                        .as_u64()
                        .context("missing version")?
                        .try_into()?,
                ));
            }
        }
        Ok(Self {
            project: project.into(),
            saved,
            frames,
            revision: 1,
            touched: Instant::now(),
            leases: HashMap::new(),
            had_lease: false,
        })
    }

    pub fn touch(&mut self) {
        self.touched = Instant::now();
    }
    pub fn touch_lease(&mut self, r: &Value) {
        self.touch();
        if let Some(lease) = r["lease"].as_str() {
            self.had_lease = true;
            if r["action"] == "close" {
                self.leases.remove(lease);
            } else {
                self.leases.insert(lease.into(), Instant::now());
            }
        }
    }
    pub fn idle(&mut self) -> bool {
        self.leases
            .retain(|_, seen| seen.elapsed() < Duration::from_secs(90));
        let running = self
            .frames
            .iter()
            .any(|f| matches!(f.status, Status::Building | Status::Running));
        !running
            && self.leases.is_empty()
            && self.touched.elapsed() > Duration::from_secs(if self.had_lease { 30 } else { 90 })
    }
    fn save(&mut self) -> Result<()> {
        let dir = self.project.join(PROJECT_DIR);
        std::fs::create_dir_all(&dir)?;
        let temporary = dir.join("state.json.mod-tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&self.saved)?)?;
        std::fs::rename(temporary, dir.join("state.json"))?;
        self.revision += 1;
        Ok(())
    }
    fn summary(frame: &Frame) -> Summary {
        Summary {
            id: frame.artifact.id.clone(),
            title: frame.artifact.title.clone(),
            version: frame.version,
            mode: frame.artifact.mode,
            status: frame.status.clone(),
        }
    }
    fn frame_index(&self, request: &Value) -> Result<usize> {
        let id = request["id"].as_str().context("missing artifact id")?;
        let version = request["version"].as_u64();
        self.frames
            .iter()
            .rposition(|f| f.artifact.id == id && version.is_none_or(|v| v == u64::from(f.version)))
            .context("artifact version not found")
    }
    pub fn snapshot(&self) -> Result<Value> {
        Ok(json!({"revision":self.revision,"project":self.project,
            "frames":self.frames.iter().map(|f| json!({"summary":Self::summary(f),"tree":f.tree,"frame":f.artifact.frame,"source":f.artifact.source})).collect::<Vec<_>>(),
            "messages":self.saved["messages"],"markers":self.saved["markers"],"drawings":self.saved["drawings"],"notes":self.saved["notes"],
            "drafts":self.drafts()?,"selected":self.saved["selected"],"camera":self.saved["camera"]}))
    }
    fn upsert(&mut self, artifact: Artifact) -> Result<Response> {
        let version = self
            .frames
            .iter()
            .filter(|f| f.artifact.id == artifact.id)
            .map(|f| f.version)
            .max()
            .unwrap_or(0)
            + 1;
        // Hex-encoding ids avoids source-path collisions ("a/b" and "a_b").
        let slug: String = artifact
            .id
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let relative = PathBuf::from("artifacts")
            .join(slug)
            .join(format!("v{version}.rs"));
        let path = self.project.join(PROJECT_DIR).join(&relative);
        std::fs::create_dir_all(path.parent().unwrap())?;
        // Never overwrite a version left on disk by an earlier save failure.
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(artifact.source.as_bytes())?;
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::AlreadyExists
                    && std::fs::read_to_string(&path)? == artifact.source => {}
            Err(e) => return Err(e.into()),
        }
        let row = json!({"version":version,"title":artifact.title,"mode":artifact.mode,"frame":artifact.frame,"source":relative});
        let rows = self.saved["rows"].as_array_mut().unwrap();
        if let Some(existing) = rows.iter_mut().find(|r| r["id"] == artifact.id) {
            existing["frames"].as_array_mut().unwrap().push(row);
        } else {
            rows.push(json!({"id":artifact.id,"frames":[row]}));
        }
        let frame = evaluate(artifact, version);
        let reply = Response::Artifact {
            summary: Self::summary(&frame),
            source: frame.artifact.source.clone(),
        };
        self.saved["selected"] = json!([frame.artifact.id, version]);
        self.frames.push(frame);
        self.save()?;
        Ok(reply)
    }
    fn post(&mut self, author: Author, text: String, attachments: Vec<Attachment>) -> Result<()> {
        let id = self.saved["next_message"].as_u64().unwrap_or(0) + 1;
        self.saved["next_message"] = json!(id);
        let message = ChatMessage {
            id,
            author,
            text,
            attachments,
            captures: vec![],
        };
        self.saved["messages"]
            .as_array_mut()
            .context("messages must be an array")?
            .push(serde_json::to_value(message)?);
        if author == Author::User {
            for name in ["markers", "drawings", "notes"] {
                for item in self.saved[name]
                    .as_array_mut()
                    .context("annotations must be arrays")?
                {
                    if item["message"].is_null() {
                        item["message"] = json!(id);
                    }
                }
            }
        }
        self.save()
    }
    fn drafts(&self) -> Result<Vec<Attachment>> {
        let mut out = Vec::new();
        for name in ["markers", "drawings", "notes"] {
            for item in self.saved[name]
                .as_array()
                .context("annotations must be arrays")?
            {
                if !item["message"].is_null() {
                    continue;
                }
                let key = if name == "notes" {
                    &item["near"]
                } else {
                    &item["frame"]
                };
                let kind = match name {
                    "markers" => AttachmentKind::Marker {
                        note: item["note"].as_str().unwrap_or_default().into(),
                        anchor: serde_json::from_value(item["anchor"].clone())?,
                    },
                    "notes" => AttachmentKind::Note {
                        text: item["text"].as_str().unwrap_or_default().into(),
                    },
                    _ => AttachmentKind::Drawing {
                        shape: format!(
                            "{} {}",
                            item["shape"]
                                .as_object()
                                .and_then(|o| o.keys().next())
                                .map(String::as_str)
                                .unwrap_or("drawing"),
                            item["shape"]
                        ),
                        anchor: serde_json::from_value(item["anchor"].clone())?,
                    },
                };
                out.push(Attachment {
                    number: item["number"]
                        .as_u64()
                        .context("missing annotation number")?,
                    artifact_id: key[0].as_str().map(str::to_string),
                    version: key[1].as_u64().map(|v| v as u32),
                    kind,
                });
            }
        }
        out.sort_by_key(|a| a.number);
        Ok(out)
    }
    fn chat(&mut self, after: Option<u64>) -> Result<Response> {
        let start = after.unwrap_or(self.saved["delivered"].as_u64().unwrap_or(0));
        let messages: Vec<ChatMessage> = serde_json::from_value(self.saved["messages"].clone())?;
        let latest = messages
            .iter()
            .filter(|m| m.author == Author::User)
            .map(|m| m.id)
            .max()
            .unwrap_or(0);
        let mut messages: Vec<_> = messages
            .into_iter()
            .filter(|m| m.author == Author::User && m.id > start)
            .collect();
        for capture in messages.iter_mut().flat_map(|m| &mut m.captures) {
            if capture.path.is_relative() {
                capture.path = self.project.join(PROJECT_DIR).join(&capture.path);
            }
        }
        let report = ChatReport {
            latest,
            messages,
            drafts: self.drafts()?,
        };
        if after.is_none() && latest > start {
            self.saved["delivered"] = json!(latest);
            self.save()?;
        }
        Ok(Response::Chat { report })
    }
    pub fn request(&mut self, request: Request) -> Response {
        self.touch();
        (|| -> Result<Response> { Ok(match request {
            Request::Ping | Request::Activate => Response::Pong,
            Request::Upsert { artifact } => return self.upsert(artifact),
            Request::Get { id } => {
                let f = &self.frames[self.frame_index(&json!({"id":id}))?];
                Response::Artifact { summary: Self::summary(f), source: f.artifact.source.clone() }
            }
            Request::List => {
                let mut latest = Vec::<Summary>::new();
                for f in &self.frames {
                    if let Some(old) = latest.iter_mut().find(|s| s.id == f.artifact.id) { *old = Self::summary(f); }
                    else { latest.push(Self::summary(f)); }
                }
                Response::List { artifacts: latest }
            }
            Request::Say { text } => { self.post(Author::Claude, text, vec![])?; Response::Done }
            Request::Chat { after, .. } => return self.chat(after),
            Request::Export { .. } => bail!("Herdr mods cannot capture GPUI frames. PNG export is available in the standalone viewer (GPUI_ARTIFACTS_VIEWER)."),
        }) })().unwrap_or_else(|e| Response::Error { message: format!("{e:#}") })
    }
    fn anchor(frame: &Frame, path: &[usize]) -> Option<Anchor> {
        let node = frame.tree.as_ref()?.at(path)?;
        let (element, source) = match node {
            Node::Element(el) => (el.kind.name(), el.source),
            Node::Text { .. } => (
                "text",
                match frame
                    .tree
                    .as_ref()?
                    .at(&path[..path.len().saturating_sub(1)])?
                {
                    Node::Element(el) => el.source,
                    _ => None,
                },
            ),
        };
        Some(Anchor {
            path: path.into(),
            element: element.into(),
            line: source.map(|s| s.line),
            column: source.map(|s| s.column),
            source_line: source
                .and_then(|s| frame.artifact.source.lines().nth(s.line.saturating_sub(1)))
                .unwrap_or_default()
                .trim()
                .into(),
            text: node.text_content().chars().take(240).collect(),
        })
    }
    pub fn action(&mut self, r: &Value) -> Result<Value> {
        self.touch_lease(r);
        match r["action"].as_str().unwrap_or_default() {
            "close" => {
                // Wake the closing copy's outstanding watch so it does not
                // extend our idle timeout by another long-poll interval.
                self.revision += 1;
                return Ok(json!({"ok":true,"attached":!self.leases.is_empty()}));
            }
            "ping" | "open" => {
                return Ok(json!({"ok":true,"attached":!self.leases.is_empty()}));
            }
            "send" => {
                let text = r["text"].as_str().unwrap_or_default().trim();
                let drafts = self.drafts()?;
                if !text.is_empty() || !drafts.is_empty() {
                    self.post(Author::User, text.into(), drafts)?;
                }
                return Ok(json!({"ok":true}));
            }
            "select" => {
                self.frame_index(r)?;
                self.saved["selected"] = json!([r["id"], r["version"]]);
            }
            "decline" => {
                let i = self.frame_index(r)?;
                self.frames[i].status = Status::Declined;
            }
            "render_error" => {
                let i = self.frame_index(r)?;
                let f = &mut self.frames[i];
                let diagnostic = Diagnostic {
                    kind: DiagnosticKind::Unsupported,
                    message: r["message"]
                        .as_str()
                        .unwrap_or("unsupported mod renderer call")
                        .into(),
                    line: 1,
                    column: 1,
                    snippet: String::new(),
                };
                f.status = if f.artifact.mode == Mode::Auto {
                    Status::AwaitingApproval {
                        reasons: vec![diagnostic],
                    }
                } else {
                    Status::Invalid {
                        diagnostics: vec![diagnostic],
                    }
                };
                f.tree = None;
            }
            "annotate" => {
                let i = self.frame_index(r)?;
                let frame = &self.frames[i];
                let path: Vec<usize> =
                    serde_json::from_value(r.get("path").cloned().unwrap_or(json!([])))?;
                let anchor = Self::anchor(frame, &path);
                let number = self.saved["next_number"].as_u64().unwrap_or(0) + 1;
                let mut value = json!({"number":number,"frame":[frame.artifact.id,frame.version],"anchor":anchor,"message":null});
                let name = match r["kind"].as_str() {
                    Some("note") => {
                        value["near"] = value["frame"].clone();
                        value["pos"] = r.get("pin").cloned().unwrap_or(json!({"x":0,"y":0}));
                        value["text"] = json!(r["note"].as_str().unwrap_or_default());
                        value.as_object_mut().unwrap().remove("frame");
                        value.as_object_mut().unwrap().remove("anchor");
                        "notes"
                    }
                    Some("drawing") => {
                        let shape = r["shape"].as_str().context("missing drawing shape")?;
                        if !["Pen", "Arrow", "Rect"].contains(&shape) {
                            bail!("invalid drawing shape");
                        }
                        value["shape"] = json!({shape:r["points"]});
                        "drawings"
                    }
                    _ => {
                        value["pin"] = r.get("pin").cloned().unwrap_or(json!({"x":0,"y":0}));
                        value["note"] = json!(r["note"].as_str().unwrap_or_default());
                        "markers"
                    }
                };
                self.saved[name].as_array_mut().unwrap().push(value);
                self.saved["next_number"] = json!(number);
            }
            "remove_annotation" => {
                for name in ["markers", "drawings", "notes"] {
                    self.saved[name]
                        .as_array_mut()
                        .unwrap()
                        .retain(|a| a["number"] != r["number"] || !a["message"].is_null());
                }
            }
            _ => bail!("unknown Easel mod action"),
        }
        self.save()?;
        Ok(json!({"ok":true}))
    }
    pub fn begin_build(&mut self, r: &Value) -> Result<(Artifact, u32, PathBuf)> {
        let i = self.frame_index(r)?;
        let f = &mut self.frames[i];
        if !matches!(f.status, Status::AwaitingApproval { .. }) {
            bail!("this version is not awaiting approval");
        }
        f.status = Status::Building;
        let work = (f.artifact.clone(), f.version, self.project.clone());
        self.revision += 1;
        Ok(work)
    }
}

pub fn build(shared: super::Shared, (artifact, version, project): (Artifact, u32, PathBuf)) {
    let set_status = |status: Status| {
        let mut state = shared.0.lock().unwrap();
        if let Some(frame) = state
            .frames
            .iter_mut()
            .find(|f| f.artifact.id == artifact.id && f.version == version)
        {
            frame.status = status;
        }
        state.revision += 1;
        shared.1.notify_all();
    };
    let result = (|| -> Result<()> {
        let sandbox = easel_compiler::Sandbox::open(easel_compiler::Sandbox::default_dir())?;
        let build_id = format!(
            "mod-{}-{version}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        );
        let outcome = sandbox.build(&artifact.source, &artifact.title, &build_id)?;
        match outcome {
            easel_compiler::BuildOutcome::Failed { errors } => {
                set_status(Status::BuildFailed { errors })
            }
            easel_compiler::BuildOutcome::Built { binary } => {
                let mut child = easel_compiler::run(&binary, &project, true)?;
                let stderr = child.stderr.take();
                let tail = std::thread::spawn(move || {
                    let mut tail = Vec::new();
                    if let Some(mut stderr) = stderr {
                        let mut buffer = [0; 4096];
                        while let Ok(n) = stderr.read(&mut buffer) {
                            if n == 0 {
                                break;
                            }
                            tail.extend_from_slice(&buffer[..n]);
                            if tail.len() > 4000 {
                                tail.drain(..tail.len() - 4000);
                            }
                        }
                    }
                    String::from_utf8_lossy(&tail).into_owned()
                });
                set_status(Status::Running);
                let status = child.wait()?;
                set_status(Status::Exited {
                    code: status.code(),
                    stderr_tail: tail.join().unwrap_or_default(),
                });
                let _ = std::fs::remove_file(binary);
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        set_status(Status::Exited {
            code: None,
            stderr_tail: format!("{e:#}"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "easel-mod-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    fn artifact(id: &str) -> Artifact {
        Artifact {
            id: id.into(),
            title: "Hello".into(),
            source: "div().child(\"Hi\")".into(),
            mode: Mode::Auto,
            frame: Default::default(),
        }
    }
    #[test]
    fn versions_annotations_chat_and_native_state_survive_reopen() {
        let project = project();
        let mut state = State::load(&project).unwrap();
        state.saved["future_native_field"] = json!({"keep":true});
        state.upsert(artifact("card")).unwrap();
        state.upsert(artifact("card")).unwrap();
        state
            .action(
                &json!({"action":"annotate","id":"card","version":2,"path":[0],"note":"Bigger"}),
            )
            .unwrap();
        state
            .action(&json!({"action":"send","text":"Please change"}))
            .unwrap();
        assert_eq!(state.drafts().unwrap().len(), 0);
        let mut reopened = State::load(&project).unwrap();
        assert_eq!(reopened.frames.len(), 2);
        assert_eq!(reopened.saved["future_native_field"]["keep"], true);
        let Response::Chat { report } = reopened.chat(None).unwrap() else {
            panic!()
        };
        assert_eq!(report.messages.len(), 1);
        let AttachmentKind::Marker { note, anchor } = &report.messages[0].attachments[0].kind
        else {
            panic!()
        };
        assert_eq!(note, "Bigger");
        assert_eq!(anchor.as_ref().unwrap().text, "Hi");
        let Response::Chat { report } = reopened.chat(None).unwrap() else {
            panic!()
        };
        assert!(report.messages.is_empty());
        let Response::Chat { report } = reopened.chat(Some(0)).unwrap() else {
            panic!()
        };
        assert_eq!(report.messages.len(), 1);
        std::fs::remove_dir_all(project).unwrap();
    }
    #[test]
    fn colliding_ids_have_independent_sources_and_compilation_waits_for_approval() {
        let project = project();
        let mut state = State::load(&project).unwrap();
        state.upsert(artifact("a/b")).unwrap();
        let mut other = artifact("a_b");
        other.source = "div().child(\"Different\")".into();
        other.mode = Mode::Compile;
        state.upsert(other).unwrap();
        assert!(matches!(
            state.frames[1].status,
            Status::AwaitingApproval { .. }
        ));
        state.begin_build(&json!({"id":"a_b","version":1})).unwrap();
        assert!(state.begin_build(&json!({"id":"a_b","version":1})).is_err());
        let reopened = State::load(&project).unwrap();
        assert_ne!(
            reopened.frames[0].artifact.source,
            reopened.frames[1].artifact.source
        );
        assert!(matches!(
            reopened.frames[1].status,
            Status::AwaitingApproval { .. }
        ));
        std::fs::remove_dir_all(project).unwrap();
    }
    #[test]
    fn refuses_future_formats_and_escaping_sources() {
        let project = project();
        std::fs::create_dir_all(project.join(PROJECT_DIR)).unwrap();
        let path = project.join(PROJECT_DIR).join("state.json");
        std::fs::write(&path, r#"{"format":2}"#).unwrap();
        assert!(State::load(&project).is_err());
        std::fs::write(
            &path,
            r#"{"format":1,"rows":[{"frames":[{"source":"../secret"}]}]}"#,
        )
        .unwrap();
        assert!(matches!(State::load(&project), Err(e) if e.to_string().contains("source path")));
        std::fs::remove_dir_all(project).unwrap();
    }
}
