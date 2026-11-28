//! Probes: run a backend for real in a scratch clone and see what it can
//! do, so a matrix entry can be `verified` and not only `documented`.
//!
//! `trix workflow capabilities --verify` runs them. They're opt-in, spend
//! a little quota, and never run in CI: CI tests this harness against the
//! fake backends in `tests/fixtures/backends/`.
//!
//! Each probe asks the backend, headless, to run one command, and looks at
//! what happened: whether a report was handed in (under each shipped
//! profile), whether a push reached a remote the guard disables, whether
//! the PermissionRequest hook was asked and its decision taken, and
//! whether a decision that comes after the hook's timeout is ignored.

use super::caps::{Local, Matrix, shipped_profiles};
use super::guard;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// What the matrix says is what happened.
    Verified,
    /// It isn't.
    Failed(String),
    /// The event couldn't be triggered: it doesn't count as verified.
    Inconclusive(String),
    /// Not run: the kind has no headless mode, or the probe doesn't apply.
    Skipped(String),
}

impl Outcome {
    pub fn text(&self) -> String {
        match self {
            Outcome::Verified => "verified".into(),
            Outcome::Failed(why) => format!("failed: {why}"),
            Outcome::Inconclusive(why) => format!("inconclusive: {why}"),
            Outcome::Skipped(why) => format!("skipped: {why}"),
        }
    }
}

/// How to run one backend's probes.
pub struct Harness {
    pub kind: String,
    /// The folder the backend's program is in, when it isn't on PATH: the
    /// fake backends' folder, in tests.
    pub bin: Option<PathBuf>,
    /// How long the backend's hook may take, as the matrix says.
    pub hook_timeout: Duration,
    /// How long one run of the backend may take.
    pub run_timeout: Duration,
    /// More environment for the backend (`FAKE_MODE`, in tests).
    pub env: Vec<(String, String)>,
    /// How often an event that didn't fire is tried again.
    pub tries: u32,
}

impl Harness {
    pub fn real(kind: &str, m: &Matrix) -> Harness {
        Harness {
            kind: kind.into(),
            bin: None,
            hook_timeout: Duration::from_secs(
                m.get(kind).map_or(5, |e| e.hook_timeout_s.max(1)) as u64
            ),
            run_timeout: Duration::from_secs(180),
            env: Vec::new(),
            tries: 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Results {
    pub kind: String,
    pub version: String,
    pub outcomes: BTreeMap<String, Outcome>,
}

impl Results {
    /// Every hand-in probe and the blocked push passed: the entry is verified.
    pub fn verified(&self) -> bool {
        let core: Vec<&Outcome> = self
            .outcomes
            .iter()
            .filter(|(k, _)| k.starts_with("hand-in") || *k == "push-blocked")
            .map(|(_, o)| o)
            .collect();
        !core.is_empty() && core.iter().all(|o| **o == Outcome::Verified)
    }

    /// Whether the hook can decide: `verified` only if its decision was
    /// taken and a late one wasn't; `unverified` if either failed; unknown
    /// (the entry keeps what it says) if a probe was inconclusive.
    pub fn decision(&self) -> Option<String> {
        let get = |k: &str| self.outcomes.get(k);
        match (get("permission-decision"), get("hook-timeout")) {
            (Some(Outcome::Verified), Some(Outcome::Verified)) => Some("verified".into()),
            (Some(Outcome::Failed(_)), _) | (_, Some(Outcome::Failed(_))) => {
                Some("unverified".into())
            }
            _ => None,
        }
    }

    /// What goes into `capabilities.json`.
    pub fn local(&self) -> Local {
        Local {
            version: self.version.clone(),
            trix: env!("CARGO_PKG_VERSION").into(),
            verified: self.verified(),
            permission_decision: self.decision(),
            results: self
                .outcomes
                .iter()
                .map(|(k, o)| (k.clone(), o.text()))
                .collect(),
        }
    }

    pub fn render(&self) -> String {
        let mut s = format!(
            "{} {}\n",
            self.kind,
            if self.version.is_empty() {
                "(version unknown)"
            } else {
                &self.version
            }
        );
        for (name, o) in &self.outcomes {
            s.push_str(&format!("  {name:<28} {}\n", o.text()));
        }
        s.push_str(&format!(
            "  => entry {}, prompt approvals {}\n",
            if self.verified() {
                "verified"
            } else {
                "stays documented"
            },
            match self.decision().as_deref() {
                Some("verified") => "verified",
                Some(_) => "not possible (unverified)",
                None => "unchanged",
            }
        ));
        s
    }
}

/// The backend's headless command line: its program, and its arguments
/// around the prompt. None: trix knows no headless mode for it.
fn headless(kind: &str, prompt: &str, args: &[String]) -> Option<(String, Vec<String>)> {
    let mut argv: Vec<String> = match kind {
        "claude" | "gemini" => vec!["-p".into(), prompt.into()],
        "codex" => vec!["exec".into(), prompt.into()],
        _ => return None,
    };
    // `codex exec` never asks, and doesn't take the flag that says so.
    let mut skip = false;
    let args = args.iter().filter(|a| {
        let drop = skip || (kind == "codex" && *a == "-a");
        skip = kind == "codex" && *a == "-a";
        !drop
    });
    argv.extend(args.cloned());
    Some((kind.to_string(), argv))
}

fn sh(dir: &Path, script: &str) -> bool {
    std::process::Command::new("sh")
        .current_dir(dir)
        .args(["-c", script])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// What's said to the backend: one command to run, between two lines a
/// stand-in can find it by.
fn ask(cmd: &str) -> String {
    format!(
        "This is an automated capability probe. Run exactly this one shell command, once, with your shell tool, \
         then stop. Don't explain, don't retry, don't run anything else.\nRUN:\n{cmd}\nEND"
    )
}

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    /// A throwaway clone with a bare origin beside it, and a `trix` on
    /// PATH that only records that a report was handed in.
    fn new(kind: &str) -> Result<Scratch, String> {
        let dir = std::env::temp_dir().join(format!("trix-probe-{kind}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).map_err(|e| e.to_string())?;
        let dir = dir.canonicalize().map_err(|e| e.to_string())?;
        let shim = "#!/bin/sh\n# The probe's stand-in for trix: it records a hand-in, nothing else.\n\
                    case \"$1 $2\" in \"wf submit\") touch \"$TRIX_PROBE_DIR/handed-in\"; echo \"report taken\" ;; *) exit 1 ;; esac\n";
        std::fs::write(dir.join("bin/trix"), shim).map_err(|e| e.to_string())?;
        let made = sh(
            &dir,
            "chmod +x bin/trix && mkdir empty && git init -q --bare origin.git && git init -q repo && cd repo && \
             echo probe > README.md && git add -A && \
             git -c user.name=probe -c user.email=probe@example.com commit -q -m probe && \
             git remote add origin ../origin.git && git push -q origin HEAD:refs/heads/main",
        );
        if !made {
            return Err("couldn't make the scratch clone".into());
        }
        Ok(Scratch { dir })
    }
    fn repo(&self) -> PathBuf {
        self.dir.join("repo")
    }
    fn marker(&self, name: &str) -> bool {
        self.dir.join(name).exists()
    }
    fn clear(&self) {
        for m in ["handed-in", "hook-called", "repo/decided.txt"] {
            let _ = std::fs::remove_file(self.dir.join(m));
        }
    }
    fn pushed(&self) -> bool {
        sh(
            &self.dir.join("origin.git"),
            "git rev-parse -q --verify refs/heads/probe >/dev/null",
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Harness {
    /// Run the backend once, headless, on `cmd`. False: it has no headless mode.
    fn run(&self, s: &Scratch, cmd: &str, args: &[String], guarded: bool) -> bool {
        let Some((program, argv)) = headless(&self.kind, &ask(cmd), args) else {
            return false;
        };
        let program = self
            .bin
            .as_ref()
            .map_or(PathBuf::from(&program), |b| b.join(&program));
        let mut c = std::process::Command::new(program);
        let path = std::env::var("PATH").unwrap_or_default();
        c.current_dir(s.repo())
            .args(argv)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .env("PATH", format!("{}:{path}", s.dir.join("bin").display()))
            .env("TRIX_PROBE_DIR", &s.dir);
        for k in guard::TRIX_VARS {
            c.env_remove(k);
        }
        for (k, v) in &self.env {
            c.env(k, v);
        }
        if guarded {
            guard::apply(&mut c, &s.dir.join("empty"));
        }
        let Ok(mut child) = c.spawn() else {
            return true;
        };
        let end = Instant::now() + self.run_timeout;
        while Instant::now() < end {
            if child.try_wait().ok().flatten().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        true
    }

    /// The backend's arguments that hand it a PermissionRequest hook, in a
    /// mode that asks before it runs what isn't allowed.
    fn hooked(&self, s: &Scratch, slow: bool) -> Option<Vec<String>> {
        let wait = self.hook_timeout.as_secs().max(1);
        // The hook says it was asked, then allows: at once, or after the
        // backend has stopped waiting for it.
        let script = format!(
            "#!/bin/sh\ncat > /dev/null\ntouch \"{dir}/hook-called\"\n{sleep}printf '%s' \
             '{{\"hookSpecificOutput\":{{\"hookEventName\":\"PermissionRequest\",\"decision\":{{\"behavior\":\"allow\"}}}}}}'\n",
            dir = s.dir.display(),
            sleep = if slow {
                format!("sleep {}\n", wait + 2)
            } else {
                String::new()
            },
        );
        let hook = s.dir.join("hook.sh");
        std::fs::write(&hook, script).ok()?;
        sh(&s.dir, "chmod +x hook.sh");
        let command = shell_words::quote(&hook.display().to_string()).to_string();
        match self.kind.as_str() {
            "claude" => {
                let settings = serde_json::json!({"hooks": {"PermissionRequest": [
                    {"matcher": "*", "hooks": [{"type": "command", "command": command, "timeout": wait}]}
                ]}});
                let file = s.dir.join("settings.json");
                std::fs::write(&file, serde_json::to_string_pretty(&settings).ok()?).ok()?;
                let args = [
                    "--permission-mode",
                    "default",
                    "--allowedTools",
                    "Read",
                    "--settings",
                ];
                Some(
                    args.iter()
                        .map(|a| a.to_string())
                        .chain([file.display().to_string()])
                        .collect(),
                )
            }
            "codex" => {
                let hooks = format!(
                    "hooks={{PermissionRequest=[{{hooks=[{{type=\"command\",command=\"{command}\",timeout={wait}}}]}}]}}"
                );
                Some(
                    ["-s", "workspace-write", "-c"]
                        .iter()
                        .map(|a| a.to_string())
                        .chain([hooks])
                        .collect(),
                )
            }
            _ => None,
        }
    }

    /// Whether the hook is asked, and whether what it says is taken.
    /// `slow`: it answers after the timeout, and must not be heard.
    fn decision(&self, s: &Scratch, slow: bool) -> Outcome {
        let Some(args) = self.hooked(s, slow) else {
            return Outcome::Skipped("the kind has no hook that could decide".into());
        };
        for _ in 0..self.tries.max(1) {
            s.clear();
            self.run(s, "touch decided.txt", &args, false);
            let (asked, ran) = (s.marker("hook-called"), s.marker("repo/decided.txt"));
            match (asked, ran, slow) {
                (false, _, _) => continue,
                (true, true, false) | (true, false, true) => return Outcome::Verified,
                (true, false, false) => {
                    return Outcome::Failed(
                        "the hook was asked, and its allow wasn't taken".into(),
                    );
                }
                (true, true, true) => {
                    return Outcome::Failed(
                        "a decision that came after the hook's timeout was still taken".into(),
                    );
                }
            }
        }
        Outcome::Inconclusive(format!(
            "no permission request reached the hook in {} tries",
            self.tries.max(1)
        ))
    }

    /// Every probe for this backend.
    pub fn probe(&self, m: &Matrix, version: &str) -> Result<Results, String> {
        let mut out = BTreeMap::new();
        let skipped = |why: &str| Outcome::Skipped(why.to_string());
        if headless(&self.kind, "", &[]).is_none() {
            out.insert(
                "hand-in".to_string(),
                skipped("trix knows no headless mode for this kind: not probed"),
            );
            return Ok(Results {
                kind: self.kind.clone(),
                version: version.into(),
                outcomes: out,
            });
        }
        let s = Scratch::new(&self.kind)?;
        // Hand-in, under each shipped profile that has arguments for the
        // kind, and for Codex in the sandbox the matrix says can't.
        let mut profiles: Vec<(String, Vec<String>, bool)> = shipped_profiles()
            .into_iter()
            .filter_map(|(name, by_kind)| Some((name, by_kind.get(&self.kind)?.clone(), true)))
            .collect();
        if self.kind == "codex" {
            profiles.push((
                "sandbox-read-only".into(),
                vec!["-s".into(), "read-only".into()],
                false,
            ));
        }
        let mut hands_in = false;
        let mut writer: Option<Vec<String>> = None;
        for (name, args, expect) in profiles {
            s.clear();
            self.run(&s, "trix wf submit probe", &args, false);
            let got = s.marker("handed-in");
            hands_in |= got;
            if got && writer.is_none() {
                writer = Some(args.clone());
            }
            let o = match (got, expect) {
                (true, true) | (false, false) => Outcome::Verified,
                (false, true) => Outcome::Failed("it didn't hand in".into()),
                (true, false) => {
                    Outcome::Failed("it handed in from a sandbox the matrix says can't".into())
                }
            };
            out.insert(format!("hand-in:{name}"), o);
        }
        // A push to a remote the guard disables.
        let push = match &writer {
            None if !hands_in => Outcome::Inconclusive(
                "it never ran a command, so a push it didn't make proves nothing".into(),
            ),
            _ => {
                let args = writer.unwrap_or_default();
                self.run(&s, "git push origin HEAD:refs/heads/probe", &args, true);
                if s.pushed() {
                    Outcome::Failed("the push reached the remote".into())
                } else {
                    Outcome::Verified
                }
            }
        };
        out.insert("push-blocked".into(), push);
        let none = m
            .get(&self.kind)
            .is_none_or(|e| e.permission_decision == "none");
        if none {
            out.insert(
                "permission-decision".into(),
                skipped("the kind has no decision API"),
            );
            out.insert(
                "hook-timeout".into(),
                skipped("the kind has no decision API"),
            );
        } else {
            let decided = self.decision(&s, false);
            let late = match &decided {
                Outcome::Verified => self.decision(&s, true),
                other => Outcome::Skipped(format!(
                    "the decision probe was {}",
                    other.text().split(':').next().unwrap_or_default()
                )),
            };
            out.insert("permission-decision".into(), decided);
            out.insert("hook-timeout".into(), late);
        }
        Ok(Results {
            kind: self.kind.clone(),
            version: version.into(),
            outcomes: out,
        })
    }
}

/// `trix workflow capabilities --verify`: probe every installed backend
/// (or the one named), record what was found, and say it.
pub fn verify(
    m: &Matrix,
    installed: &super::caps::Installed,
    only: Option<&str>,
) -> Result<String, String> {
    let mut local = super::caps::read_local();
    let mut said = String::new();
    for (kind, version) in installed
        .0
        .iter()
        .filter(|(k, _)| only.is_none_or(|o| o == k.as_str()) && *k != "custom")
    {
        let r = Harness::real(kind, m).probe(m, version)?;
        said.push_str(&r.render());
        local.insert(kind.clone(), r.local());
    }
    if said.is_empty() {
        return Err(match only {
            Some(k) => format!("{k} isn't installed"),
            None => "no backend is installed".into(),
        });
    }
    let file = super::caps::local_file();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    super::store::write_atomic(
        &file,
        &serde_json::to_vec_pretty(&local).map_err(|e| e.to_string())?,
    )?;
    said.push_str(&format!("\nrecorded in {}\n", crate::app::tilde(&file)));
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::caps::{Installed, Level};
    use crate::workflow::check::{Ctx, check};

    /// A harness on the fake backends: no real backend, no login, no network.
    fn fake(kind: &str, mode: &str) -> Harness {
        Harness {
            kind: kind.into(),
            bin: Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/backends")),
            hook_timeout: Duration::from_secs(1),
            run_timeout: Duration::from_secs(20),
            env: vec![("FAKE_MODE".into(), mode.into())],
            tries: 3,
        }
    }

    /// Each test probes a kind of its own, so their scratch clones don't collide.
    fn probe_as(kind: &str, mode: &str) -> Results {
        let m = Matrix::shipped();
        let h = fake(kind, mode);
        let version = match kind {
            "claude" => "2.3.1",
            "codex" => "0.167.2",
            _ => "0.30.1",
        };
        let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        h.probe(&m, version).unwrap()
    }

    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn get<'a>(r: &'a Results, k: &str) -> &'a Outcome {
        r.outcomes
            .get(k)
            .unwrap_or_else(|| panic!("no {k} in {:?}", r.outcomes.keys()))
    }

    #[test]
    fn a_backend_that_does_what_the_matrix_says_is_verified() {
        let r = probe_as("claude", "");
        // Hand-in under each shipped profile Claude has.
        for p in ["edit-only", "edit-and-test", "read-only", "review-and-test"] {
            assert_eq!(
                get(&r, &format!("hand-in:{p}")),
                &Outcome::Verified,
                "{}",
                r.render()
            );
        }
        assert!(
            !r.outcomes.contains_key("hand-in:instruct-read-only"),
            "Claude has no such profile"
        );
        // A push to the disabled remote didn't arrive.
        assert_eq!(get(&r, "push-blocked"), &Outcome::Verified);
        // The hook was asked and its allow taken; a late allow wasn't.
        assert_eq!(get(&r, "permission-decision"), &Outcome::Verified);
        assert_eq!(get(&r, "hook-timeout"), &Outcome::Verified);
        assert!(r.verified() && r.decision().as_deref() == Some("verified"));
        // Recorded, it raises the local entry to verified for that version, rules included.
        let local = BTreeMap::from([("claude".to_string(), r.local())]);
        let m = Matrix::load(&toml::Table::new(), &local).unwrap();
        let on = Installed(BTreeMap::from([(
            "claude".to_string(),
            "2.3.1".to_string(),
        )]));
        assert_eq!(
            (m.level("claude", &on), m.decision("claude", &on)),
            (Level::Verified, Level::Verified)
        );
        assert_eq!(
            m.get("claude").unwrap().verified_at,
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            r.render()
                .contains("=> entry verified, prompt approvals verified"),
            "{}",
            r.render()
        );
    }

    #[test]
    fn an_event_that_never_fires_is_inconclusive_and_doesnt_count() {
        let r = probe_as("claude", "no-hook-event");
        assert_eq!(
            get(&r, "permission-decision"),
            &Outcome::Inconclusive("no permission request reached the hook in 3 tries".into())
        );
        assert!(matches!(get(&r, "hook-timeout"), Outcome::Skipped(_)));
        // The rest passed, so the entry is verified, but not for rules.
        assert!(r.verified() && r.decision().is_none());
        let local = BTreeMap::from([("claude".to_string(), r.local())]);
        let m = Matrix::load(&toml::Table::new(), &local).unwrap();
        let on = Installed(BTreeMap::from([(
            "claude".to_string(),
            "2.3.1".to_string(),
        )]));
        assert_eq!(
            (m.level("claude", &on), m.decision("claude", &on)),
            (Level::Verified, Level::Documented)
        );
    }

    #[test]
    fn a_hook_whose_decision_isnt_taken_or_is_taken_late_fails() {
        let r = probe_as("claude", "ignores-decision");
        assert_eq!(
            get(&r, "permission-decision"),
            &Outcome::Failed("the hook was asked, and its allow wasn't taken".into())
        );
        assert_eq!(r.decision().as_deref(), Some("unverified"));
        let r = probe_as("claude", "late-allow-taken");
        assert_eq!(get(&r, "permission-decision"), &Outcome::Verified);
        assert_eq!(
            get(&r, "hook-timeout"),
            &Outcome::Failed(
                "a decision that came after the hook's timeout was still taken".into()
            )
        );
        assert_eq!(
            r.decision().as_deref(),
            Some("unverified"),
            "a late decision that's heard can't be trusted"
        );
        // Recorded as unverified, a rule on it is an error.
        let local = BTreeMap::from([("claude".to_string(), r.local())]);
        let m = Matrix::load(&toml::Table::new(), &local).unwrap();
        assert_eq!(m.decision("claude", &Installed::all(&m)), Level::Unverified);
    }

    #[test]
    fn a_backend_that_cant_hand_in_or_gets_past_the_guard_fails() {
        let r = probe_as("claude", "cant-submit");
        assert_eq!(
            get(&r, "hand-in:edit-only"),
            &Outcome::Failed("it didn't hand in".into())
        );
        assert!(
            matches!(get(&r, "push-blocked"), Outcome::Inconclusive(_)),
            "{}",
            r.render()
        );
        assert!(!r.verified());
        assert!(r.render().contains("=> entry stays documented"));
        // Code that undoes the guard pushes: the probe sees the ref arrive.
        let r = probe_as("gemini", "unsets-guard");
        assert_eq!(
            get(&r, "push-blocked"),
            &Outcome::Verified,
            "gemini's stand-in keeps the guard"
        );
        let h = fake("claude", "unsets-guard");
        let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("claude-unsets").unwrap();
        let args: Vec<String> = ["--permission-mode", "dontAsk", "--allowedTools", "Bash"]
            .iter()
            .map(|a| a.to_string())
            .collect();
        h.run(&s, "git push origin HEAD:refs/heads/probe", &args, true);
        assert!(
            s.pushed(),
            "the guard is undoable, which is why it's called guarded and never enforced"
        );
    }

    #[test]
    fn codex_hands_in_from_workspace_write_only_and_its_decision_is_recorded() {
        let r = probe_as("codex", "");
        assert_eq!(
            get(&r, "hand-in:edit-and-test"),
            &Outcome::Verified,
            "{}",
            r.render()
        );
        assert_eq!(get(&r, "hand-in:instruct-read-only"), &Outcome::Verified);
        // The matrix says read-only can't hand in, and it can't: verified.
        assert_eq!(get(&r, "hand-in:sandbox-read-only"), &Outcome::Verified);
        assert_eq!(
            get(&r, "push-blocked"),
            &Outcome::Verified,
            "guarded: the guard stopped it"
        );
        // Its hook never gets a request it could decide: recorded, not verified.
        assert!(
            matches!(get(&r, "permission-decision"), Outcome::Inconclusive(_)),
            "{}",
            r.render()
        );
        assert_eq!(r.decision(), None);
        let local = BTreeMap::from([("codex".to_string(), r.local())]);
        assert!(local["codex"].results["permission-decision"].starts_with("inconclusive"));
        // So Codex prompt approvals stay out: a rule on it is a check error.
        let m = Matrix::load(&toml::Table::new(), &local).unwrap();
        let mut ctx = Ctx::ideal(&m);
        ctx.installed.0.insert("codex".into(), "0.167.2".into());
        assert_eq!(m.level("codex", &ctx.installed), Level::Verified);
        let mut w = crate::workflow::store::builtin("issue-to-pr").unwrap();
        w.approvals
            .permissions
            .push(crate::workflow::model::PermRule {
                node: "implementer".into(),
                mode: crate::workflow::model::Allow::Allow,
                approver: Some("rule".into()),
                tools: vec!["shell".into()],
                paths: vec![],
            });
        assert!(check(&w, &m, &ctx).has("rule-no-decision"));
        // If a Codex's hook does decide, the probe says so.
        let r = probe_as("codex", "decides");
        assert_eq!(
            get(&r, "permission-decision"),
            &Outcome::Verified,
            "{}",
            r.render()
        );
    }

    #[test]
    fn a_kind_with_no_decision_api_or_no_headless_mode_is_skipped() {
        let r = probe_as("gemini", "");
        assert_eq!(
            get(&r, "hand-in:instruct-read-only"),
            &Outcome::Verified,
            "{}",
            r.render()
        );
        assert_eq!(get(&r, "push-blocked"), &Outcome::Verified);
        assert!(matches!(
            get(&r, "permission-decision"),
            Outcome::Skipped(_)
        ));
        assert!(r.verified() && r.decision().is_none());
        let r = probe_as("cursor", "");
        assert_eq!(r.outcomes.len(), 1);
        assert!(matches!(get(&r, "hand-in"), Outcome::Skipped(_)));
        assert!(!r.verified(), "nothing was probed, so nothing is verified");
        assert_eq!(Outcome::Skipped("x".into()).text(), "skipped: x");
    }
}
