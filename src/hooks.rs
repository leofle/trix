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
    /// Conversation id (Claude `session_id`, Codex `thread-id`), for resume.
    #[serde(default)]
    pub session: Option<String>,
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
    let session = s("session_id").or_else(|| s("thread-id")).filter(|id| !id.is_empty());
    HookMsg { agent, source: source.into(), event, tool, detail, session }
}

/// A shell command as a string: Claude sends one; Codex may send an argv,
/// often `["bash", "-lc", "<script>"]`, where the script is what matters.
fn shell_command(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => {
            let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            match parts.as_slice() {
                [_, flag, script] if flag.starts_with('-') && flag.ends_with('c') => Some(script.to_string()),
                [] => None,
                _ => Some(parts.join(" ")),
            }
        }
        _ => None,
    }
}

/// The files a Codex `apply_patch` touches, by name.
fn patch_files(patch: &str) -> Option<String> {
    let files: Vec<String> = patch
        .lines()
        .filter_map(|l| ["*** Add File: ", "*** Update File: ", "*** Delete File: "].iter().find_map(|p| l.strip_prefix(p)))
        .map(|p| Path::new(p.trim()).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| p.trim().into()))
        .collect();
    (!files.is_empty()).then(|| one_line(&files.join(", "), 60))
}

fn tool_detail(tool: Option<&str>, input: &Value) -> Option<String> {
    let g = |k: &str| input.get(k).and_then(Value::as_str);
    let base = |p: &str| Path::new(p).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| p.into());
    let d = match tool.unwrap_or("") {
        "Bash" | "exec_command" | "shell" | "local_shell" => {
            shell_command(input.get("command").or_else(|| input.get("cmd"))).map(|c| one_line(&c, 60))
        }
        "apply_patch" => g("command").or(g("input")).or(g("patch")).and_then(patch_files),
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
    // Codex approvals carry their own one-line explanation.
    d.or_else(|| g("description").map(|s| one_line(s, 60)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn c(v: Value) -> HookMsg {
        condense(7, "claude", &v)
    }

    #[test]
    fn bash_command_is_flattened() {
        let m = c(json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "cargo   test\n  --release"}, "session_id": "s1"}));
        assert_eq!((m.agent, m.source.as_str(), m.event.as_str()), (7, "claude", "PreToolUse"));
        assert_eq!(m.tool.as_deref(), Some("Bash"));
        assert_eq!(m.detail.as_deref(), Some("cargo test --release"));
        assert_eq!(m.session.as_deref(), Some("s1"));
    }

    #[test]
    fn file_tools_show_the_file_name() {
        let m = c(json!({"hook_event_name": "PermissionRequest", "tool_name": "Edit", "tool_input": {"file_path": "/home/x/src/app.rs"}}));
        assert_eq!(m.detail.as_deref(), Some("app.rs"));
        let m = c(json!({"hook_event_name": "PreToolUse", "tool_name": "NotebookEdit", "tool_input": {"notebook_path": "/n/a.ipynb"}}));
        assert_eq!(m.detail.as_deref(), Some("a.ipynb"));
    }

    #[test]
    fn question_detail_is_the_first_question() {
        let m = c(json!({"hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion",
            "tool_input": {"questions": [{"question": "Which db?"}, {"question": "ignored"}]}}));
        assert_eq!(m.detail.as_deref(), Some("Which db?"));
    }

    #[test]
    fn unknown_tool_has_no_detail() {
        let m = c(json!({"hook_event_name": "PreToolUse", "tool_name": "Mystery", "tool_input": {"x": 1}}));
        assert_eq!(m.tool.as_deref(), Some("Mystery"));
        assert_eq!(m.detail, None);
    }

    #[test]
    fn notification_and_prompt() {
        let m = c(json!({"hook_event_name": "Notification", "message": "Claude needs your permission"}));
        assert_eq!(m.detail.as_deref(), Some("Claude needs your permission"));
        let long = "word ".repeat(40);
        let m = c(json!({"hook_event_name": "UserPromptSubmit", "prompt": long}));
        let d = m.detail.unwrap();
        assert_eq!(d.chars().count(), 80);
        assert!(d.ends_with('…'));
    }

    #[test]
    fn codex_turn_complete() {
        let m = condense(2, "codex", &json!({"type": "agent-turn-complete", "thread-id": "t-9", "last-assistant-message": "All tests pass."}));
        assert_eq!(m.event, "agent-turn-complete");
        assert_eq!(m.detail.as_deref(), Some("All tests pass."));
        assert_eq!(m.session.as_deref(), Some("t-9"));
    }

    #[test]
    fn codex_shell_commands() {
        let m = condense(2, "codex", &json!({"hook_event_name": "PreToolUse", "tool_name": "exec_command", "session_id": "thr_1",
            "tool_input": {"cmd": ["bash", "-lc", "cargo   test --release"]}}));
        assert_eq!((m.event.as_str(), m.detail.as_deref(), m.session.as_deref()), ("PreToolUse", Some("cargo test --release"), Some("thr_1")));
        let m = condense(2, "codex", &json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": ["git", "status"]}}));
        assert_eq!(m.detail.as_deref(), Some("git status"));
        let m = condense(2, "codex", &json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "ls -la"}}));
        assert_eq!(m.detail.as_deref(), Some("ls -la"));
    }

    #[test]
    fn codex_patches_show_their_files() {
        let patch = "*** Begin Patch\n*** Update File: src/app.rs\n@@\n-a\n+b\n*** Add File: docs/new.md\n+hi\n*** End Patch";
        let m = condense(2, "codex", &json!({"hook_event_name": "PreToolUse", "tool_name": "apply_patch", "tool_input": {"command": patch}}));
        assert_eq!(m.detail.as_deref(), Some("app.rs, new.md"));
        let m = condense(2, "codex", &json!({"hook_event_name": "PreToolUse", "tool_name": "apply_patch", "tool_input": {"command": "garbage"}}));
        assert_eq!(m.detail, None);
    }

    #[test]
    fn codex_approval_falls_back_to_its_description() {
        let m = condense(2, "codex", &json!({"hook_event_name": "PermissionRequest", "tool_name": "mcp__fs__write",
            "tool_input": {"path": "/x", "description": "Write the config file"}}));
        assert_eq!(m.detail.as_deref(), Some("Write the config file"));
    }

    #[test]
    fn empty_session_id_is_ignored() {
        assert_eq!(c(json!({"hook_event_name": "Stop", "session_id": ""})).session, None);
    }

    #[test]
    fn garbage_payload_still_reports() {
        let m = c(Value::Null);
        assert_eq!(m.event, "unknown");
        assert_eq!((m.tool, m.detail, m.session), (None, None, None));
    }

    #[test]
    fn hook_msg_wire_roundtrip_with_missing_fields() {
        let m: HookMsg = serde_json::from_str(r#"{"agent":3,"source":"custom","event":"Stop"}"#).unwrap();
        assert_eq!((m.agent, m.event.as_str()), (3, "Stop"));
        assert_eq!(m.session, None);
    }

    #[test]
    fn one_line_collapses_and_truncates() {
        assert_eq!(one_line("  a \n\t b  c ", 50), "a b c");
        assert_eq!(one_line("abcdef", 6), "abcdef");
        assert_eq!(one_line("abcdefg", 6), "abcde…");
        assert_eq!(one_line("日本語テキスト", 4), "日本語…");
        assert_eq!(one_line("abc", 0), "…");
    }

    #[test]
    fn claude_settings_hook_every_event() {
        // The exe path is shell-quoted, so one with a quote in it still runs.
        let exe = Path::new("/opt/it's/trix");
        let path = write_claude_settings(exe).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        let hooks = v["hooks"].as_object().unwrap();
        for ev in ["PreToolUse", "PostToolUse", "PermissionRequest", "SessionStart", "UserPromptSubmit", "Notification", "Stop", "SessionEnd"] {
            let cmd = hooks[ev][0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(cmd, r"'/opt/it'\''s/trix' hook claude", "{ev}");
        }
        assert_eq!(hooks["PreToolUse"][0]["matcher"], "*");
    }
}
