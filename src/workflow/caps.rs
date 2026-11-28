//! The capability matrix: what each backend can do, at what level trix
//! knows it, and what a profile makes of it. Shipped in the binary
//! (`capabilities.toml`), never downloaded.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const SHIPPED: &str = include_str!("capabilities.toml");

/// How well trix knows an entry.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Counts as no.
    #[default]
    Unverified,
    /// From trix's source and README and the vendor's docs; not probed.
    Documented,
    /// A real probe passed, on stated versions.
    Verified,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Unverified => "unverified",
            Level::Documented => "documented",
            Level::Verified => "verified",
        }
    }
    /// Verified and documented count as yes; unverified as no.
    pub fn yes(self) -> bool {
        self != Level::Unverified
    }
}

/// Whether a kind can hand in a report: always, or only in some modes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum HandIn {
    All(bool),
    /// By sandbox mode: Codex hands in from workspace-write, not read-only.
    ByMode(BTreeMap<String, bool>),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Versions {
    pub min: String,
    pub max: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub level: Level,
    #[serde(default)]
    pub verified_with: String,
    #[serde(default)]
    pub verified_at: String,
    #[serde(default)]
    pub versions: Option<Versions>,
    pub hand_in: HandIn,
    pub read_only_enforced: bool,
    /// The best push protection the kind can have: `enforced` or `guarded`.
    pub push: String,
    pub state: String,
    /// `yes`, `after_id` or `no`.
    pub resume: String,
    /// `screen`, or `none`: a quota stop looks like an idle agent.
    pub quota_signal: String,
    /// Whether a hook can answer a permission prompt: `verified`,
    /// `documented`, `unverified`, or `none` (no decision API at all).
    pub permission_decision: String,
    pub merge_approver: bool,
    pub priced: bool,
    pub hook_timeout_s: u32,
    /// Set from `[capabilities.<kind>]` in your config.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub overridden: bool,
}

/// The backends on this machine: each kind's version, as it says it.
/// A kind that isn't here isn't installed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Installed(pub BTreeMap<String, String>);

impl Installed {
    /// Every kind, in a version inside its entry's range: for tests and
    /// for checking a workflow as its author's machine would.
    pub fn all(m: &Matrix) -> Installed {
        Installed(
            m.0.iter()
                .map(|(k, e)| {
                    (
                        k.clone(),
                        e.versions
                            .as_ref()
                            .map_or("1.0.0".into(), |v| v.min.clone()),
                    )
                })
                .collect(),
        )
    }

    /// What's on this machine: each kind's program asked for its version.
    pub fn detect(m: &Matrix) -> Installed {
        let mut out = BTreeMap::new();
        for kind in m.0.keys() {
            let program = match kind.as_str() {
                "cursor" => "cursor-agent",
                "antigravity" => "agy",
                // A custom command is whatever you name: always there.
                "custom" => {
                    out.insert(kind.clone(), "1.0.0".to_string());
                    continue;
                }
                k => k,
            };
            let Ok(o) = std::process::Command::new(program)
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .output()
            else {
                continue;
            };
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            out.insert(kind.clone(), version_in(&text).unwrap_or_default());
        }
        Installed(out)
    }
}

/// The first `1.2.3` (or `1.2`) in what a program printed.
pub fn version_in(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|w| w.trim_matches('.'))
        .find(|w| w.contains('.') && w.split('.').all(|p| !p.is_empty() && p.len() < 8))
        .map(String::from)
}

/// `1.2.3` as numbers, to compare: missing parts are 0.
fn parts(v: &str) -> Vec<u64> {
    let mut p: Vec<u64> = v.split('.').map(|x| x.parse().unwrap_or(0)).collect();
    p.resize(3.max(p.len()), 0);
    p
}

#[derive(Clone, Debug, PartialEq)]
pub struct Matrix(pub BTreeMap<String, Entry>);

impl Matrix {
    /// The matrix as shipped.
    pub fn shipped() -> Matrix {
        Matrix(toml::from_str(SHIPPED).expect("the shipped capabilities.toml parses"))
    }

    /// The shipped matrix, with your `[capabilities.<kind>]` overrides and
    /// what local probes verified (`capabilities.json`).
    pub fn load(
        overrides: &toml::Table,
        local: &BTreeMap<String, Local>,
    ) -> Result<Matrix, String> {
        let mut m = Matrix::shipped();
        for (kind, over) in overrides {
            let entry =
                m.0.get(kind)
                    .ok_or_else(|| format!("capabilities.{kind}: there's no such kind"))?;
            let mut table = toml::Table::try_from(entry).map_err(|e| e.to_string())?;
            for (k, v) in over
                .as_table()
                .ok_or_else(|| format!("capabilities.{kind}: a table of settings"))?
            {
                table.insert(k.clone(), v.clone());
            }
            let mut e: Entry = table
                .try_into()
                .map_err(|e| format!("capabilities.{kind}: {e}"))?;
            e.overridden = true;
            m.0.insert(kind.clone(), e);
        }
        for (kind, l) in local {
            if let Some(e) = m.0.get_mut(kind) {
                e.level = Level::Verified;
                e.verified_with = l.version.clone();
                e.verified_at = l.trix.clone();
                if let Some(d) = &l.permission_decision {
                    e.permission_decision = d.clone();
                }
            }
        }
        Ok(m)
    }

    pub fn get(&self, kind: &str) -> Option<&Entry> {
        self.0.get(kind)
    }

    /// The level an entry counts at for the installed version: one outside
    /// the entry's range (or other than the one a probe passed on) drops to
    /// documented.
    pub fn level(&self, kind: &str, installed: &Installed) -> Level {
        let Some(e) = self.get(kind) else {
            return Level::Unverified;
        };
        if e.level == Level::Unverified {
            return Level::Unverified;
        }
        if self.in_range(kind, installed) == Some(false) {
            return Level::Documented;
        }
        match (e.level, installed.0.get(kind)) {
            (Level::Verified, Some(v)) if !e.verified_with.is_empty() && *v != e.verified_with => {
                Level::Documented
            }
            (l, _) => l,
        }
    }

    /// Whether the installed version is inside the entry's range. None:
    /// not installed, or the entry names no range.
    pub fn in_range(&self, kind: &str, installed: &Installed) -> Option<bool> {
        let v = installed.0.get(kind).filter(|v| !v.is_empty())?;
        let r = self.get(kind)?.versions.as_ref()?;
        Some(parts(&r.min) <= parts(v) && parts(v) <= parts(&r.max))
    }

    /// Whether trix may answer this kind's permission prompts through its
    /// hook: only when that's verified for the installed version.
    pub fn decision(&self, kind: &str, installed: &Installed) -> Level {
        let Some(e) = self.get(kind) else {
            return Level::Unverified;
        };
        match e.permission_decision.as_str() {
            "verified" if self.level(kind, installed) == Level::Verified => Level::Verified,
            "verified" | "documented" => Level::Documented,
            _ => Level::Unverified,
        }
    }
}

/// What a local probe found, kept in `<state>/capabilities.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Local {
    pub version: String,
    pub trix: String,
    #[serde(default)]
    pub permission_decision: Option<String>,
    #[serde(default)]
    pub results: BTreeMap<String, String>,
}

/// Where local probe results are kept.
pub fn local_file() -> std::path::PathBuf {
    crate::worktree::state_dir().join("capabilities.json")
}

/// What local probes verified, by kind. Nothing, if none ran.
pub fn read_local() -> BTreeMap<String, Local> {
    std::fs::read(local_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

// ───────────────────────────── profiles ─────────────────────────────

/// Every command an enforced profile may allow. Anything else (a test
/// runner, a build tool, a package script, an interpreter, `git push`,
/// `git:*`, bare `Bash`) runs code the agent can change, or pushes: the
/// profile is guarded at best.
pub const SAFE_LIST: [&str; 13] = [
    "Read",
    "Glob",
    "Grep",
    "Edit",
    "Write",
    "Bash(git add:*)",
    "Bash(git commit:*)",
    "Bash(git diff:*)",
    "Bash(git status:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(trix wf submit:*)",
    "Bash(trix wf ask:*)",
];

/// The safe-list commands that don't change anything.
const READ_ONLY: [&str; 9] = [
    "Read",
    "Glob",
    "Grep",
    "Bash(git diff:*)",
    "Bash(git status:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(trix wf submit:*)",
    "Bash(trix wf ask:*)",
];

const EDIT_ONLY: &str = "Read,Edit,Write,Glob,Grep,Bash(git add:*),Bash(git commit:*),Bash(git diff:*),Bash(git status:*),Bash(git log:*),Bash(git show:*),Bash(trix wf submit:*),Bash(trix wf ask:*)";
const READ: &str =
    "Read,Glob,Grep,Bash(git diff:*),Bash(git log:*),Bash(git show:*),Bash(trix wf submit:*)";

/// The kinds that take any profile "by instruction": no arguments to give.
const INSTRUCTED: [&str; 9] = [
    "gemini",
    "opencode",
    "copilot",
    "muse",
    "grok",
    "cursor",
    "antigravity",
    "amp",
    "custom",
];

/// The profiles shipped in the binary: name, then kind, then arguments.
pub fn shipped_profiles() -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
    let claude = |tools: &str| -> Vec<String> {
        ["--permission-mode", "dontAsk", "--allowedTools", tools]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    let codex: Vec<String> = ["-s", "workspace-write", "-a", "never"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let one = |kind: &str, args: Vec<String>| BTreeMap::from([(kind.to_string(), args)]);
    let mut p = BTreeMap::new();
    p.insert("edit-only".to_string(), one("claude", claude(EDIT_ONLY)));
    let mut edit_test = one("claude", claude(&format!("{EDIT_ONLY},Bash(npm test:*)")));
    edit_test.insert("codex".into(), codex.clone());
    p.insert("edit-and-test".to_string(), edit_test);
    // No codex entry: -s read-only can't reach trix, so it could never hand in.
    p.insert("read-only".to_string(), one("claude", claude(READ)));
    p.insert(
        "review-and-test".to_string(),
        one("claude", claude(&format!("{READ},Bash(npm test:*)"))),
    );
    let mut instruct = one("codex", codex);
    for kind in INSTRUCTED {
        instruct.insert(kind.to_string(), Vec::new());
    }
    p.insert("instruct-read-only".to_string(), instruct);
    p
}

/// What trix can back up about pushes from a node.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Protection {
    /// `push: allow`: agents push themselves.
    None,
    /// The pane or check gets the guard: an accidental push fails, code
    /// that sets out to undo the guard doesn't.
    Guarded,
    /// The agent's own tools can't push or call gh.
    Enforced,
}

impl Protection {
    pub fn label(self) -> &'static str {
        match self {
            Protection::None => "none",
            Protection::Guarded => "guarded",
            Protection::Enforced => "enforced",
        }
    }
}

/// What a profile's arguments mean for one kind.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub kind: String,
    pub args: Vec<String>,
}

impl Profile {
    /// The tools a Claude profile allows without asking. None: it names none.
    pub fn allowed(&self) -> Option<Vec<String>> {
        let i = self
            .args
            .iter()
            .position(|a| a == "--allowedTools" || a == "--allowed-tools")?;
        Some(split_tools(self.args.get(i + 1)?))
    }

    fn flag(&self, name: &str) -> Option<&str> {
        let i = self.args.iter().position(|a| a == name)?;
        self.args.get(i + 1).map(String::as_str)
    }

    /// It never shows a permission prompt: what isn't allowed is refused.
    pub fn never_prompts(&self) -> bool {
        match self.kind.as_str() {
            "claude" => matches!(
                self.flag("--permission-mode"),
                Some("dontAsk" | "bypassPermissions")
            ),
            "codex" => {
                self.flag("-a") == Some("never") || self.flag("--ask-for-approval") == Some("never")
            }
            _ => false,
        }
    }

    /// Codex in a sandbox that can't reach trix's socket: it can't hand in.
    pub fn cant_hand_in(&self) -> bool {
        self.kind == "codex"
            && matches!(
                self.flag("-s").or(self.flag("--sandbox")),
                Some("read-only")
            )
    }

    /// The commands it allows that aren't on the safe list.
    pub fn off_safe_list(&self) -> Vec<String> {
        self.allowed()
            .unwrap_or_default()
            .into_iter()
            .filter(|t| !SAFE_LIST.contains(&t.as_str()))
            .collect()
    }

    /// Enforced: a Claude profile that names its tools, every one of them
    /// on the safe list, and refuses the rest. `also`: tools a permission
    /// rule would let through for this node, which count like the profile's.
    pub fn protection(&self, m: &Matrix, also: &[String]) -> Protection {
        let best = m.get(&self.kind).map_or("guarded", |e| e.push.as_str());
        let named = self.allowed().is_some();
        let safe =
            self.off_safe_list().is_empty() && also.iter().all(|t| SAFE_LIST.contains(&t.as_str()));
        // In a prompting mode, what isn't allowed asks: the rule or you decide.
        let closed = self.never_prompts()
            || self
                .flag("--permission-mode")
                .is_none_or(|m| m == "default");
        if self.kind == "claude" && best == "enforced" && named && safe && closed {
            Protection::Enforced
        } else {
            Protection::Guarded
        }
    }

    /// Read-only, enforced: a Claude profile whose every tool only reads.
    pub fn read_only_enforced(&self, m: &Matrix) -> bool {
        self.kind == "claude"
            && m.get("claude").is_some_and(|e| e.read_only_enforced)
            && self.never_prompts()
            && self
                .allowed()
                .is_some_and(|t| t.iter().all(|t| READ_ONLY.contains(&t.as_str())))
    }
}

/// `Read,Bash(git add:*),Bash(a, b)`: split at commas outside parentheses.
fn split_tools(list: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in list.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' | ' ' if depth == 0 => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(kind: &str, name: &str) -> Profile {
        Profile {
            kind: kind.into(),
            args: shipped_profiles()[name][kind].clone(),
        }
    }

    #[test]
    fn the_shipped_matrix_has_every_kind_and_an_honest_level() {
        let m = Matrix::shipped();
        for kind in crate::agent::Kind::ALL.into_iter().filter(|k| k.is_agent()) {
            let e = m
                .get(kind.label())
                .unwrap_or_else(|| panic!("no entry for {}", kind.label()));
            // Nothing ships as verified unless a probe's version and release are named.
            assert!(
                e.level != Level::Verified
                    || (!e.verified_with.is_empty() && !e.verified_at.is_empty()),
                "{}",
                kind.label()
            );
            assert!(["screen", "none"].contains(&e.quota_signal.as_str()));
            assert!(["enforced", "guarded"].contains(&e.push.as_str()));
        }
        // What the PRD's tables say, kind by kind.
        assert_eq!(
            m.get("codex").unwrap().hand_in,
            HandIn::ByMode(
                [
                    ("workspace-write".into(), true),
                    ("read-only".into(), false)
                ]
                .into()
            )
        );
        assert_eq!(
            (
                m.get("codex").unwrap().hook_timeout_s,
                m.get("claude").unwrap().hook_timeout_s
            ),
            (3, 5)
        );
        assert_eq!(m.get("custom").unwrap().quota_signal, "none");
        assert!(
            m.0.iter()
                .all(|(k, e)| e.priced == (k == "claude" || k == "codex"))
        );
        assert!(
            m.0.iter()
                .filter(|(k, _)| *k != "claude")
                .all(|(_, e)| e.push == "guarded" && !e.read_only_enforced)
        );
        for k in [
            "opencode",
            "copilot",
            "muse",
            "grok",
            "gemini",
            "cursor",
            "antigravity",
            "amp",
            "custom",
        ] {
            assert_eq!(m.get(k).unwrap().permission_decision, "none", "{k}");
        }
    }

    #[test]
    fn a_version_outside_the_range_drops_to_documented_and_rules_need_verified() {
        let local = BTreeMap::from([(
            "claude".to_string(),
            Local {
                version: "2.3.1".into(),
                trix: "1.8.0".into(),
                permission_decision: Some("verified".into()),
                results: BTreeMap::new(),
            },
        )]);
        let m = Matrix::load(&toml::Table::new(), &local).unwrap();
        let on = |v: &str| Installed(BTreeMap::from([("claude".to_string(), v.to_string())]));
        // Verified for the version the probe passed on.
        assert_eq!(
            (
                m.level("claude", &on("2.3.1")),
                m.decision("claude", &on("2.3.1"))
            ),
            (Level::Verified, Level::Verified)
        );
        // Another version in range: documented until it's probed again.
        assert_eq!(
            (
                m.level("claude", &on("2.4.0")),
                m.decision("claude", &on("2.4.0"))
            ),
            (Level::Documented, Level::Documented)
        );
        // Newer than the range: documented, with the range said to be left.
        assert_eq!(
            (
                m.in_range("claude", &on("3.0.0")),
                m.level("claude", &on("3.0.0"))
            ),
            (Some(false), Level::Documented)
        );
        assert_eq!(m.in_range("claude", &on("2.3.1")), Some(true));
        // As shipped: documented, never enough for a rule; codex unverified.
        let shipped = Matrix::shipped();
        assert_eq!(shipped.decision("claude", &on("2.3.1")), Level::Documented);
        assert_eq!(
            shipped.decision("codex", &Installed::all(&shipped)),
            Level::Unverified
        );
        assert_eq!(
            shipped.decision("gemini", &Installed::all(&shipped)),
            Level::Unverified
        );
        assert!(Level::Documented.yes() && Level::Verified.yes() && !Level::Unverified.yes());
        assert_eq!(
            version_in("claude 2.3.1 (Claude Code)").as_deref(),
            Some("2.3.1")
        );
        assert_eq!(version_in("codex-cli 0.48.0\n").as_deref(), Some("0.48.0"));
        assert_eq!(version_in("no version here"), None);
    }

    #[test]
    fn an_override_in_your_config_is_labelled() {
        let over: toml::Table =
            toml::from_str("[codex]\npermission_decision = \"documented\"\nhook_timeout_s = 4\n")
                .unwrap();
        let m = Matrix::load(&over, &BTreeMap::new()).unwrap();
        let e = m.get("codex").unwrap();
        assert!(e.overridden && e.hook_timeout_s == 4 && e.permission_decision == "documented");
        assert!(!m.get("claude").unwrap().overridden);
        let bad: toml::Table = toml::from_str("[codexx]\npriced = true\n").unwrap();
        assert!(
            Matrix::load(&bad, &BTreeMap::new())
                .unwrap_err()
                .contains("no such kind")
        );
        let bad: toml::Table = toml::from_str("[codex]\ncolour = 1\n").unwrap();
        assert!(
            Matrix::load(&bad, &BTreeMap::new())
                .unwrap_err()
                .contains("capabilities.codex")
        );
    }

    #[test]
    fn only_a_safe_list_profile_is_enforced() {
        let m = Matrix::shipped();
        // Every command on the safe list: enforced.
        assert_eq!(
            profile("claude", "edit-only").protection(&m, &[]),
            Protection::Enforced
        );
        assert_eq!(
            profile("claude", "read-only").protection(&m, &[]),
            Protection::Enforced
        );
        assert!(profile("claude", "read-only").read_only_enforced(&m));
        // A test command runs code the agent can edit: guarded, and read-only by instruction.
        let p = profile("claude", "edit-and-test");
        assert_eq!(
            (p.protection(&m, &[]), p.off_safe_list()),
            (Protection::Guarded, vec!["Bash(npm test:*)".to_string()])
        );
        let p = profile("claude", "review-and-test");
        assert_eq!(
            (p.protection(&m, &[]), p.read_only_enforced(&m)),
            (Protection::Guarded, false)
        );
        assert!(
            !profile("claude", "edit-only").read_only_enforced(&m),
            "it edits"
        );
        // git:*, git push, a shell, bare Bash, no list at all.
        let claude = |tools: &str| Profile {
            kind: "claude".into(),
            args: vec![
                "--permission-mode".into(),
                "dontAsk".into(),
                "--allowedTools".into(),
                tools.into(),
            ],
        };
        for tools in [
            "Read,Bash(git:*)",
            "Read,Bash(git push:*)",
            "Read,Bash",
            "Read,Bash(sh:*)",
            "Read,Bash(cargo build:*)",
            "Read,Bash(npm run lint:*)",
        ] {
            assert_eq!(
                claude(tools).protection(&m, &[]),
                Protection::Guarded,
                "{tools}"
            );
        }
        assert_eq!(
            Profile {
                kind: "claude".into(),
                args: vec![]
            }
            .protection(&m, &[]),
            Protection::Guarded
        );
        // A rule that lets npm install through makes a safe profile guarded.
        let ask = Profile {
            kind: "claude".into(),
            args: vec![
                "--permission-mode".into(),
                "default".into(),
                "--allowedTools".into(),
                EDIT_ONLY.into(),
            ],
        };
        assert_eq!(ask.protection(&m, &[]), Protection::Enforced);
        assert_eq!(
            ask.protection(&m, &["Bash(npm install:*)".to_string()]),
            Protection::Guarded
        );
        assert!(!ask.never_prompts() && profile("claude", "edit-only").never_prompts());
        // Every other kind is guarded at best, whatever its profile.
        assert_eq!(
            profile("codex", "edit-and-test").protection(&m, &[]),
            Protection::Guarded
        );
        assert_eq!(
            profile("gemini", "instruct-read-only").protection(&m, &[]),
            Protection::Guarded
        );
        assert!(profile("codex", "edit-and-test").never_prompts());
        // Codex read-only can't reach trix.
        assert!(
            Profile {
                kind: "codex".into(),
                args: vec!["-s".into(), "read-only".into()]
            }
            .cant_hand_in()
        );
        assert!(!profile("codex", "instruct-read-only").cant_hand_in());
        assert!(!shipped_profiles()["read-only"].contains_key("codex"));
        assert_eq!(
            split_tools("Read, Bash(git add:*),Bash(a, b)"),
            vec!["Read", "Bash(git add:*)", "Bash(a, b)"]
        );
        // The safe list is exactly the PRD's.
        assert_eq!(SAFE_LIST.len(), 13);
        assert!(
            !SAFE_LIST
                .iter()
                .any(|t| t.contains("push") || t.contains("npm") || *t == "Bash")
        );
    }
}
