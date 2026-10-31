//! A display-independent backend. Herdr only loads mods/easel/main.js.
mod state;

use anyhow::{Context, Result, bail};
use easel_protocol::socket_path;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::{
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

type Shared = Arc<(Mutex<state::State>, Condvar)>;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") if args.len() == 2 => serve(Path::new(&args[1])),
        Some("start") if args.len() == 2 => start(Path::new(&args[1])),
        Some("call") if args.len() == 3 => {
            let value = call(
                Path::new(&args[1]),
                &serde_json::from_str::<Value>(&args[2])?,
            )?;
            println!("{value}");
            Ok(())
        }
        _ => bail!("usage: gpui-artifacts-mod start|serve PROJECT | call PROJECT JSON"),
    }
}

fn mod_socket(project: &Path) -> PathBuf {
    socket_path(
        &project
            .canonicalize()
            .unwrap_or_else(|_| project.to_path_buf()),
    )
    .with_extension("mod.sock")
}

fn call(project: &Path, value: &Value) -> Result<Value> {
    let mut stream =
        UnixStream::connect(mod_socket(project)).context("connecting to Easel mod backend")?;
    stream.set_read_timeout(Some(Duration::from_secs(35)))?;
    writeln!(stream, "{value}")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let value: Value = serde_json::from_str(&line).context("reading Easel mod response")?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        bail!("{error}");
    }
    Ok(value)
}

fn start(project: &Path) -> Result<()> {
    if call(project, &json!({"action":"ping"})).is_ok() {
        return Ok(());
    }
    let mut child = Command::new(std::env::current_exe()?)
        .arg("serve")
        .arg(project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if call(project, &json!({"action":"ping"})).is_ok() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!(
                "Easel backend exited ({status}); the project may already be open in a native viewer"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("Easel backend did not start")
}

fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            bail!("{} is already in use", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

struct Sockets(Vec<PathBuf>);
impl Drop for Sockets {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn serve(project: &Path) -> Result<()> {
    let project = project.canonicalize()?;
    let path = socket_path(&project);
    // Serialize startup before removing stale sockets. Do not touch a live native viewer.
    let lock: File = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("mod.lock"))?;
    lock.try_lock()
        .context("another Easel mod backend is starting")?;
    let protocol = bind(&path)?;
    let mut cleanup = Sockets(vec![path]);
    let ui = bind(&mod_socket(&project))?;
    cleanup.0.push(mod_socket(&project));
    let shared = Arc::new((Mutex::new(state::State::load(&project)?), Condvar::new()));
    loop {
        for (listener, is_ui) in [(&protocol, false), (&ui, true)] {
            if let Ok((stream, _)) = listener.accept() {
                let shared = shared.clone();
                std::thread::spawn(move || {
                    if is_ui {
                        let _ = ui_connection(stream, shared);
                    } else {
                        let _ = easel_protocol::serve_connection(stream, |request| {
                            let (mutex, changed) = &*shared;
                            let mut state = mutex.lock().unwrap();
                            let response = state.request(request);
                            changed.notify_all();
                            response
                        });
                    }
                });
            }
        }
        if shared.0.lock().unwrap().idle() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

fn ui_connection(mut stream: UnixStream, shared: Shared) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(35)))?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let result = (|| -> Result<Value> {
        let request: Value = serde_json::from_str(&line)?;
        let (mutex, changed) = &*shared;
        let mut state = mutex.lock().unwrap();
        state.touch_lease(&request);
        if request["action"] == "watch" {
            let revision = request["revision"].as_u64().unwrap_or(0);
            let (next, _) = changed
                .wait_timeout_while(state, Duration::from_secs(25), |s| s.revision == revision)
                .unwrap_or_else(|e| e.into_inner());
            state = next;
            state.touch();
            return state.snapshot();
        }
        if request["action"] == "approve" {
            let work = state.begin_build(&request)?;
            changed.notify_all();
            drop(state);
            let shared = shared.clone();
            std::thread::spawn(move || state::build(shared, work));
            return Ok(json!({"ok":true}));
        }
        let response = state.action(&request)?;
        changed.notify_all();
        Ok(response)
    })()
    .unwrap_or_else(|err| json!({"error":format!("{err:#}")}));
    writeln!(stream, "{result}")?;
    Ok(())
}
