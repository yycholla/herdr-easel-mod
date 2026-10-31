//! Builds snippets the interpreter can't handle in a cached sandbox crate,
//! and runs the result in its own window.

mod wrap;

pub use wrap::{Wrapped, wrap};

use anyhow::{Context as _, Result};
use easel_protocol::CompilerMessage;
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
};

/// Must match the gpui-pre the interpreter's tables were generated from.
const GPUI_VERSION: &str = "0.3.8";

/// Starts the sandbox from the workspace's resolution, so it gets the same
/// working dependency versions (e.g. the `libc` pin `xattr 0.2.3` needs).
const LOCKFILE: &str = include_str!("../../../Cargo.lock");

/// A crate under `~/.cache/gpui-artifacts/sandbox-gpui-pre-<version>` whose
/// `src/main.rs` is replaced for each build. Dependencies are compiled once
/// and reused. The directory is per version so an older install's sandbox
/// (and its lockfile) is left alone.
pub struct Sandbox {
    dir: PathBuf,
    /// One build at a time: they share `src/main.rs` and the target dir.
    lock: Mutex<()>,
}

#[derive(Debug)]
pub enum BuildOutcome {
    Built { binary: PathBuf },
    Failed { errors: Vec<CompilerMessage> },
}

impl Sandbox {
    pub fn default_dir() -> PathBuf {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        cache.join("gpui-artifacts").join(format!("sandbox-gpui-pre-{GPUI_VERSION}"))
    }

    /// Creates the sandbox crate if needed.
    pub fn open(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(dir.join("src"))?;
        fs::create_dir_all(dir.join("bin"))?;
        let manifest = format!(
            r#"[package]
name = "gpui-artifact"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
gpui = {{ package = "gpui-pre", version = "={GPUI_VERSION}" }}
gpui_platform = {{ package = "gpui-pre-platform", version = "={GPUI_VERSION}", features = ["font-kit", "x11", "wayland"] }}
anyhow = "1"

[profile.dev]
debug = 0

[profile.dev.package."*"]
opt-level = 2

[workspace]
"#
        );
        write_if_changed(&dir.join("Cargo.toml"), &manifest)?;
        if !dir.join("Cargo.lock").exists() {
            fs::write(dir.join("Cargo.lock"), LOCKFILE)?;
        }
        if !dir.join("src/main.rs").exists() {
            let hello = wrap("div().child(\"warm\")", "warm-up").map_err(anyhow::Error::msg)?;
            fs::write(dir.join("src/main.rs"), hello.code)?;
        }
        Ok(Self { dir, lock: Mutex::new(()) })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether dependencies have been built, i.e. builds will be fast.
    pub fn is_warm(&self) -> bool {
        self.dir.join("target/debug/gpui-artifact").exists()
    }

    /// Builds `source` and copies the binary to a path unique to this build,
    /// so rebuilding never overwrites a running program.
    pub fn build(&self, source: &str, title: &str, build_id: &str) -> Result<BuildOutcome> {
        let wrapped = match wrap(source, title) {
            Ok(wrapped) => wrapped,
            Err(message) => {
                return Ok(BuildOutcome::Failed {
                    errors: vec![CompilerMessage {
                        level: "error".into(),
                        message: message.clone(),
                        line: None,
                        column: None,
                        rendered: message,
                    }],
                });
            }
        };
        let snippet_lines = source.lines().count().max(1);
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // Native viewers and mod backends can build for different projects
        // concurrently, but all use this directory's source and target files.
        let process_lock = fs::OpenOptions::new().create(true).truncate(false).write(true)
            .open(self.dir.join("build.lock"))?;
        process_lock.lock().context("locking the compile sandbox")?;
        fs::write(self.dir.join("src/main.rs"), &wrapped.code)?;

        let mut child = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["build", "--message-format=json"])
            .current_dir(&self.dir)
            .env_remove("RUSTFLAGS") // changing flags would rebuild every dependency
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("running cargo")?;

        let mut errors = Vec::new();
        let mut executable = None;
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            let Ok(msg) = serde_json::from_str::<Value>(&line?) else {
                continue;
            };
            match msg["reason"].as_str() {
                Some("compiler-message") if is_ours(&msg) => {
                    if let Some(error) = compiler_message(&msg["message"], &wrapped, snippet_lines) {
                        errors.push(error);
                    }
                }
                Some("compiler-artifact") if is_ours(&msg) => {
                    executable = msg["executable"].as_str().map(PathBuf::from);
                }
                _ => {}
            }
        }
        let status = child.wait()?;

        match executable {
            Some(executable) if status.success() => {
                let binary = self.dir.join("bin").join(sanitize(build_id));
                fs::copy(&executable, &binary).context("copying the built binary")?;
                Ok(BuildOutcome::Built { binary })
            }
            _ => {
                if errors.is_empty() {
                    errors.push(CompilerMessage {
                        level: "error".into(),
                        message: format!("cargo build failed ({status}) without compiler errors"),
                        line: None,
                        column: None,
                        rendered: String::new(),
                    });
                }
                Ok(BuildOutcome::Failed { errors })
            }
        }
    }
}

fn write_if_changed(path: &Path, contents: &str) -> std::io::Result<()> {
    if fs::read_to_string(path).ok().as_deref() != Some(contents) {
        fs::write(path, contents)?;
    }
    Ok(())
}

fn is_ours(msg: &Value) -> bool {
    msg["package_id"].as_str().is_some_and(|id| id.contains("gpui-artifact"))
        || msg["target"]["name"] == "gpui-artifact"
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// An error with its location mapped from `main.rs` back to the snippet.
fn compiler_message(message: &Value, wrapped: &Wrapped, snippet_lines: usize) -> Option<CompilerMessage> {
    let level = message["level"].as_str()?;
    let text = message["message"].as_str()?;
    if level != "error" || text.starts_with("aborting due to") {
        return None;
    }
    let primary = message["spans"]
        .as_array()
        .and_then(|spans| spans.iter().find(|span| span["is_primary"] == true));
    let line = primary
        .and_then(|span| span["line_start"].as_u64())
        .and_then(|line| wrapped.snippet_line(line as usize, snippet_lines));
    let column = line.and(primary.and_then(|span| span["column_start"].as_u64()).map(|c| c as usize));
    let rendered = message["rendered"].as_str().unwrap_or_default();
    Some(CompilerMessage {
        level: level.into(),
        message: text.into(),
        line,
        column,
        rendered: remap_rendered(rendered, wrapped, snippet_lines),
    })
}

/// Rewrites rustc's `--> src/main.rs:L:C` and gutter line numbers to
/// snippet lines. Lines outside the snippet (generated code) keep their
/// `main.rs` numbers.
fn remap_rendered(rendered: &str, wrapped: &Wrapped, snippet_lines: usize) -> String {
    let map = |line: usize| wrapped.snippet_line(line, snippet_lines);
    let mut out = String::new();
    for text in rendered.lines() {
        let trimmed = text.trim_start();
        if let Some(location) = trimmed.strip_prefix("--> src/main.rs:") {
            let mut parts = location.splitn(2, ':');
            let line = parts.next().and_then(|l| l.parse().ok());
            let rest = parts.next().unwrap_or("");
            let indent = &text[..text.len() - trimmed.len()];
            match line.and_then(map) {
                Some(line) => out.push_str(&format!("{indent}--> snippet:{line}:{rest}")),
                None => out.push_str(&format!("{indent}--> generated wrapper:{}:{rest}", line.unwrap_or(0))),
            }
        } else if let Some((gutter, rest)) = text.split_once(" |")
            && let Ok(line) = gutter.trim().parse::<usize>()
        {
            let line = map(line).map_or_else(|| format!("~{line}"), |l| l.to_string());
            out.push_str(&format!("{line:>width$} |{rest}", width = gutter.len()));
        } else {
            out.push_str(text);
        }
        out.push('\n');
    }
    out
}

/// Runs a built artifact. With `isolate`, it runs under bubblewrap with no
/// network, a read-only filesystem and `$HOME` hidden, keeping only what a
/// GUI needs: the display sockets and the GPU.
pub fn run(binary: &Path, working_dir: &Path, isolate: bool) -> Result<Child> {
    let mut command = if isolate && bwrap_available() {
        let mut command = Command::new("bwrap");
        command.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"]);
        if Path::new("/dev/dri").exists() {
            command.args(["--dev-bind", "/dev/dri", "/dev/dri"]);
        }
        if let Some(home) = std::env::var_os("HOME") {
            command.arg("--tmpfs").arg(home);
        }
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
            let runtime = PathBuf::from(runtime);
            command.arg("--tmpfs").arg(&runtime);
            if let Some(display) = std::env::var_os("WAYLAND_DISPLAY") {
                let socket = runtime.join(display);
                command.arg("--bind").arg(&socket).arg(&socket);
            }
        }
        if Path::new("/tmp/.X11-unix").exists() {
            command.args(["--bind", "/tmp/.X11-unix", "/tmp/.X11-unix"]);
        }
        command.arg("--ro-bind").arg(binary).arg(binary);
        command.arg("--ro-bind").arg(working_dir).arg(working_dir);
        command.arg("--chdir").arg(working_dir);
        command.args(["--unshare-net", "--die-with-parent", "--new-session", "--"]);
        command.arg(binary);
        command
    } else {
        let mut command = Command::new(binary);
        command.current_dir(working_dir);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", binary.display()))
}

pub fn bwrap_available() -> bool {
    Command::new("bwrap")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaps_rendered_locations() {
        let wrapped = wrap("let x = 1;\ndiv().bad()", "T").unwrap();
        let main_line = wrapped.first_line + 1;
        let rendered = format!(
            "error[E0599]: no method named `bad`\n  --> src/main.rs:{main_line}:7\n   |\n{main_line:>2} | div().bad()\n   |       ^^^\n"
        );
        let remapped = remap_rendered(&rendered, &wrapped, 2);
        assert!(remapped.contains("--> snippet:2:7"), "{remapped}");
        assert!(remapped.contains(" 2 | div().bad()"), "{remapped}");
    }
}
