//! Agent → trix reporting channel.
//!
//! Every pane gets `TRIX_SOCKET` and `TRIX_AGENT_ID` in its environment. Agents
//! that support lifecycle hooks (Claude Code via `--settings`, Codex via
//! `notify`, Cursor and Copilot via a `--plugin-dir` plugin, OpenCode via a plugin named in
//! `OPENCODE_CONFIG_CONTENT`) call `trix hook <kind>`, which condenses the hook
//! payload into one small JSON line and writes it to the socket. Nothing in the
//! user's own agent config is modified: everything is passed per process.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock};
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
    /// Claude's `transcript_path`: the conversation log the context gauge reads.
    #[serde(default)]
    pub transcript: Option<String>,
    /// When the hook fired, in microseconds since the epoch. Each hook is its
    /// own process and the server reads them on separate threads, so two that
    /// fire close together can arrive in the wrong order: this tells which was
    /// first. Zero when unknown.
    #[serde(default)]
    pub seq: u64,
}

pub fn runtime_dir() -> PathBuf {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(base) => PathBuf::from(base).join("trix"),
        // /tmp is shared by every user: the first one to create a plain
        // /tmp/trix would own it and lock everyone else out.
        None => std::env::temp_dir().join(format!("trix-{}", unsafe { libc::getuid() })),
    };
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
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
            .trim_start_matches("opencode-plugin-")
            .trim_start_matches("cursor-plugin-")
            .trim_start_matches("copilot-plugin-")
            .split('.')
            .next()
            .and_then(|p| p.parse::<u32>().ok());
        if pid.is_some_and(|pid| !crate::platform::alive(pid)) {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = std::fs::remove_dir_all(e.path());
            } else {
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
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            };
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
    let path = runtime_dir().join(format!("claude-settings-{}.json", std::process::id()));
    write_claude_settings_to(&path, exe, None)?;
    Ok(path)
}

/// The overlay at `path`: trix's hooks and, with `theme`, Claude's theme,
/// which wins over the user's own for that Claude (it reads it as it starts).
/// Written through a rename, since a Claude may be starting and reading it.
pub fn write_claude_settings_to(path: &Path, exe: &Path, theme: Option<&str>) -> anyhow::Result<()> {
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
    let mut settings = serde_json::json!({ "hooks": hooks });
    if let Some(t) = theme {
        settings["theme"] = t.into();
    }
    crate::state::write_atomic(path, &serde_json::to_vec_pretty(&settings)?)?;
    Ok(())
}

/// What trix hands each agent so it reports back, written once per server
/// into the runtime dir and removed when the server stops.
#[derive(Debug, Clone)]
pub struct Integrations {
    /// Claude Code `--settings` overlay.
    pub claude_settings: Option<PathBuf>,
    /// OpenCode plugin, loaded through `OPENCODE_CONFIG_CONTENT`: a file for
    /// OpenCode 1, a folder for OpenCode 2.
    pub opencode_plugin: Option<PathBuf>,
    pub opencode_plugin_dir: Option<PathBuf>,
    /// Cursor plugin directory, loaded with `cursor-agent --plugin-dir`.
    pub cursor_plugin: Option<PathBuf>,
    /// Copilot CLI plugin directory, loaded with `copilot --plugin-dir`.
    pub copilot_plugin: Option<PathBuf>,
    /// OpenCode takes `--standalone` (2.x). Without it, OpenCode 2 joins a
    /// background server that the first OpenCode started, with that one's
    /// environment: the plugin, the pane and the trix server it reports to.
    /// Read with `opencode_standalone()`.
    pub standalone: Arc<OnceLock<bool>>,
    /// trix paints the panes with a theme of its own: Grok and OpenCode are
    /// started with their themes that take the terminal's colors.
    pub themed: bool,
}

/// Nothing written, and OpenCode taken for 1.x (tests).
impl Default for Integrations {
    fn default() -> Self {
        Integrations::with_standalone(false)
    }
}

impl Integrations {
    /// Write them all. One that can't be written is left out, and that agent
    /// falls back to reading its screen.
    pub fn write(exe: &Path) -> Integrations {
        // Asking OpenCode its version runs it, which takes a moment (up to
        // 3 s): in the background, so the server answers its first `trix`
        // without waiting for it. Only an OpenCode starting that soon waits.
        let standalone = Arc::new(OnceLock::new());
        let ask = standalone.clone();
        let _ = std::thread::Builder::new().name("trix-opencode".into()).spawn(move || {
            ask.get_or_init(opencode_has_standalone);
        });
        Integrations {
            claude_settings: write_claude_settings(exe).ok(),
            opencode_plugin: write_opencode_plugin(exe).ok(),
            opencode_plugin_dir: write_opencode_plugin_dir(exe).ok(),
            cursor_plugin: write_cursor_plugin(exe).ok(),
            copilot_plugin: write_copilot_plugin(exe).ok(),
            standalone,
            themed: false,
        }
    }

    /// Nothing written, and OpenCode's version already known.
    pub fn with_standalone(standalone: bool) -> Integrations {
        Integrations {
            claude_settings: None,
            opencode_plugin: None,
            opencode_plugin_dir: None,
            cursor_plugin: None,
            copilot_plugin: None,
            standalone: Arc::new(OnceLock::from(standalone)),
            themed: false,
        }
    }

    /// Does OpenCode take `--standalone` (2.x)? Waits for the answer if it
    /// is still being asked.
    pub fn opencode_standalone(&self) -> bool {
        *self.standalone.get_or_init(opencode_has_standalone)
    }

    /// The plugin for the OpenCode installed: OpenCode 2 is the one with
    /// `--standalone`.
    pub fn opencode_plugin_for_version(&self) -> Option<&Path> {
        if self.opencode_standalone() { self.opencode_plugin_dir.as_deref() } else { self.opencode_plugin.as_deref() }
    }

    pub fn remove(&self) {
        for f in [&self.claude_settings, &self.opencode_plugin].into_iter().flatten() {
            let _ = std::fs::remove_file(f);
        }
        for d in [&self.cursor_plugin, &self.copilot_plugin, &self.opencode_plugin_dir].into_iter().flatten() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// Does the OpenCode on `PATH` know `--standalone`? One that isn't installed
/// yet will be the latest, which does. Its help, read once per server, says;
/// one that doesn't answer within 3 s gets the flag left out (OpenCode 1.x
/// refuses options it doesn't know).
fn opencode_has_standalone() -> bool {
    use std::process::{Command, Stdio};
    if crate::install::missing("opencode") {
        return true;
    }
    let Ok(mut child) = Command::new("opencode").arg("--help").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    let mut out = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return reader.join().is_ok_and(|s| s.contains("--standalone"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

/// `'<exe>' hook <source>`, for hook runners that take a shell string.
fn hook_command(exe: &Path, source: &str) -> String {
    let quoted = exe.display().to_string().replace('\'', r"'\''");
    format!("'{quoted}' hook {source}")
}

/// OpenCode plugin, for OpenCode 1 or 2. OpenCode runs its server, plugins
/// included, with the environment of the process that started it (each
/// pane's own, with `--standalone`), so the plugin only reports when it runs
/// inside a trix pane. It hands each event to `trix hook opencode` as an argv
/// (no shell), and keeps track of subagent sessions so a subagent finishing
/// isn't taken for the pane's turn ending.
pub fn opencode_plugin_source(exe: &Path, v2: bool) -> String {
    let exe = serde_json::to_string(&exe.display().to_string()).unwrap_or_else(|_| "\"trix\"".into());
    let tail = if v2 { OPENCODE_PLUGIN_V2 } else { OPENCODE_PLUGIN_V1 };
    format!("{OPENCODE_PLUGIN}{tail}").replace("__TRIX_EXE__", &exe)
}

/// What both versions share: reporting, cutting text short, tool arguments
/// and subagent sessions.
const OPENCODE_PLUGIN: &str = r#"// Written by trix for the OpenCode agents it runs; removed when trix stops.
import { spawn } from "node:child_process";

const TRIX = __TRIX_EXE__;
const KEYS = ["command", "filePath", "path", "pattern", "url", "query", "description"];
const parents = new Map();
const aborted = new Set();
let seq = 0;

const root = (id) => {
  for (let n = 0; id && parents.has(id) && n < 64; n++) id = parents.get(id);
  return id;
};

function report(payload) {
  if (!process.env.TRIX_SOCKET || !process.env.TRIX_AGENT_ID) return;
  seq = Math.max(seq + 1, Date.now() * 1000);
  payload.trix_seq = seq;
  try {
    const child = spawn(TRIX, ["hook", "opencode", JSON.stringify(payload)], { stdio: "ignore" });
    child.on("error", () => {});
    child.unref();
  } catch {}
}

// At most 400 UTF-16 units, never half an emoji: a lone surrogate is
// invalid in JSON, and the whole report would be dropped.
function cut(s) {
  let t = s.slice(0, 400);
  if (/[\uD800-\uDBFF]$/.test(t)) t = t.slice(0, -1);
  return typeof t.toWellFormed === "function" ? t.toWellFormed() : t;
}

function brief(args) {
  const out = {};
  for (const k of KEYS) {
    const v = args?.[k];
    if (typeof v === "string") out[k] = cut(v);
  }
  return out;
}
"#;

/// OpenCode 1: a named export returning its hooks, with every event through
/// `event`.
const OPENCODE_PLUGIN_V1: &str = r#"
export const TrixPlugin = async () => {
  report({ type: "trix.loaded" });
  return {
    "chat.message": async (input, output) => {
      if (parents.has(input?.sessionID)) return;
      const text = (output?.parts ?? [])
        .filter((p) => p?.type === "text" && !p.synthetic && typeof p.text === "string")
        .map((p) => p.text)
        .join(" ");
      report({ type: "chat.message", sessionID: input?.sessionID, prompt: cut(text) });
    },
    "tool.execute.before": async (input, output) => {
      report({ type: "tool.execute.before", sessionID: root(input?.sessionID), tool: input?.tool, args: brief(output?.args) });
    },
    "tool.execute.after": async (input) => {
      report({ type: "tool.execute.after", sessionID: root(input?.sessionID), tool: input?.tool });
    },
    event: async ({ event }) => {
      const type = event?.type;
      const p = event?.properties ?? {};
      if (p.info?.id && p.info.parentID) parents.set(p.info.id, p.info.parentID);
      const child = parents.has(p.sessionID);
      switch (type) {
        case "session.status": {
          if (child) return;
          let status = p.status?.type;
          if (status === "idle" && aborted.delete(p.sessionID)) status = "aborted";
          report({ type, sessionID: p.sessionID, status });
          return;
        }
        case "session.error":
          if (!child && p.error?.name === "MessageAbortedError") aborted.add(p.sessionID);
          return;
        case "permission.asked":
          report({ type, sessionID: root(p.sessionID), permission: p.permission, patterns: p.patterns });
          return;
        case "question.asked":
          report({ type, sessionID: root(p.sessionID), question: p.questions?.[0]?.question });
          return;
        case "permission.replied":
        case "question.replied":
        case "question.rejected":
          report({ type, sessionID: root(p.sessionID) });
          return;
      }
    },
  };
};
"#;

/// OpenCode 2: a default `{ id, setup }` export. Events come from
/// `ctx.event.subscribe()` with their fields under `data`, tools from
/// `ctx.tool.hook`. Each is reported as the OpenCode 1 plugin would, so
/// `condense_opencode` reads both: a turn is `session.execution.*`, a prompt
/// is a user item in the session's inbox, and a question is a form.
const OPENCODE_PLUGIN_V2: &str = r#"
function on(event) {
  const type = event?.type;
  const d = event?.data ?? {};
  if (type === "session.created" && d.parentID) {
    parents.set(d.sessionID, d.parentID);
    return;
  }
  const child = parents.has(d.sessionID);
  const status = (s) => {
    if (!child) report({ type: "session.status", sessionID: d.sessionID, status: s });
  };
  switch (type) {
    case "session.inbox.enqueued": {
      const text = d.item?.payload?.text;
      if (!child && d.item?.type === "user" && typeof text === "string") report({ type: "chat.message", sessionID: d.sessionID, prompt: cut(text) });
      return;
    }
    case "session.execution.started":
      return status("busy");
    case "session.status":
      if (d.status?.type === "retry") status("retry");
      return;
    case "session.execution.succeeded":
    case "session.execution.failed":
      return status("idle");
    case "session.execution.interrupted":
      return status(d.reason === "user" ? "aborted" : "idle");
    case "permission.asked":
      report({ type, sessionID: root(d.sessionID), permission: d.action, patterns: d.resources });
      return;
    case "permission.replied":
      report({ type, sessionID: root(d.sessionID) });
      return;
    case "form.created": {
      const f = d.form ?? {};
      const q = f.fields?.[0]?.title ?? f.title;
      report({ type: "question.asked", sessionID: root(f.sessionID), question: typeof q === "string" ? cut(q) : undefined });
      return;
    }
    case "form.replied":
      report({ type: "question.replied", sessionID: root(d.sessionID) });
      return;
    case "form.cancelled":
      report({ type: "question.rejected", sessionID: root(d.sessionID) });
      return;
  }
}

export default {
  id: "trix",
  setup: async (ctx) => {
    report({ type: "trix.loaded" });
    const undo = [];
    const hook = (name, fn) => {
      try {
        const h = ctx.tool?.hook?.(name, fn);
        if (typeof h === "function") undo.push(h);
        else if (typeof h?.dispose === "function") undo.push(() => h.dispose());
      } catch {}
    };
    hook("execute.before", async (input) => {
      report({ type: "tool.execute.before", sessionID: root(input?.sessionID), tool: input?.tool, args: brief(input?.input) });
    });
    hook("execute.after", async (input) => {
      report({ type: "tool.execute.after", sessionID: root(input?.sessionID), tool: input?.tool });
    });
    let stopped = false;
    (async () => {
      try {
        for await (const event of ctx.event.subscribe()) {
          if (stopped) break;
          on(event);
        }
      } catch {}
    })();
    return async () => {
      stopped = true;
      for (const u of undo) {
        try {
          await u();
        } catch {}
      }
    };
  },
};
"#;

fn write_opencode_plugin(exe: &Path) -> anyhow::Result<PathBuf> {
    let path = runtime_dir().join(format!("opencode-plugin-{}.js", std::process::id()));
    std::fs::write(&path, opencode_plugin_source(exe, false))?;
    Ok(path)
}

/// OpenCode 2 loads a plugin from a folder only: its `index.js`.
fn write_opencode_plugin_dir(exe: &Path) -> anyhow::Result<PathBuf> {
    let dir = runtime_dir().join(format!("opencode-plugin-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("index.js"), opencode_plugin_source(exe, true))?;
    Ok(dir)
}

/// The `OPENCODE_CONFIG_CONTENT` that loads trix's plugin. OpenCode merges it
/// over every other config and appends its `plugin` list to theirs. If the
/// user already sets the variable, the plugin joins their list; if theirs
/// isn't a plain JSON object with a list there, it is left alone and the
/// agent goes without the plugin.
pub fn opencode_config(plugin: &Path, existing: Option<&str>) -> Option<String> {
    let url = format!("file://{}", file_url_path(plugin));
    let mut config = match existing.filter(|e| !e.trim().is_empty()) {
        None => serde_json::json!({}),
        Some(e) => serde_json::from_str::<Value>(e).ok().filter(Value::is_object)?,
    };
    let list = config.as_object_mut()?.entry("plugin").or_insert_with(|| Value::Array(Vec::new()));
    list.as_array_mut()?.push(Value::String(url));
    serde_json::to_string(&config).ok()
}

/// A path as a file URL path: every byte but `/` and RFC 3986's unreserved
/// ones percent-encoded.
fn file_url_path(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Cursor events trix listens to. None of them is one of Cursor's
/// permission hooks (`preToolUse`, `beforeShellExecution`, …), whose answer
/// decides whether a tool may run: trix only watches, it never approves.
pub const CURSOR_HOOK_EVENTS: [&str; 6] = ["sessionStart", "beforeSubmitPrompt", "postToolUse", "postToolUseFailure", "stop", "sessionEnd"];

/// A Cursor plugin holding only hooks, for `cursor-agent --plugin-dir`.
fn write_cursor_plugin(exe: &Path) -> anyhow::Result<PathBuf> {
    let dir = runtime_dir().join(format!("cursor-plugin-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".cursor-plugin"))?;
    std::fs::create_dir_all(dir.join("hooks"))?;
    let manifest = serde_json::json!({
        "name": "trix",
        "description": "Reports this agent's state to the trix pane it runs in",
        "version": "1.0.0",
        "author": { "name": "trix" },
    });
    std::fs::write(dir.join(".cursor-plugin/plugin.json"), serde_json::to_vec_pretty(&manifest)?)?;
    std::fs::write(dir.join("hooks/hooks.json"), serde_json::to_vec_pretty(&cursor_hooks(exe))?)?;
    Ok(dir)
}

pub fn cursor_hooks(exe: &Path) -> Value {
    let handler = serde_json::json!([{ "command": hook_command(exe, "cursor"), "timeout": 5 }]);
    let hooks: serde_json::Map<String, Value> = CURSOR_HOOK_EVENTS.iter().map(|e| (e.to_string(), handler.clone())).collect();
    serde_json::json!({ "version": 1, "hooks": hooks })
}

/// Copilot CLI events trix listens to, by their Claude-style names: with
/// those, Copilot sends snake_case fields and Claude's tool names (`Bash`,
/// `Read`, `AskUserQuestion` for `ask_user`), as Claude's hooks do. Not
/// `PermissionRequest`: it fires before Copilot's own rules decide, so for
/// tools that never ask too. A prompt that waits on you raises
/// `Notification` (`permission_prompt`, `elicitation_dialog`).
pub const COPILOT_HOOK_EVENTS: [&str; 8] =
    ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure", "Notification", "Stop", "SessionEnd"];

/// A Copilot CLI plugin holding only hooks, for `copilot --plugin-dir`.
fn write_copilot_plugin(exe: &Path) -> anyhow::Result<PathBuf> {
    let dir = runtime_dir().join(format!("copilot-plugin-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let manifest = serde_json::json!({
        "name": "trix",
        "description": "Reports this agent's state to the trix pane it runs in",
        "version": "1.0.0",
    });
    std::fs::write(dir.join("plugin.json"), serde_json::to_vec_pretty(&manifest)?)?;
    std::fs::write(dir.join("hooks.json"), serde_json::to_vec_pretty(&copilot_hooks(exe))?)?;
    Ok(dir)
}

/// Copilot denies a tool when its `PreToolUse` command fails, so the hook
/// always succeeds, and says nothing: no output is no decision.
pub fn copilot_hooks(exe: &Path) -> Value {
    let command = format!("{} || true", hook_command(exe, "copilot"));
    let hooks: serde_json::Map<String, Value> = COPILOT_HOOK_EVENTS
        .iter()
        .map(|e| {
            let mut h = serde_json::json!({ "type": "command", "bash": command, "timeoutSec": 5 });
            if e.contains("ToolUse") {
                h["matcher"] = "*".into();
            }
            (e.to_string(), Value::Array(vec![h]))
        })
        .collect();
    serde_json::json!({ "version": 1, "hooks": hooks })
}

/// `trix hook <source> [payload]` — runs inside the agent's process tree.
/// Must be fast, silent and never fail loudly: the agent is waiting on it.
pub fn run_client(source: &str, arg_payload: Option<String>) {
    // Cursor reads every hook's stdout as its answer. trix's hooks only
    // watch: answer with nothing to say (and let a prompt through), so no
    // reply of ours can read as a decision.
    if source == "cursor" {
        let mut s = String::new();
        let _ = std::io::stdin().take(64 << 20).read_to_string(&mut s);
        let v: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
        let submit = v.get("hook_event_name").and_then(Value::as_str) == Some("beforeSubmitPrompt");
        println!("{}", if submit { r#"{"continue":true}"# } else { "{}" });
        return report(source, &v, None);
    }
    let (Ok(_), Ok(_)) = (std::env::var("TRIX_SOCKET"), std::env::var("TRIX_AGENT_ID")) else {
        return;
    };
    // Codex: hand the payload on to the user's own notify, which our
    // `-c notify=` replaced. Fire-and-forget, like Codex itself does.
    if source == "codex"
        && let (Some(p), Ok(chain)) = (&arg_payload, std::env::var("TRIX_CODEX_NOTIFY"))
        && let Ok(argv) = serde_json::from_str::<Vec<String>>(&chain)
        && let Some((prog, rest)) = argv.split_first()
    {
        let _ = std::process::Command::new(prog)
            .args(rest)
            .arg(p)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    let seq = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64);
    let raw = match arg_payload {
        Some(p) => p,
        None => {
            let mut s = String::new();
            // Approvals carry the full content (a Write of a big file): cut it
            // short and the JSON, event name included, no longer parses.
            let _ = std::io::stdin().take(64 << 20).read_to_string(&mut s);
            s
        }
    };
    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    report(source, &v, Some(seq));
}

/// Condense one payload and send it to the trix pane this runs in.
fn report(source: &str, v: &Value, seq: Option<u64>) {
    let (Ok(sock), Ok(id)) = (std::env::var("TRIX_SOCKET"), std::env::var("TRIX_AGENT_ID")) else {
        return;
    };
    let Ok(agent) = id.parse::<u32>() else { return };
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64);
    let mut msg = condense(agent, source, v);
    // OpenCode's plugin stamps each event as it happens: the processes it
    // starts to report them may get here in another order.
    msg.seq = v.get("trix_seq").and_then(Value::as_u64).or(seq).unwrap_or_else(now);
    send(&sock, &msg);
}

/// Tell the trix pane this runs in about `event`, with nothing else to say
/// (trix's own installer: "Installed").
pub fn tell(source: &str, event: &str) {
    let (Ok(sock), Ok(id)) = (std::env::var("TRIX_SOCKET"), std::env::var("TRIX_AGENT_ID")) else {
        return;
    };
    let Ok(agent) = id.parse::<u32>() else { return };
    let seq = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64);
    let msg = HookMsg { agent, source: source.into(), event: event.into(), tool: None, detail: None, session: None, transcript: None, seq };
    send(&sock, &msg);
}

fn send(sock: &str, msg: &HookMsg) {
    if let Ok(mut stream) = UnixStream::connect(sock) {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        if let Ok(line) = serde_json::to_string(msg) {
            let _ = stream.write_all(line.as_bytes());
            let _ = stream.write_all(b"\n");
        }
    }
}

fn condense(agent: u32, source: &str, v: &Value) -> HookMsg {
    match source {
        "opencode" => return condense_opencode(agent, v),
        "cursor" => return condense_cursor(agent, v),
        "grok" => return condense_grok(agent, v),
        "copilot" => return condense_copilot(agent, v),
        _ => {}
    }
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
    let transcript = s("transcript_path").filter(|p| !p.is_empty());
    HookMsg { agent, source: source.into(), event, tool, detail, session, transcript, seq: 0 }
}

fn msg(agent: u32, source: &str, event: &str) -> HookMsg {
    HookMsg { agent, source: source.into(), event: event.into(), tool: None, detail: None, session: None, transcript: None, seq: 0 }
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// An event from trix's OpenCode plugin (see `OPENCODE_PLUGIN`): OpenCode's
/// own event and hook names, put in Claude's terms. `sessionID` is the
/// pane's conversation, a subagent's already traced back to it.
fn condense_opencode(agent: u32, v: &Value) -> HookMsg {
    let kind = text(v, "type").unwrap_or_default();
    let event = match kind.as_str() {
        "trix.loaded" => "SessionStart",
        "chat.message" => "UserPromptSubmit",
        "tool.execute.before" => "PreToolUse",
        "tool.execute.after" => "PostToolUse",
        "permission.asked" => "PermissionRequest",
        "question.asked" => "Question",
        "permission.replied" | "question.replied" | "question.rejected" => "Answered",
        "session.status" => match v.get("status").and_then(Value::as_str) {
            Some("idle") => "Stop",
            Some("aborted") => "Interrupt",
            Some("busy" | "retry") => "Busy",
            _ => "unknown",
        },
        _ => "unknown",
    };
    let mut m = msg(agent, "opencode", event);
    m.session = text(v, "sessionID");
    match event {
        "UserPromptSubmit" => m.detail = text(v, "prompt").map(|p| one_line(&p, 80)),
        "PreToolUse" | "PostToolUse" => {
            m.tool = text(v, "tool");
            if event == "PreToolUse" {
                m.detail = v.get("args").and_then(|a| tool_detail(m.tool.as_deref(), a));
            }
        }
        "PermissionRequest" => {
            m.tool = text(v, "permission");
            let patterns: Vec<&str> = v.get("patterns").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            m.detail = (!patterns.is_empty()).then(|| one_line(&patterns.join(", "), 60));
        }
        "Question" => m.detail = text(v, "question").map(|q| one_line(&q, 60)),
        _ => {}
    }
    m
}

/// A Cursor hook payload. Cursor names its events in camelCase and its
/// tools `Shell`, `Read`, `Write`…; `stop` says how the turn ended.
fn condense_cursor(agent: u32, v: &Value) -> HookMsg {
    let name = text(v, "hook_event_name").unwrap_or_default();
    let event = match name.as_str() {
        "sessionStart" => "SessionStart",
        "beforeSubmitPrompt" => "UserPromptSubmit",
        "postToolUse" | "postToolUseFailure" => "ToolDone",
        "stop" if v.get("status").and_then(Value::as_str) == Some("aborted") => "Interrupt",
        "stop" => "Stop",
        "sessionEnd" => "SessionEnd",
        _ => "unknown",
    };
    let mut m = msg(agent, "cursor", event);
    // A subagent's tool calls carry its own conversation id: only the
    // session's start, the prompt and the turn's end are surely the pane's
    // conversation. cursor-agent 2026.11.11 runs only `sessionStart` and the
    // tool hooks from a plugin, and a subagent doesn't run `sessionStart`.
    if matches!(name.as_str(), "sessionStart" | "beforeSubmitPrompt" | "stop") {
        m.session = text(v, "conversation_id").or_else(|| text(v, "session_id"));
    }
    match event {
        "UserPromptSubmit" => m.detail = text(v, "prompt").map(|p| one_line(&p, 80)),
        "ToolDone" => {
            m.tool = text(v, "tool_name");
            m.detail = v.get("tool_input").and_then(|i| tool_detail(m.tool.as_deref(), i));
        }
        _ => {}
    }
    m
}

/// A Grok Build hook payload, from hooks the user installed themselves (trix
/// can't pass Grok hooks per process). Grok sends Claude's event names in
/// `hook_event_name` and its own fields in camelCase.
fn condense_grok(agent: u32, v: &Value) -> HookMsg {
    let name = text(v, "hook_event_name").unwrap_or_default();
    let event = match name.as_str() {
        "StopCancelled" => "Interrupt",
        "StopFailure" => "Stop",
        "PostToolUseFailure" => "PostToolUse",
        // Grok has no `PermissionRequest` hook: a waiting permission prompt
        // raises `Notification` with type `permission_prompt`.
        "Notification" if text(v, "notificationType").or_else(|| text(v, "notification_type")).as_deref() == Some("permission_prompt") => "PermissionRequest",
        "SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "Notification" | "Stop" | "SessionEnd" => name.as_str(),
        _ => "unknown",
    };
    let mut m = msg(agent, "grok", event);
    m.session = text(v, "sessionId").or_else(|| text(v, "session_id")).or_else(|| std::env::var("GROK_SESSION_ID").ok().filter(|s| !s.is_empty()));
    m.tool = text(v, "toolName").or_else(|| text(v, "tool_name"));
    let input = v.get("toolInput").or_else(|| v.get("tool_input"));
    m.detail = match event {
        "PreToolUse" => input.and_then(|i| tool_detail(m.tool.as_deref(), i)),
        "UserPromptSubmit" => text(v, "prompt").map(|p| one_line(&p, 80)),
        "Notification" => text(v, "message").or_else(|| text(v, "notificationType")).or_else(|| text(v, "notification_type")),
        // Grok's own words for what waits, such as "Grok needs your permission".
        "PermissionRequest" => text(v, "message").map(|m| one_line(&m, 80)),
        _ => None,
    };
    if event == "PermissionRequest" && m.tool.is_none() {
        m.tool = Some("permission".into());
    }
    m
}

/// A Copilot CLI hook payload, from the Claude-style events trix's plugin
/// asks for (`COPILOT_HOOK_EVENTS`): Claude's event, field and tool names.
/// A subagent's hooks carry `agent_id`: its tools show, its end doesn't end
/// the pane's turn.
fn condense_copilot(agent: u32, v: &Value) -> HookMsg {
    let name = text(v, "hook_event_name").unwrap_or_default();
    let event = match name.as_str() {
        "PostToolUseFailure" => "PostToolUse",
        "Stop" if text(v, "agent_id").is_some() => "unknown",
        "Notification" => match text(v, "notification_type").as_deref() {
            Some("permission_prompt") => "PermissionRequest",
            Some("elicitation_dialog") => "Question",
            // Background shells and agents finishing: not this pane's turn.
            _ => "unknown",
        },
        "SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "Stop" | "SessionEnd" => name.as_str(),
        _ => "unknown",
    };
    let mut m = msg(agent, "copilot", event);
    m.session = text(v, "session_id");
    m.tool = text(v, "tool_name");
    m.detail = match event {
        "PreToolUse" => v.get("tool_input").and_then(|i| tool_detail(m.tool.as_deref(), i)),
        "UserPromptSubmit" => text(v, "prompt").map(|p| one_line(&p, 80)),
        "PermissionRequest" | "Question" => text(v, "message").map(|m| one_line(&m, 80)),
        _ => None,
    };
    m
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
        "Bash" | "exec_command" | "shell" | "local_shell" | "Shell" | "bash" | "run_terminal_command" => {
            shell_command(input.get("command").or_else(|| input.get("cmd"))).map(|c| one_line(&c, 60))
        }
        "apply_patch" => g("command").or(g("input")).or(g("patch")).and_then(patch_files),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            g("file_path").or(g("notebook_path")).or(g("path")).map(base)
        }
        // OpenCode's file tools, Grok's and Cursor's own.
        "read" | "write" | "edit" | "patch" | "read_file" | "write_file" | "search_replace" | "StrReplace" | "Delete" => {
            g("filePath").or(g("file_path")).or(g("path")).or(g("target_file")).map(base)
        }
        "Grep" | "Glob" | "grep" | "glob" => g("pattern").map(|p| one_line(p, 40)),
        "WebFetch" | "webfetch" | "web_fetch" => g("url").map(|u| one_line(u, 50)),
        "WebSearch" | "websearch" | "web_search" => g("query").map(|q| one_line(q, 50)),
        "Task" | "Agent" | "task" => g("description").map(|d| one_line(d, 50)),
        // Claude's take a list; Copilot's `ask_user` one question.
        "AskUserQuestion" => input
            .get("questions")
            .and_then(|q| q.get(0))
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str)
            .or(g("question"))
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
        assert!(v.get("theme").is_none(), "Claude keeps the user's theme");
    }

    #[test]
    fn claude_settings_carry_a_theme_and_keep_the_hooks() {
        let path = std::env::temp_dir().join(format!("trix-claude-theme-test-{}.json", std::process::id()));
        let read = || serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap();
        write_claude_settings_to(&path, Path::new("/bin/trix"), Some("light-ansi")).unwrap();
        let v = read();
        assert_eq!(v["theme"], "light-ansi");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "'/bin/trix' hook claude");
        write_claude_settings_to(&path, Path::new("/bin/trix"), None).unwrap();
        assert!(read().get("theme").is_none(), "back to the user's own");
        let _ = std::fs::remove_file(&path);
    }

    fn o(v: Value) -> HookMsg {
        condense(7, "opencode", &v)
    }

    #[test]
    fn condenses_opencode_plugin_events() {
        let m = o(json!({"type":"chat.message","sessionID":"ses_1","prompt":"fix\nthe   login","trix_seq":5}));
        assert_eq!((m.source.as_str(), m.event.as_str(), m.detail.as_deref(), m.session.as_deref()), ("opencode", "UserPromptSubmit", Some("fix the login"), Some("ses_1")));
        let m = o(json!({"type":"tool.execute.before","sessionID":"ses_1","tool":"bash","args":{"command":"cargo test"}}));
        assert_eq!((m.event.as_str(), m.tool.as_deref(), m.detail.as_deref()), ("PreToolUse", Some("bash"), Some("cargo test")));
        let m = o(json!({"type":"tool.execute.before","tool":"edit","args":{"filePath":"/src/app/main.rs"}}));
        assert_eq!(m.detail.as_deref(), Some("main.rs"));
        assert_eq!(o(json!({"type":"tool.execute.after","tool":"bash"})).event, "PostToolUse");
        let m = o(json!({"type":"permission.asked","permission":"bash","patterns":["rm *","git push"]}));
        assert_eq!((m.event.as_str(), m.tool.as_deref(), m.detail.as_deref()), ("PermissionRequest", Some("bash"), Some("rm *, git push")));
        let m = o(json!({"type":"question.asked","question":"Which database?"}));
        assert_eq!((m.event.as_str(), m.detail.as_deref()), ("Question", Some("Which database?")));
        for t in ["permission.replied", "question.replied", "question.rejected"] {
            assert_eq!(o(json!({ "type": t })).event, "Answered", "{t}");
        }
        let status = |s: &str| o(json!({"type":"session.status","status":s})).event;
        assert_eq!((status("idle"), status("busy"), status("retry"), status("aborted")), ("Stop".into(), "Busy".into(), "Busy".into(), "Interrupt".into()));
        assert_eq!(o(json!({"type":"trix.loaded"})).event, "SessionStart");
        assert_eq!(o(json!({"type":"session.diff"})).event, "unknown");
    }

    #[test]
    fn condenses_cursor_hooks() {
        let c = |v: Value| condense(7, "cursor", &v);
        let m = c(json!({"hook_event_name":"beforeSubmitPrompt","conversation_id":"conv-1","session_id":"conv-1","prompt":"add tests"}));
        assert_eq!((m.event.as_str(), m.detail.as_deref(), m.session.as_deref()), ("UserPromptSubmit", Some("add tests"), Some("conv-1")));
        let m = c(json!({"hook_event_name":"postToolUse","conversation_id":"sub-2","tool_name":"Shell","tool_input":{"command":"ls -la"}}));
        assert_eq!((m.event.as_str(), m.tool.as_deref(), m.detail.as_deref(), m.session), ("ToolDone", Some("Shell"), Some("ls -la"), None), "a tool's conversation may be a subagent's");
        let m = c(json!({"hook_event_name":"postToolUseFailure","tool_name":"Write","tool_input":{"path":"/a/b.rs"}}));
        assert_eq!((m.event.as_str(), m.detail.as_deref()), ("ToolDone", Some("b.rs")));
        let m = c(json!({"hook_event_name":"stop","conversation_id":"conv-1","status":"completed"}));
        assert_eq!((m.event.as_str(), m.session.as_deref()), ("Stop", Some("conv-1")));
        assert_eq!(c(json!({"hook_event_name":"stop","status":"aborted"})).event, "Interrupt");
        assert_eq!(c(json!({"hook_event_name":"stop","status":"error"})).event, "Stop");
        // As cursor-agent 2026.11.11 sends it: the pane's conversation, which `--resume` takes.
        let m = c(json!({"hook_event_name":"sessionStart","conversation_id":"conv-1","session_id":"conv-1","is_background_agent":false}));
        assert_eq!((m.event.as_str(), m.session.as_deref()), ("SessionStart", Some("conv-1")));
        assert_eq!(c(json!({"hook_event_name":"sessionEnd"})).event, "SessionEnd");
        assert_eq!(c(json!({"hook_event_name":"beforeShellExecution"})).event, "unknown");
    }

    #[test]
    fn condenses_grok_hooks() {
        let g = |v: Value| condense(7, "grok", &v);
        let m = g(json!({"hook_event_name":"PreToolUse","hookEventName":"pre_tool_use","sessionId":"s-1","toolName":"run_terminal_command","toolInput":{"command":"cargo build"}}));
        assert_eq!((m.event.as_str(), m.tool.as_deref(), m.detail.as_deref(), m.session.as_deref()), ("PreToolUse", Some("run_terminal_command"), Some("cargo build"), Some("s-1")));
        let m = g(json!({"hook_event_name":"PreToolUse","toolName":"search_replace","toolInput":{"path":"src/x.rs"}}));
        assert_eq!(m.detail.as_deref(), Some("x.rs"));
        assert_eq!(g(json!({"hook_event_name":"StopCancelled"})).event, "Interrupt");
        assert_eq!(g(json!({"hook_event_name":"StopFailure"})).event, "Stop");
        assert_eq!(g(json!({"hook_event_name":"PostToolUseFailure"})).event, "PostToolUse");
        let m = g(json!({"hook_event_name":"Notification","notificationType":"task_complete"}));
        assert_eq!((m.event.as_str(), m.detail.as_deref()), ("Notification", Some("task_complete")));
        assert_eq!(g(json!({"hook_event_name":"PermissionDenied"})).event, "unknown");
        let m = g(json!({"hook_event_name":"Notification","notificationType":"permission_prompt","notification_type":"permission_prompt","message":"Grok needs your permission"}));
        assert_eq!((m.event.as_str(), m.tool.as_deref(), m.detail), ("PermissionRequest", Some("permission"), Some("Grok needs your permission".into())));
        assert_eq!(g(json!({"hook_event_name":"Notification","notification_type":"permission_prompt"})).event, "PermissionRequest");
        assert_eq!(g(json!({"hook_event_name":"Notification","notificationType":"idle_prompt"})).event, "Notification");
    }

    #[test]
    fn new_tool_names_get_details() {
        let d = |t: &str, v: Value| tool_detail(Some(t), &v);
        assert_eq!(d("run_terminal_command", json!({"command":"ls"})).as_deref(), Some("ls"));
        assert_eq!(d("read_file", json!({"path":"/a/b.txt"})).as_deref(), Some("b.txt"));
        assert_eq!(d("StrReplace", json!({"path":"c.rs"})).as_deref(), Some("c.rs"));
        assert_eq!(d("webfetch", json!({"url":"https://x.ai"})).as_deref(), Some("https://x.ai"));
        // Claude's own stay as they were.
        assert_eq!(d("Read", json!({"file_path":"/a/z.rs"})).as_deref(), Some("z.rs"));
    }

    #[test]
    fn opencode_config_adds_the_plugin_to_theirs() {
        let p = Path::new("/run/u 1/opencode-plugin-9.js");
        let c: Value = serde_json::from_str(&opencode_config(p, None).unwrap()).unwrap();
        assert_eq!(c, json!({"plugin":["file:///run/u%201/opencode-plugin-9.js"]}));
        let c: Value = serde_json::from_str(&opencode_config(p, Some(r#"{"model":"x","plugin":["mine"]}"#)).unwrap()).unwrap();
        assert_eq!(c, json!({"model":"x","plugin":["mine","file:///run/u%201/opencode-plugin-9.js"]}));
        assert!(opencode_config(p, Some("  ")).is_some());
        assert_eq!(opencode_config(p, Some("not json")), None, "theirs is left alone");
        assert_eq!(opencode_config(p, Some(r#"{"plugin":"one"}"#)), None);
    }

    #[test]
    fn file_urls_percent_encode_all_but_unreserved_and_slash() {
        assert_eq!(file_url_path(Path::new("/run/user/1000/trix/opencode-plugin-9.js")), "/run/user/1000/trix/opencode-plugin-9.js");
        assert_eq!(file_url_path(Path::new("/a b/#?%&+=;:@[]'é~_.")), "/a%20b/%23%3F%25%26%2B%3D%3B%3A%40%5B%5D%27%C3%A9~_.");
    }

    #[test]
    fn opencode_plugin_names_trix_and_runs_no_shell() {
        for v2 in [false, true] {
            let src = opencode_plugin_source(Path::new("/opt/it's \"trix\""), v2);
            assert!(src.contains(r#"const TRIX = "/opt/it's \"trix\"";"#));
            assert!(!src.contains("__TRIX_EXE__"));
            assert!(src.contains(r#"spawn(TRIX, ["hook", "opencode""#) && !src.contains("shell:") && !src.contains("exec("));
            // Text is cut without splitting an emoji, which JSON can't carry.
            assert!(src.contains("out[k] = cut(v)") && src.contains("prompt: cut(text)"));
            assert_eq!(src.matches("slice(0, 400)").count(), 1, "only inside cut()");
        }
        // Each version's own entry point, and only that one.
        let (v1, v2) = (opencode_plugin_source(Path::new("/t"), false), opencode_plugin_source(Path::new("/t"), true));
        assert!(v1.contains("export const TrixPlugin") && !v1.contains("export default"));
        assert!(v2.contains("export default {\n  id: \"trix\",\n  setup:") && !v2.contains("export const"));
    }

    /// The OpenCode 2 plugin, run by Node against a fake `ctx`: what it
    /// reports for each event, in the OpenCode 1 plugin's terms.
    #[test]
    fn opencode_2_plugin_reports_like_the_first() {
        let src = opencode_plugin_source(Path::new("/t"), true)
            .replace(r#"import { spawn } from "node:child_process";"#, "")
            .replace("export default", "const plugin =");
        let script = format!(
            r#"{src}
const out = [];
process.env.TRIX_SOCKET = "s"; process.env.TRIX_AGENT_ID = "1";
globalThis.spawn = (_exe, argv) => {{ const p = JSON.parse(argv[2]); delete p.trix_seq; out.push(p); return {{ on() {{}}, unref() {{}} }}; }};
const hooks = {{}};
const events = [
  {{ type: "session.created", data: {{ sessionID: "sub", parentID: "s1" }} }},
  {{ type: "session.inbox.enqueued", data: {{ sessionID: "s1", item: {{ type: "user", payload: {{ text: "hi" }} }} }} }},
  {{ type: "session.execution.started", data: {{ sessionID: "s1" }} }},
  {{ type: "session.execution.started", data: {{ sessionID: "sub" }} }},
  {{ type: "permission.asked", data: {{ sessionID: "sub", action: "shell", resources: ["rm -rf dist"] }} }},
  {{ type: "permission.replied", data: {{ sessionID: "sub" }} }},
  {{ type: "form.created", data: {{ form: {{ sessionID: "s1", title: "Question", fields: [{{ title: "Which one?" }}] }} }} }},
  {{ type: "form.cancelled", data: {{ sessionID: "s1" }} }},
  {{ type: "session.execution.succeeded", data: {{ sessionID: "sub" }} }},
  {{ type: "session.execution.interrupted", data: {{ sessionID: "s1", reason: "user" }} }},
  {{ type: "session.execution.succeeded", data: {{ sessionID: "s1" }} }},
];
const ctx = {{
  tool: {{ hook: (name, fn) => {{ hooks[name] = fn; }} }},
  event: {{ subscribe: async function* () {{ for (const e of events) yield e; }} }},
}};
await plugin.setup(ctx);
await new Promise((r) => setTimeout(r, 50));
await hooks["execute.before"]({{ sessionID: "sub", tool: "shell", input: {{ command: "ls", other: 1 }} }});
await hooks["execute.after"]({{ sessionID: "sub", tool: "shell" }});
process.stdout.write(JSON.stringify(out));"#
        );
        let script = script.replace("spawn(TRIX", "globalThis.spawn(TRIX");
        let Ok(out) = std::process::Command::new("node").arg("--input-type=module").arg("-e").arg(&script).output() else { return };
        if !out.status.success() {
            panic!("{}", String::from_utf8_lossy(&out.stderr));
        }
        let got: Value = serde_json::from_slice(&out.stdout).unwrap();
        let types: Vec<&str> = got.as_array().unwrap().iter().map(|p| p["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            [
                "trix.loaded",
                "chat.message",
                "session.status",
                "permission.asked",
                "permission.replied",
                "question.asked",
                "question.rejected",
                "session.status",
                "session.status",
                "tool.execute.before",
                "tool.execute.after",
            ],
            "{got}"
        );
        let g = got.as_array().unwrap();
        assert_eq!(g[1], serde_json::json!({ "type": "chat.message", "sessionID": "s1", "prompt": "hi" }));
        assert_eq!(g[2]["status"], "busy");
        // Approvals and tools in a subagent show on the pane's own session.
        assert_eq!(g[3], serde_json::json!({ "type": "permission.asked", "sessionID": "s1", "permission": "shell", "patterns": ["rm -rf dist"] }));
        assert_eq!(g[5]["question"], "Which one?");
        // The subagent's own turn never shows; the pane's: aborted, then idle.
        assert_eq!((g[7]["status"].as_str(), g[8]["status"].as_str()), (Some("aborted"), Some("idle")));
        assert_eq!(g[9], serde_json::json!({ "type": "tool.execute.before", "sessionID": "s1", "tool": "shell", "args": { "command": "ls" } }));
        // And each one reads as the same hook trix already knows.
        let events: Vec<String> = g.iter().map(|p| condense_opencode(1, p).event).collect();
        assert_eq!(events, ["SessionStart", "UserPromptSubmit", "Busy", "PermissionRequest", "Answered", "Question", "Answered", "Interrupt", "Stop", "PreToolUse", "PostToolUse"]);
    }

    /// The plugin's `cut`, run by Node when it's installed.
    #[test]
    fn opencode_plugin_cut_keeps_emoji_whole() {
        let src = OPENCODE_PLUGIN;
        let Some(start) = src.find("function cut(") else { panic!("no cut()") };
        let end = start + src[start..].find("\n}\n").unwrap() + 2;
        let script = format!(
            "{}\nconst r = cut(\"a\".repeat(399) + \"\\u{{1F600}}b\");\nprocess.stdout.write(JSON.stringify([r.length, cut(\"x\\uD83D\")]));",
            &src[start..end]
        );
        let Ok(out) = std::process::Command::new("node").arg("-e").arg(&script).output() else { return };
        if !out.status.success() {
            return;
        }
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v, serde_json::json!([399, "x"]));
    }

    #[test]
    fn cursor_plugin_hooks_only_watch() {
        let h = cursor_hooks(Path::new("/opt/it's/trix"));
        assert_eq!(h["version"], 1);
        let hooks = h["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), CURSOR_HOOK_EVENTS.len());
        for ev in ["preToolUse", "beforeShellExecution", "beforeMCPExecution", "beforeReadFile", "subagentStart"] {
            assert!(!hooks.contains_key(ev), "{ev} decides on tools");
        }
        for ev in CURSOR_HOOK_EVENTS {
            assert_eq!(hooks[ev][0]["command"], r"'/opt/it'\''s/trix' hook cursor", "{ev}");
        }
    }

    /// As Copilot CLI 1.0.110 sent them to a `--plugin-dir` plugin's
    /// Claude-style hooks (session start, prompt, turn end, exit), and as
    /// its hooks reference documents the rest.
    #[test]
    fn condenses_copilot_hooks() {
        let c = |v: Value| condense(7, "copilot", &v);
        let m = c(json!({"hook_event_name":"SessionStart","session_id":"0cb916db-26aa-40f2-86b5-1ba81b225fd2","timestamp":"2026-11-13T11:53:30.371Z","cwd":"/tmp/w","source":"new","initial_prompt":"hello"}));
        assert_eq!((m.source.as_str(), m.event.as_str(), m.session.as_deref()), ("copilot", "SessionStart", Some("0cb916db-26aa-40f2-86b5-1ba81b225fd2")));
        let m = c(json!({"hook_event_name":"UserPromptSubmit","session_id":"s1","cwd":"/tmp/w","prompt":"fix\nthe   login"}));
        assert_eq!((m.event.as_str(), m.detail.as_deref()), ("UserPromptSubmit", Some("fix the login")));
        let m = c(json!({"hook_event_name":"Stop","session_id":"s1","transcript_path":"/home/u/.copilot/session-state/s1/events.jsonl","stop_reason":"end_turn","stop_hook_active":false}));
        assert_eq!((m.event.as_str(), m.session.as_deref(), m.transcript), ("Stop", Some("s1"), None), "its log isn't Claude's: no context gauge");
        assert_eq!(c(json!({"hook_event_name":"SessionEnd","session_id":"s1","reason":"user_exit"})).event, "SessionEnd");
        // Claude's tool names, Copilot's arguments.
        let m = c(json!({"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Bash","tool_input":{"command":"cargo   test","description":"Run tests"}}));
        assert_eq!((m.event.as_str(), m.tool.as_deref(), m.detail.as_deref()), ("PreToolUse", Some("Bash"), Some("cargo test")));
        let m = c(json!({"hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{"path":"/w/src/main.rs"}}));
        assert_eq!(m.detail.as_deref(), Some("main.rs"));
        let m = c(json!({"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"question":"Which database?","choices":["pg","sqlite"]}}));
        assert_eq!((m.tool.as_deref(), m.detail.as_deref()), (Some("AskUserQuestion"), Some("Which database?")));
        assert_eq!(c(json!({"hook_event_name":"PostToolUse","tool_name":"Bash","tool_result":{"result_type":"success"}})).event, "PostToolUse");
        assert_eq!(c(json!({"hook_event_name":"PostToolUseFailure","tool_name":"Bash","error":"denied"})).event, "PostToolUse");
        // Waiting on you: an approval, or a dialog.
        let m = c(json!({"hook_event_name":"Notification","session_id":"s1","message":"Copilot needs your permission","title":"Permission needed","notification_type":"permission_prompt"}));
        assert_eq!((m.event.as_str(), m.tool, m.detail.as_deref()), ("PermissionRequest", None, Some("Copilot needs your permission")));
        let m = c(json!({"hook_event_name":"Notification","message":"Copilot has a question","notification_type":"elicitation_dialog"}));
        assert_eq!((m.event.as_str(), m.detail.as_deref()), ("Question", Some("Copilot has a question")));
        for t in ["shell_completed", "agent_completed", "agent_idle"] {
            assert_eq!(c(json!({"hook_event_name":"Notification","message":"permission granted","notification_type":t})).event, "unknown", "{t}");
        }
        // A subagent's turn ending isn't the pane's.
        assert_eq!(c(json!({"hook_event_name":"Stop","agent_id":"sub-1","stop_reason":"end_turn"})).event, "unknown");
        assert_eq!(c(json!({"hook_event_name":"PermissionRequest","tool_name":"Bash"})).event, "unknown", "fires before Copilot's own rules");
        assert_eq!(c(json!({"sessionId":"s1","toolName":"bash"})).event, "unknown");
    }

    #[test]
    fn copilot_plugin_hooks_only_watch_and_never_fail() {
        let h = copilot_hooks(Path::new("/opt/it's/trix"));
        assert_eq!(h["version"], 1);
        let hooks = h["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), COPILOT_HOOK_EVENTS.len());
        assert!(!hooks.contains_key("PermissionRequest") && !hooks.contains_key("permissionRequest"));
        for ev in COPILOT_HOOK_EVENTS {
            let e = &hooks[ev][0];
            // A failing PreToolUse command would deny the tool.
            assert_eq!(e["bash"], r"'/opt/it'\''s/trix' hook copilot || true", "{ev}");
            assert_eq!((e["type"].as_str(), e["timeoutSec"].as_u64()), (Some("command"), Some(5)), "{ev}");
            assert_eq!(e.get("matcher").is_some(), ev.contains("ToolUse"), "{ev}");
        }
        // Claude-style names, so the payloads come in Claude's terms.
        assert!(COPILOT_HOOK_EVENTS.iter().all(|e| e.starts_with(char::is_uppercase)));
    }

    #[test]
    fn copilot_plugin_is_a_folder_with_a_manifest_and_hooks() {
        let dir = write_copilot_plugin(Path::new("/bin/trix")).unwrap();
        let read = |f: &str| serde_json::from_slice::<Value>(&std::fs::read(dir.join(f)).unwrap()).unwrap();
        assert_eq!(read("plugin.json")["name"], "trix");
        assert_eq!(read("hooks.json"), copilot_hooks(Path::new("/bin/trix")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
