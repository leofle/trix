//! Agent → trix reporting channel.
//!
//! Every pane gets `TRIX_SOCKET` and `TRIX_AGENT_ID` in its environment. Agents
//! that support lifecycle hooks (Claude Code via `--settings`, Codex via
//! `notify`) call `trix hook <kind>`, which condenses the hook payload into one
//! small JSON line and writes it to the socket. Nothing in the user's own agent
//! config is modified.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::app::AppEvent;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookMsg {
    pub agent: u32,
    pub source: String,
    pub event: String,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

pub fn runtime_dir() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let dir = base.join("trix");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Remove sockets / settings left behind by trix processes that died hard.
pub fn sweep_stale() {
    let Ok(entries) = std::fs::read_dir(runtime_dir()) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let pid = name
            .trim_start_matches("trix-")
            .trim_start_matches("claude-settings-")
            .split('.')
            .next()
            .and_then(|p| p.parse::<u32>().ok());
        if let Some(pid) = pid {
            if !Path::new(&format!("/proc/{pid}")).exists() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join(format!("trix-{}.sock", std::process::id()))
}

/// Start the listener thread. Each connection carries one or more JSON lines.
pub fn serve(path: &Path, tx: Sender<AppEvent>) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::thread::Builder::new().name("trix-hooks".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if let Ok(msg) = serde_json::from_str::<HookMsg>(&line) {
                        let _ = tx.send(AppEvent::Hook(msg));
                    }
                }
            });
        }
    })?;
    Ok(())
}

/// Claude Code settings overlay, passed with `claude --settings <file>`.
pub fn write_claude_settings(exe: &Path) -> anyhow::Result<PathBuf> {
    let quoted = exe.display().to_string().replace('\'', r"'\''");
    let cmd = format!("'{quoted}' hook claude");
    let handler = serde_json::json!([{ "type": "command", "command": cmd, "timeout": 5 }]);
    let mut hooks = serde_json::Map::new();
    for ev in ["PreToolUse", "PostToolUse", "PermissionRequest"] {
        hooks.insert(ev.into(), serde_json::json!([{ "matcher": "*", "hooks": handler }]));
    }
    for ev in ["SessionStart", "UserPromptSubmit", "Notification", "Stop", "SessionEnd"] {
        hooks.insert(ev.into(), serde_json::json!([{ "hooks": handler }]));
    }
    let settings = serde_json::json!({ "hooks": hooks });
    let path = runtime_dir().join(format!("claude-settings-{}.json", std::process::id()));
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)?;
    Ok(path)
}

/// `trix hook <source> [payload]` — runs inside the agent's process tree.
/// Must be fast, silent and never fail loudly: the agent is waiting on it.
pub fn run_client(source: &str, arg_payload: Option<String>) {
    let (Ok(sock), Ok(id)) = (std::env::var("TRIX_SOCKET"), std::env::var("TRIX_AGENT_ID")) else {
        return;
    };
    let Ok(agent) = id.parse::<u32>() else { return };
    // Codex: hand the payload on to the user's own notify, which our
    // `-c notify=` replaced. Fire-and-forget, like Codex itself does.
    if source == "codex" {
        if let (Some(p), Ok(chain)) = (&arg_payload, std::env::var("TRIX_CODEX_NOTIFY")) {
            if let Ok(argv) = serde_json::from_str::<Vec<String>>(&chain) {
                if let Some((prog, rest)) = argv.split_first() {
                    let _ = std::process::Command::new(prog)
                        .args(rest)
                        .arg(p)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                }
            }
        }
    }
    let raw = match arg_payload {
        Some(p) => p,
        None => {
            let mut s = String::new();
            let _ = std::io::stdin().take(256 * 1024).read_to_string(&mut s);
            s
        }
    };
    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let msg = condense(agent, source, &v);
    if let Ok(mut stream) = UnixStream::connect(sock) {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        if let Ok(line) = serde_json::to_string(&msg) {
            let _ = stream.write_all(line.as_bytes());
            let _ = stream.write_all(b"\n");
        }
    }
}

fn condense(agent: u32, source: &str, v: &Value) -> HookMsg {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let event = s("hook_event_name")
        .or_else(|| s("type")) // codex notify: {"type":"agent-turn-complete",...}
        .unwrap_or_else(|| "unknown".into());
    let tool = s("tool_name");
    let input = v.get("tool_input");
    let detail = match event.as_str() {
        "PreToolUse" | "PermissionRequest" => input.and_then(|i| tool_detail(tool.as_deref(), i)),
        "Notification" => s("message"),
        "UserPromptSubmit" => s("prompt").map(|p| one_line(&p, 80)),
        "agent-turn-complete" => s("last-assistant-message").map(|p| one_line(&p, 80)),
        _ => None,
    };
    HookMsg { agent, source: source.into(), event, tool, detail }
}

fn tool_detail(tool: Option<&str>, input: &Value) -> Option<String> {
    let g = |k: &str| input.get(k).and_then(Value::as_str);
    let base = |p: &str| Path::new(p).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| p.into());
    let d = match tool.unwrap_or("") {
        "Bash" => g("command").map(|c| one_line(c, 60)),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            g("file_path").or(g("notebook_path")).map(base)
        }
        "Grep" | "Glob" => g("pattern").map(|p| one_line(p, 40)),
        "WebFetch" => g("url").map(|u| one_line(u, 50)),
        "WebSearch" => g("query").map(|q| one_line(q, 50)),
        "Task" | "Agent" => g("description").map(|d| one_line(d, 50)),
        "AskUserQuestion" => input
            .get("questions")
            .and_then(|q| q.get(0))
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str)
            .map(|q| one_line(q, 60)),
        _ => None,
    };
    d
}

pub fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        flat
    }
}
