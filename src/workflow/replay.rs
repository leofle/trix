//! Seeded replay of triage: a recording `Env` over a real temporary git
//! repository and a fake GitHub, and the scenarios whose goldens freeze
//! what triage does.
//!
//! A golden holds every `Env` call in order, every prompt word for word,
//! the case folder (`case.json` and the report files) and the repository
//! (worktrees, branches and refs, here and on origin). The goldens were
//! recorded against the triage runner of 1.7.8, before any of it moved,
//! and are never recorded again: the generic runner that drives triage
//! now must replay them identically. `TRIX_BLESS=1` writes a golden that doesn't exist yet, and
//! refuses to overwrite one that does.

use crate::case::{self, Ask, Case, Env, Op, Pr, Role, RoleAgent, Seen, Stage};
use crate::config::TriageOpts;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// When every scenario starts.
pub const T0: u64 = 1_795_000_000;

/// The runner under replay: one look at the case.
fn step(c: &mut Case, root: &Path, env: &mut dyn Env) -> bool {
    super::runner::step(c, root, env)
}

/// A pull request as the fake GitHub has it.
#[derive(Clone, Debug)]
pub struct HubPr {
    pub number: u64,
    pub title: String,
    pub head: String,
    pub base: String,
    pub state: &'static str,
    pub fork: bool,
}

/// GitHub, as far as triage asks it anything.
#[derive(Default)]
pub struct Hub {
    /// Exact argument lists and their answers, looked at first.
    pub answers: Vec<(String, String)>,
    pub prs: Vec<HubPr>,
    /// The check runs every commit has now.
    pub ci: String,
    /// What `mergeable` says, one per look, the last one again after.
    pub mergeable: Vec<&'static str>,
    /// Errors the next `gh pr merge`s fail with.
    pub merge_errors: Vec<String>,
    /// The PRs the issue says close it.
    pub closes: Vec<u64>,
    pub repo_info: Option<String>,
    pub review: &'static str,
    pub next_pr: u64,
    /// The body of every PR opened.
    pub pr_bodies: Vec<String>,
}

/// An agent as it was started.
#[derive(Clone, Debug)]
pub struct Spawned {
    pub name: String,
    pub kind: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub prompt: String,
    pub guarded: bool,
}

/// The world a scenario runs in, and the record of what the runner did.
pub struct World {
    name: String,
    pub base: PathBuf,
    pub root: PathBuf,
    pub repo: PathBuf,
    pub origin: PathBuf,
    pub now: u64,
    pub seen: Vec<Seen>,
    pub keys: VecDeque<char>,
    pub hub: Hub,
    /// Agents that fail to start.
    pub refuse: Vec<String>,
    /// Every agent started, with its prompt and whether its pane got the guard.
    pub spawned: Vec<Spawned>,
    /// Every message sent to an open pane.
    pub sent: Vec<(String, String)>,
    /// What each agent's screen shows now.
    pub screens: std::collections::BTreeMap<String, String>,
    pub notified: Vec<(String, String)>,
    /// Every check run: where, what, and whether the guard was on.
    pub checks: Vec<(PathBuf, Vec<String>, bool)>,
    pub opened: Vec<String>,
    guard_next: bool,
    trace: Vec<String>,
    commits: u32,
    ticks: u32,
}

pub fn runs(list: &[(&str, &str, Option<&str>)]) -> String {
    let runs: Vec<serde_json::Value> = list
        .iter()
        .map(|(n, status, conclusion)| serde_json::json!({"name": n, "status": status, "conclusion": conclusion}))
        .collect();
    serde_json::json!({"total_count": runs.len(), "check_runs": runs}).to_string()
}

pub fn green() -> String {
    runs(&[("test", "completed", Some("success"))])
}

/// git, the same on every machine: no personal or system config, one
/// author, and a date that only depends on how many commits came before.
fn git_cmd(dir: &Path, home: &Path, date: u64) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    let when = format!("{date} +0000");
    c.current_dir(dir)
        .stdin(std::process::Stdio::null())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Replay")
        .env("GIT_AUTHOR_EMAIL", "replay@example.com")
        .env("GIT_COMMITTER_NAME", "Replay")
        .env("GIT_COMMITTER_EMAIL", "replay@example.com")
        .env("GIT_AUTHOR_DATE", &when)
        .env("GIT_COMMITTER_DATE", &when)
        .env("TZ", "UTC")
        .env("LC_ALL", "C");
    c
}

fn output(mut c: std::process::Command) -> Result<String, String> {
    let out = c.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            err
        })
    }
}

impl World {
    /// A repository with an origin, `main`, and a PR branch `rate-limits`
    /// (PR #12) one commit ahead.
    pub fn new(name: &str) -> World {
        let base = std::env::temp_dir().join(format!("trix-replay-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let (root, repo, origin) = (
            base.join("cases"),
            base.join("repo"),
            base.join("origin.git"),
        );
        std::fs::create_dir_all(&root).unwrap();
        let mut w = World {
            name: name.into(),
            base,
            root,
            repo,
            origin,
            now: T0,
            seen: Vec::new(),
            keys: VecDeque::new(),
            hub: Hub {
                ci: green(),
                review: "",
                next_pr: 20,
                ..Hub::default()
            },
            refuse: Vec::new(),
            spawned: Vec::new(),
            sent: Vec::new(),
            screens: Default::default(),
            notified: Vec::new(),
            checks: Vec::new(),
            opened: Vec::new(),
            guard_next: false,
            trace: Vec::new(),
            commits: 0,
            ticks: 0,
        };
        let (base, repo, origin) = (w.base.clone(), w.repo.clone(), w.origin.clone());
        w.sh(
            &base,
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        w.sh(&base, &["init", "--quiet", "-b", "main", "repo"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("README.md"), "# app\n").unwrap();
        std::fs::write(
            repo.join("src/limit.rs"),
            "pub fn allowed() -> bool { true }\n",
        )
        .unwrap();
        std::fs::write(repo.join(".gitignore"), ".trix/\n").unwrap();
        w.sh(&repo, &["add", "-A"]);
        w.commit(&repo, "init");
        w.sh(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        w.sh(&repo, &["push", "--quiet", "origin", "main"]);
        w.sh(&repo, &["checkout", "--quiet", "-b", "rate-limits"]);
        std::fs::write(
            repo.join("src/limit.rs"),
            "pub fn allowed() -> bool { false }\n",
        )
        .unwrap();
        w.sh(&repo, &["add", "-A"]);
        w.commit(&repo, "Add rate limits");
        w.sh(&repo, &["push", "--quiet", "origin", "rate-limits"]);
        w.sh(&repo, &["checkout", "--quiet", "main"]);
        w.sh(&repo, &["branch", "--quiet", "-D", "rate-limits"]);
        w.hub.prs.push(HubPr {
            number: 12,
            title: "Add rate limits".into(),
            head: "rate-limits".into(),
            base: "main".into(),
            state: "OPEN",
            fork: false,
        });
        w
    }

    /// git, as the scenario itself runs it: not part of the record.
    pub fn sh(&mut self, dir: &Path, args: &[&str]) -> String {
        let mut c = git_cmd(dir, &self.base, T0 + self.commits as u64);
        c.args(args);
        output(c).unwrap_or_else(|e| panic!("git {}: {e}", args.join(" ")))
    }

    pub fn commit(&mut self, dir: &Path, msg: &str) -> String {
        self.commits += 1;
        self.sh(dir, &["commit", "--quiet", "-m", msg]);
        self.sh(dir, &["rev-parse", "HEAD"]).trim().to_string()
    }

    /// The head of a branch on origin.
    pub fn head(&mut self, branch: &str) -> String {
        let origin = self.origin.clone();
        self.sh(&origin, &["rev-parse", &format!("refs/heads/{branch}")])
            .trim()
            .to_string()
    }

    /// A case for PR #12, stored, as `trix triage 12` would.
    pub fn case(&mut self, opts: &TriageOpts) -> Case {
        let sha = self.head("rate-limits");
        let pr = Pr {
            number: 12,
            title: "Add rate limits".into(),
            url: "https://example.com/pr/12".into(),
            head: "rate-limits".into(),
            base: "main".into(),
            sha,
        };
        // The template path: the built-in triage workflow, bound to these
        // settings, is what the case is made from.
        let opts = &super::triage::opts_of(&super::triage::bound(opts)).unwrap();
        let c = Case::new("triage-12", Some(pr), None, self.repo.clone(), opts, T0);
        case::store(&self.root, &c).unwrap();
        c
    }

    /// What an agent does in its checkout: a change, committed and pushed
    /// to `branch` on origin. Its commit.
    pub fn work(&mut self, c: &Case, role: Role, branch: &str, text: &str) -> String {
        let dir = self.repo.join(".trix/worktrees").join(match role {
            Role::Builder => format!("{}-build", c.id),
            _ => format!("{}-fix", c.id),
        });
        std::fs::write(dir.join("src/limit.rs"), text).unwrap();
        self.sh(&dir, &["add", "-A"]);
        let sha = self.commit(&dir, &format!("{} work", role.name()));
        self.sh(
            &dir,
            &["push", "--quiet", "origin", &format!("HEAD:{branch}")],
        );
        self.trace
            .push(format!("agent {} pushed {sha} to {branch}", role.name()));
        sha
    }

    /// A commit on origin's `branch` that nobody here made: the base moves.
    pub fn upstream(&mut self, branch: &str, file: &str, text: &str) -> String {
        let tmp = self.base.join("upstream");
        let (base, origin) = (self.base.clone(), self.origin.clone());
        if !tmp.is_dir() {
            self.sh(
                &base,
                &["clone", "--quiet", origin.to_str().unwrap(), "upstream"],
            );
        }
        self.sh(&tmp, &["fetch", "--quiet", "origin"]);
        self.sh(
            &tmp,
            &[
                "checkout",
                "--quiet",
                "-B",
                branch,
                &format!("origin/{branch}"),
            ],
        );
        std::fs::create_dir_all(tmp.join(file).parent().unwrap()).unwrap();
        std::fs::write(tmp.join(file), text).unwrap();
        self.sh(&tmp, &["add", "-A"]);
        let sha = self.commit(&tmp, "upstream work");
        self.sh(&tmp, &["push", "--quiet", "origin", branch]);
        sha
    }

    /// One look, as the case's pane does it: stored when it changed.
    pub fn tick(&mut self, c: &mut Case) {
        self.ticks += 1;
        self.trace
            .push(format!("tick {} @+{}", self.ticks, self.now - T0));
        let root = self.root.clone();
        let changed = step(c, &root, self);
        if changed {
            case::store(&root, c).unwrap();
        }
        let ask = c.ask.map_or("-".to_string(), |a| format!("{a:?}"));
        self.trace.push(format!(
            "  => {} stage={:?} round={} ask={ask}",
            if changed { "changed" } else { "same" },
            c.stage,
            c.round
        ));
    }

    pub fn ticks(&mut self, c: &mut Case, n: u32) {
        for _ in 0..n {
            self.tick(c);
        }
    }

    /// A report handed in by the role's own agent, through the server's check.
    pub fn submit(&mut self, c: &Case, role: Role, json: &str) -> Result<String, String> {
        let who = crate::control::Asker::Agent(c.agent_name(role));
        let r = case::handle(
            &self.root,
            Some(&who),
            &c.id,
            Op::Submit {
                role,
                report: json.into(),
            },
        );
        let said = match &r {
            Ok((out, _)) => format!("ok: {}", out.trim()),
            Err(e) => format!("refused: {}", e.lines().next().unwrap_or_default()),
        };
        self.trace.push(format!("submit {} -> {said}", role.name()));
        r.map(|(out, _)| out)
    }

    /// `trix case approve / abort / answer`, as the server takes it: the
    /// key it would type into the case's pane.
    pub fn op(&mut self, c: &Case, op: Op) {
        let r = case::handle(&self.root, None, &c.id, op.clone());
        match r {
            Ok((out, key)) => {
                self.trace
                    .push(format!("{op:?} -> ok: {} key={key:?}", out.trim()));
                self.keys.extend(key);
            }
            Err(e) => self.trace.push(format!("{op:?} -> refused: {e}")),
        }
    }

    pub fn key(&mut self, k: char) {
        self.trace.push(format!("you type {k}"));
        self.keys.push_back(k);
    }

    pub fn state(&mut self, name: &str, state: &str) {
        self.trace.push(format!("agent {name} is {state}"));
        self.seen
            .iter_mut()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("no agent {name}"))
            .state = state.into();
    }

    pub fn cost(&mut self, name: &str, cost: f64) {
        self.seen.iter_mut().find(|a| a.name == name).unwrap().cost = cost;
    }

    pub fn later(&mut self, secs: u64) {
        self.now += secs;
    }

    /// The case as the pane finds it after a restart: read from its folder.
    pub fn restore(&mut self, c: &mut Case) {
        self.trace
            .push("restore: the case is read from its folder".into());
        *c = case::load(&self.root, &c.id).unwrap();
    }

    pub fn note(&mut self, text: &str) {
        self.trace.push(format!("# {text}"));
    }

    /// gh for `resolve` and `open_case`, which take it as a function.
    pub fn gh_fn(&mut self) -> impl FnMut(&Path, &[&str]) -> Result<String, String> + '_ {
        move |r: &Path, a: &[&str]| self.gh(r, a)
    }

    fn norm(&self, s: &str) -> String {
        s.replace(self.base.to_str().unwrap(), "<tmp>")
    }

    fn pr_json(&mut self, p: &HubPr) -> serde_json::Value {
        let sha = self.head(&p.head);
        serde_json::json!({
            "number": p.number, "title": p.title, "url": format!("https://example.com/pr/{}", p.number),
            "headRefName": p.head, "headRefOid": sha, "baseRefName": p.base,
            "isCrossRepository": p.fork, "state": p.state,
        })
    }

    fn hub(&mut self, args: &[&str]) -> Result<String, String> {
        let joined = args.join(" ");
        if let Some((_, v)) = self.hub.answers.iter().find(|(k, _)| *k == joined) {
            return Ok(v.clone());
        }
        let num = |s: &str| s.parse::<u64>().unwrap_or(0);
        match args {
            ["api", path] if path.contains("/check-runs") => Ok(self.hub.ci.clone()),
            ["pr", "view", _, "--json", "mergeable,mergeStateStatus"] => {
                let m = if self.hub.mergeable.len() > 1 {
                    self.hub.mergeable.remove(0)
                } else {
                    self.hub.mergeable.first().copied().unwrap_or("MERGEABLE")
                };
                let state = match m {
                    "CONFLICTING" => "DIRTY",
                    "UNKNOWN" => "UNKNOWN",
                    _ => "CLEAN",
                };
                Ok(serde_json::json!({"mergeable": m, "mergeStateStatus": state}).to_string())
            }
            ["api", "graphql", ..] => self
                .hub
                .repo_info
                .clone()
                .ok_or_else(|| "unexpected".into()),
            ["pr", "view", _, "--json", "reviewDecision"] => {
                Ok(serde_json::json!({"reviewDecision": self.hub.review}).to_string())
            }
            ["pr", "view", n, "--json", "state,reviewDecision"] => {
                let state = self
                    .hub
                    .prs
                    .iter()
                    .find(|p| p.number == num(n))
                    .map_or("OPEN", |p| p.state);
                Ok(
                    serde_json::json!({"state": state, "reviewDecision": self.hub.review})
                        .to_string(),
                )
            }
            ["pr", "view", n, "--json", _] => {
                let p = self
                    .hub
                    .prs
                    .iter()
                    .find(|p| p.number == num(n))
                    .cloned()
                    .ok_or("no such PR")?;
                Ok(self.pr_json(&p).to_string())
            }
            ["pr", "merge", _, "--disable-auto"] => Ok(String::new()),
            ["pr", "merge", n, rest @ ..] => {
                if !self.hub.merge_errors.is_empty() {
                    return Err(self.hub.merge_errors.remove(0));
                }
                let p = self
                    .hub
                    .prs
                    .iter()
                    .find(|p| p.number == num(n))
                    .cloned()
                    .ok_or("no such PR")?;
                let want = rest
                    .iter()
                    .position(|a| *a == "--match-head-commit")
                    .map(|i| rest[i + 1]);
                if want != Some(self.head(&p.head).as_str()) {
                    return Err("Head branch was modified. Review and try the merge again.".into());
                }
                if !rest.contains(&"--auto") {
                    self.hub
                        .prs
                        .iter_mut()
                        .find(|p| p.number == num(n))
                        .unwrap()
                        .state = "MERGED";
                }
                Ok(String::new())
            }
            [
                "issue",
                "view",
                _,
                "--json",
                "closedByPullRequestsReferences",
            ] => {
                let refs: Vec<serde_json::Value> = self
                    .hub
                    .closes
                    .iter()
                    .map(|n| serde_json::json!({"number": n}))
                    .collect();
                Ok(serde_json::json!({"closedByPullRequestsReferences": refs}).to_string())
            }
            ["issue", "close", ..] => Ok(String::new()),
            [
                "pr",
                "create",
                "--head",
                head,
                "--base",
                base,
                "--title",
                title,
                ..,
            ] => {
                if let Some(i) = args.iter().position(|a| *a == "--body") {
                    self.hub
                        .pr_bodies
                        .push(args.get(i + 1).map(|b| b.to_string()).unwrap_or_default());
                }
                let n = self.hub.next_pr;
                self.hub.next_pr += 1;
                self.hub.prs.push(HubPr {
                    number: n,
                    title: title.to_string(),
                    head: head.to_string(),
                    base: base.to_string(),
                    state: "OPEN",
                    fork: false,
                });
                Ok(format!("https://example.com/pr/{n}\n"))
            }
            ["pr", "list", "--head", head, ..] => {
                let found: Vec<HubPr> = self
                    .hub
                    .prs
                    .iter()
                    .filter(|p| p.head == *head && p.state == "OPEN")
                    .cloned()
                    .collect();
                let v: Vec<serde_json::Value> = found.iter().map(|p| self.pr_json(p)).collect();
                Ok(serde_json::Value::Array(v).to_string())
            }
            _ => Err(format!("unexpected: gh {joined}")),
        }
    }

    /// The record, the case folder and the repository, as one text.
    pub fn snapshot(&mut self, c: &Case) -> String {
        let mut s = format!("# scenario: {}\n\n== trace ==\n", self.name);
        for line in &self.trace {
            s.push_str(&self.norm(line));
            s.push('\n');
        }
        s.push_str("\n== case folder ==\n");
        let dir = case::dir_of(&self.root, &c.id);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            s.push_str(&format!(
                "--- {}\n{}\n",
                f.file_name().unwrap().to_string_lossy(),
                self.norm(text.trim_end())
            ));
        }
        let (repo, origin) = (self.repo.clone(), self.origin.clone());
        s.push_str("\n== repository ==\n");
        let trees = self.sh(
            &repo,
            &[
                "-c",
                "core.quotePath=false",
                "worktree",
                "list",
                "--porcelain",
            ],
        );
        s.push_str(&self.norm(trees.trim_end()));
        s.push_str("\n-- refs\n");
        let refs = self.sh(
            &repo,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );
        s.push_str(refs.trim_end());
        s.push_str("\n\n== origin ==\n");
        let refs = self.sh(
            &origin,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );
        s.push_str(refs.trim_end());
        s.push('\n');
        s
    }

    /// Hold the run against its golden, then clean up.
    pub fn finish(mut self, c: &Case) {
        let got = self.snapshot(c);
        let _ = std::fs::remove_dir_all(&self.base);
        compare(&self.name, &got);
    }
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens/triage")
        .join(format!("{name}.golden"))
}

/// The golden for `name` says exactly `got`. A golden that isn't there yet
/// is written under `TRIX_BLESS=1`, and never overwritten.
pub fn compare(name: &str, got: &str) {
    compare_at(&golden_path(name), name, got)
}

/// The same, for a golden anywhere under `tests/goldens/`.
pub fn compare_in(dir: &str, name: &str, got: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(dir)
        .join(format!("{name}.golden"));
    compare_at(&path, name, got)
}

fn compare_at(path: &Path, name: &str, got: &str) {
    match std::fs::read_to_string(path) {
        Ok(want) if want == got => {}
        Ok(want) => {
            let line = want
                .lines()
                .zip(got.lines())
                .position(|(a, b)| a != b)
                .unwrap_or(want.lines().count().min(got.lines().count()));
            let show = |t: &str| {
                t.lines()
                    .skip(line.saturating_sub(3))
                    .take(8)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            panic!(
                "replay of '{name}' differs from its golden at line {}:\n--- golden\n{}\n--- now\n{}",
                line + 1,
                show(&want),
                show(got)
            );
        }
        Err(_) if std::env::var_os("TRIX_BLESS").is_some() => {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, got).unwrap();
        }
        Err(_) => panic!(
            "no golden for '{name}' at {}: goldens are recorded once, with TRIX_BLESS=1",
            path.display()
        ),
    }
}

/// `HH:MM  text`, as the pane prints a log line: the text, since the hour
/// depends on where the machine is.
fn untimed(line: &str) -> &str {
    let b = line.as_bytes();
    let timed = b.len() > 7
        && b[2] == b':'
        && b[..2].iter().chain(&b[3..5]).all(u8::is_ascii_digit)
        && &line[5..7] == "  ";
    if timed { &line[7..] } else { line }
}

impl Env for World {
    fn now(&self) -> u64 {
        self.now
    }
    fn git(&mut self, repo: &Path, args: &[&str]) -> Result<String, String> {
        let mut c = git_cmd(repo, &self.base, T0 + self.commits as u64);
        c.args(args);
        let r = output(c);
        let said = match &r {
            Ok(out) if out.trim().lines().count() == 1 && out.trim().len() <= 80 => {
                format!("ok: {}", out.trim())
            }
            Ok(_) => "ok".into(),
            // What git says of a failure differs between its versions.
            Err(_) => "err".into(),
        };
        let at = repo
            .strip_prefix(&self.base)
            .unwrap_or(repo)
            .display()
            .to_string();
        self.trace
            .push(format!("  git [{at}] {} -> {said}", args.join(" ")));
        r
    }
    fn gh(&mut self, _repo: &Path, args: &[&str]) -> Result<String, String> {
        let r = self.hub(args);
        let said = match &r {
            Ok(_) => "ok".to_string(),
            Err(e) => format!("err: {}", e.lines().next().unwrap_or_default()),
        };
        self.trace.push(format!(
            "  gh {} -> {said}",
            args.join(" ").replace('\n', "\\n")
        ));
        r
    }
    fn spawn(
        &mut self,
        name: &str,
        agent: &RoleAgent,
        cwd: &Path,
        prompt: &str,
    ) -> Result<(), String> {
        let refused = self.refuse.iter().any(|n| n == name);
        self.trace.push(format!(
            "  spawn {name} agent={} args={:?} cwd={}{}",
            agent.agent,
            agent.args,
            cwd.display(),
            if refused { " -> err" } else { "" }
        ));
        if refused {
            return Err(format!("{} isn't installed", agent.agent));
        }
        for l in prompt.lines() {
            self.trace.push(format!("  | {l}"));
        }
        self.seen.push(Seen {
            name: name.into(),
            state: "working".into(),
            cost: 1.0,
        });
        self.spawned.push(Spawned {
            name: name.into(),
            kind: agent.agent.clone(),
            args: agent.args.clone(),
            cwd: cwd.to_path_buf(),
            prompt: prompt.into(),
            guarded: std::mem::take(&mut self.guard_next),
        });
        Ok(())
    }
    fn open_case(&mut self, c: &Case) -> Result<(), String> {
        self.trace.push(format!(
            "  open_case {} stage={:?} branch={}",
            c.id,
            c.stage,
            c.issue.as_ref().map_or("-", |i| &i.branch)
        ));
        case::store(&self.root, c)
    }
    fn send(&mut self, name: &str, text: &str) -> Result<(), String> {
        self.trace.push(format!("  send {name}"));
        for l in text.lines() {
            self.trace.push(format!("  | {l}"));
        }
        self.sent.push((name.into(), text.into()));
        if let Some(a) = self.seen.iter_mut().find(|a| a.name == name) {
            a.state = "working".into();
        }
        Ok(())
    }
    fn close(&mut self, name: &str, clean: bool) -> Result<(), String> {
        self.trace.push(format!("  close {name} clean={clean}"));
        self.seen.retain(|a| a.name != name);
        Ok(())
    }
    fn agents(&mut self) -> Result<Vec<Seen>, String> {
        self.trace.push("  agents".into());
        Ok(self.seen.clone())
    }
    fn answer(&mut self) -> Option<char> {
        let k = self.keys.pop_front();
        self.trace.push(format!("  answer -> {k:?}"));
        k
    }
    fn say(&mut self, event: &str, detail: &str) {
        self.trace.push(format!("  say {event}: {detail}"));
    }
    fn print(&mut self, line: &str) {
        self.trace.push(format!("  print {}", untimed(line)));
    }
    fn spawn_in(
        &mut self,
        name: &str,
        agent: &RoleAgent,
        cwd: &Path,
        prompt: &str,
        guard: bool,
    ) -> Result<(), String> {
        self.guard_next = guard;
        self.spawn(name, agent, cwd, prompt)
    }
    /// A check, run for real: a process of its own in `cwd`, trix's
    /// variables removed, the guard on when asked, and `<base>/bin` first
    /// on PATH so a scenario can plant a `gh` or a `trix` for it to find.
    fn check(
        &mut self,
        cwd: &Path,
        argv: &[String],
        guard: bool,
    ) -> Result<super::env::CheckOut, String> {
        self.checks.push((cwd.to_path_buf(), argv.to_vec(), guard));
        let Some((program, rest)) = argv.split_first() else {
            return Err("no command".into());
        };
        let mut c = std::process::Command::new(program);
        c.current_dir(cwd)
            .args(rest)
            .stdin(std::process::Stdio::null());
        c.env("HOME", &self.base)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c.env("GIT_AUTHOR_NAME", "Check")
            .env("GIT_AUTHOR_EMAIL", "check@example.com");
        c.env("GIT_COMMITTER_NAME", "Check")
            .env("GIT_COMMITTER_EMAIL", "check@example.com");
        c.env(
            "PATH",
            format!(
                "{}:{}",
                self.base.join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        for k in super::guard::TRIX_VARS {
            c.env_remove(k);
        }
        if guard {
            let empty = self.base.join("guard-empty");
            let _ = std::fs::create_dir_all(&empty);
            super::guard::apply(&mut c, &empty);
        }
        let o = c.output().map_err(|e| format!("{program}: {e}"))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        Ok(super::env::CheckOut {
            exit_code: o.status.code().unwrap_or(-1),
            log_tail: self.norm(&super::env::tail(&text)),
        })
    }
    fn screen(&mut self, name: &str) -> Option<String> {
        self.screens.get(name).cloned()
    }
    fn notify(&mut self, title: &str, body: &str) {
        self.notified.push((title.into(), body.into()));
    }
    fn open_run(&mut self, id: &str, _repo: &Path) -> Result<(), String> {
        self.opened.push(id.into());
        Ok(())
    }
}

// ───────────────────────────── the scenarios ─────────────────────────────

const FINDINGS: &str = r#"{"findings": [{"id": "F1", "file": "src/limit.rs", "line": 1, "summary": "the window never resets", "failure": "after 60s a client is still refused", "proof": {"test": "limit_resets_after_60s", "observed": "refused at t=61", "expected": "allowed at t=61"}}]}"#;
const CONFIRMED: &str = r#"{"findings": [{"id": "F1", "confirmed": true, "evidence": "limit_resets_after_60s fails: refused at t=61"}]}"#;
const NOTHING: &str = r#"{"findings": []}"#;
const R2: &str = r#"{"findings": [{"id": "F1", "file": "./src/limit.rs", "line": 2, "summary": "reset compares the wrong clock", "failure": "never resets", "proof": {"repro": "cargo run -- --burst 61", "observed": "61st call refused after 70s", "expected": "allowed"}}, {"id": "F2", "file": "src/api.rs", "line": 9, "summary": "a refused call returns 200", "failure": "clients can't tell", "proof": {"repro": "curl -i /api after 60 calls", "observed": "HTTP/1.1 200", "expected": "HTTP/1.1 429"}}], "optional": [{"file": "src/api.rs", "summary": "might race under load", "unproven": true}]}"#;
const R2_OK: &str = r#"{"findings": [{"id": "F1", "confirmed": true, "evidence": "refused after 70s again"}, {"id": "F2", "confirmed": false, "why": "the handler does return 429; I misread the log"}]}"#;

fn one() -> TriageOpts {
    TriageOpts {
        reviewers: 1,
        ..TriageOpts::default()
    }
}

fn fixed(sha: &str, ids: &[&str]) -> String {
    let each: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| {
            let proof = serde_json::json!({"ran": "limit_resets_after_60s", "output": "ok", "passes": true});
            if id.starts_with('F') {
                serde_json::json!({"id": id, "outcome": "fixed", "test": "limit_resets_after_60s", "proof": proof})
            } else {
                serde_json::json!({"id": id, "outcome": "fixed"})
            }
        })
        .collect();
    serde_json::json!({"commits": [sha], "tests": "cargo test: 12 passed", "findings": each})
        .to_string()
}

fn verdict(pass: bool, ok: &[&str], not_ok: &[&str]) -> String {
    let proof = serde_json::json!({"repro": "cargo test limit", "observed": "refused at t=61", "expected": "allowed"});
    let mut f: Vec<serde_json::Value> = ok
        .iter()
        .map(|id| serde_json::json!({"id": id, "ok": true}))
        .collect();
    f.extend(not_ok.iter().map(|id| serde_json::json!({"id": id, "ok": false, "reason": "still refused at t=61", "proof": proof})));
    serde_json::json!({"verdict": if pass { "pass" } else { "changes" }, "findings": f, "new": [], "optional": [], "ci": "green"}).to_string()
}

/// From a fresh case to the fixer at work on F1: the review, its check,
/// and the reviewer's pane closed.
fn to_fixing(w: &mut World, c: &mut Case) {
    w.tick(c);
    w.submit(c, Role::Reviewer, FINDINGS).unwrap();
    w.tick(c);
    w.submit(c, Role::Reviewer, CONFIRMED).unwrap();
    w.tick(c);
    assert_eq!(c.stage, Stage::Fixing);
    w.state("triage-12-review", "done");
    w.tick(c);
    assert_eq!(c.w.agent.as_deref(), Some("triage-12-fix"));
}

/// The fixer's push and report, then the gatekeeper started on it.
fn fix_round(w: &mut World, c: &mut Case, text: &str, ids: &[&str]) {
    let sha = w.work(c, Role::Fixer, "rate-limits", text);
    w.submit(c, Role::Fixer, &fixed(&sha, ids)).unwrap();
    w.tick(c);
    assert_eq!(c.stage, Stage::Verifying);
    w.tick(c);
}

/// From a fresh case to the merge gate asking for your y.
fn to_gate(w: &mut World, c: &mut Case) {
    to_fixing(w, c);
    fix_round(w, c, "pub fn allowed() -> bool { reset() }\n", &["F1"]);
    w.submit(c, Role::Gatekeeper, &verdict(true, &["F1"], &[]))
        .unwrap();
    w.tick(c);
    assert_eq!(c.stage, Stage::Gate);
    w.tick(c);
    assert_eq!(c.ask, Some(Ask::Merge), "{}", c.why);
}

#[test]
fn replay_a_pr_through_two_rounds_to_a_merge() {
    let mut w = World::new("two-rounds");
    let mut c = w.case(&one());
    to_fixing(&mut w, &mut c);
    fix_round(
        &mut w,
        &mut c,
        "pub fn allowed() -> bool { reset() }\n",
        &["F1"],
    );
    w.submit(&c, Role::Gatekeeper, &verdict(false, &[], &["F1"]))
        .unwrap();
    w.tick(&mut c);
    assert_eq!((c.stage, c.round), (Stage::Fixing, 2));
    w.state("triage-12-gate-1", "done");
    w.tick(&mut c);
    fix_round(
        &mut w,
        &mut c,
        "pub fn allowed() -> bool { reset_at(60) }\n",
        &["F1"],
    );
    w.submit(&c, Role::Gatekeeper, &verdict(true, &["F1"], &[]))
        .unwrap();
    w.tick(&mut c);
    w.note("CI is still running, then green");
    w.hub.ci = runs(&[("test", "in_progress", None)]);
    w.tick(&mut c);
    w.hub.ci = green();
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::Merge));
    w.op(&c, Op::Approve);
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Merged);
    w.tick(&mut c);
    w.finish(&c);
}

#[test]
fn replay_two_reviewers_side_by_side() {
    let mut w = World::new("two-reviewers");
    let mut c = w.case(&TriageOpts::default());
    w.tick(&mut c);
    w.submit(&c, Role::Reviewer, FINDINGS).unwrap();
    w.submit(&c, Role::Reviewer2, R2).unwrap();
    w.tick(&mut c);
    w.submit(&c, Role::Reviewer2, R2_OK).unwrap();
    w.tick(&mut c);
    assert_eq!(
        c.stage,
        Stage::Reviewing,
        "waits for the first reviewer's check"
    );
    w.submit(&c, Role::Reviewer, CONFIRMED).unwrap();
    w.tick(&mut c);
    assert_eq!((c.stage, c.findings.len()), (Stage::Fixing, 1));
    w.state("triage-12-review", "done");
    w.state("triage-12-review-2", "idle");
    w.tick(&mut c);
    fix_round(
        &mut w,
        &mut c,
        "pub fn allowed() -> bool { reset() }\n",
        &["F1"],
    );
    w.submit(&c, Role::Gatekeeper, &verdict(true, &["F1"], &[]))
        .unwrap();
    w.ticks(&mut c, 2);
    w.key('y');
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Merged);
    w.finish(&c);
}

#[test]
fn replay_nothing_found_then_the_gate() {
    let mut w = World::new("nothing-found");
    let mut c = w.case(&one());
    w.tick(&mut c);
    w.submit(&c, Role::Reviewer, NOTHING).unwrap();
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::NothingFound));
    w.op(&c, Op::Answer { key: 'y' });
    w.op(&c, Op::Answer { key: 'm' });
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Gate);
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::Merge));
    w.key('y');
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Merged);
    w.finish(&c);
}

#[test]
fn replay_the_round_limit_asks_for_one_more() {
    let mut w = World::new("round-limit");
    let mut c = w.case(&TriageOpts {
        max_rounds: 1,
        ..one()
    });
    to_fixing(&mut w, &mut c);
    fix_round(
        &mut w,
        &mut c,
        "pub fn allowed() -> bool { reset() }\n",
        &["F1"],
    );
    w.submit(&c, Role::Gatekeeper, &verdict(false, &[], &["F1"]))
        .unwrap();
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::RoundLimit));
    w.key('x');
    w.tick(&mut c);
    w.key('r');
    w.tick(&mut c);
    assert_eq!((c.stage, c.round, c.max_rounds), (Stage::Fixing, 2, 2));
    fix_round(
        &mut w,
        &mut c,
        "pub fn allowed() -> bool { reset_at(60) }\n",
        &["F1"],
    );
    w.submit(&c, Role::Gatekeeper, &verdict(true, &["F1"], &[]))
        .unwrap();
    w.ticks(&mut c, 2);
    assert_eq!(c.ask, Some(Ask::Merge));
    w.key('a');
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Aborted);
    w.finish(&c);
}

#[test]
fn replay_red_ci_at_the_gate() {
    let mut w = World::new("ci-failed");
    let mut c = w.case(&one());
    to_fixing(&mut w, &mut c);
    fix_round(
        &mut w,
        &mut c,
        "pub fn allowed() -> bool { reset() }\n",
        &["F1"],
    );
    w.submit(&c, Role::Gatekeeper, &verdict(true, &["F1"], &[]))
        .unwrap();
    w.tick(&mut c);
    w.note("CI fails with a round left: a finding for the fixer");
    w.hub.ci = runs(&[("test", "completed", Some("failure"))]);
    w.tick(&mut c);
    assert_eq!((c.stage, c.round), (Stage::Fixing, 2));
    fix_round(
        &mut w,
        &mut c,
        "pub fn allowed() -> bool { reset_at(60) }\n",
        &["CI1"],
    );
    w.submit(&c, Role::Gatekeeper, &verdict(true, &["CI1"], &[]))
        .unwrap();
    w.tick(&mut c);
    w.note("and again, with the rounds used up: it asks");
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::CiFailed));
    w.key('r');
    w.tick(&mut c);
    assert_eq!((c.stage, c.round), (Stage::Fixing, 3));
    w.finish(&c);
}

#[test]
fn replay_the_budget_and_an_agent_without_its_report() {
    let mut w = World::new("budget-no-report");
    let mut c = w.case(&one());
    w.tick(&mut c);
    w.note("the reviewer stops without its report: reminded once, then you're asked");
    w.state("triage-12-review", "working");
    w.tick(&mut c);
    w.state("triage-12-review", "idle");
    w.tick(&mut c);
    w.later(20);
    w.tick(&mut c);
    w.state("triage-12-review", "working");
    w.tick(&mut c);
    w.state("triage-12-review", "done");
    w.tick(&mut c);
    w.later(20);
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::NoReport));
    w.key('n');
    w.tick(&mut c);
    w.note("it exits: asked again, and n starts it again");
    w.state("triage-12-review", "exited");
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::NoReport));
    w.key('n');
    w.tick(&mut c);
    w.tick(&mut c);
    assert_eq!(c.reviews[0].w.agent.as_deref(), Some("triage-12-review"));
    w.note("the budget is spent");
    w.cost("triage-12-review", 11.0);
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::Budget));
    w.key('b');
    w.tick(&mut c);
    assert_eq!(c.budget, 20.0);
    w.submit(&c, Role::Reviewer, NOTHING).unwrap();
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::NothingFound));
    w.op(&c, Op::Abort);
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Aborted);
    w.finish(&c);
}

#[test]
fn replay_a_failed_merge_and_a_base_that_moved() {
    let mut w = World::new("merge-failed");
    let mut c = w.case(&one());
    to_gate(&mut w, &mut c);
    w.hub.merge_errors = vec![
        "GraphQL: Something went wrong".into(),
        "GraphQL: Base branch was modified. Review and try the merge again.".into(),
    ];
    w.key('y');
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::MergeFailed));
    w.key('r');
    w.tick(&mut c);
    assert!(c.merge_when_ready);
    w.note("GitHub is still working out whether it merges");
    w.hub.mergeable = vec!["UNKNOWN", "MERGEABLE"];
    w.tick(&mut c);
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Merged);
    w.finish(&c);
}

#[test]
fn replay_a_conflict_with_the_base() {
    let mut w = World::new("conflict");
    let mut c = w.case(&TriageOpts {
        max_rounds: 1,
        ..one()
    });
    to_gate(&mut w, &mut c);
    w.note("main moves, and the PR conflicts: no rounds left, so it asks");
    w.upstream(
        "main",
        "src/limit.rs",
        "pub fn allowed() -> bool { quota() }\n",
    );
    w.hub.mergeable = vec!["CONFLICTING"];
    w.key('y');
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::Conflict), "{}", c.why);
    w.key('r');
    w.tick(&mut c);
    assert_eq!((c.stage, c.round), (Stage::Fixing, 2));
    w.hub.mergeable = vec!["MERGEABLE"];
    fix_round(
        &mut w,
        &mut c,
        "pub fn allowed() -> bool { reset() && quota() }\n",
        &["CONFLICT1"],
    );
    w.submit(&c, Role::Gatekeeper, &verdict(true, &["CONFLICT1"], &[]))
        .unwrap();
    w.ticks(&mut c, 2);
    assert_eq!(c.ask, Some(Ask::Merge));
    w.finish(&c);
}

#[test]
fn replay_an_agent_that_cant_start() {
    let mut w = World::new("setup");
    let mut c = w.case(&one());
    w.refuse.push("triage-12-review".into());
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::Setup));
    w.tick(&mut c);
    w.refuse.clear();
    w.key('r');
    w.tick(&mut c);
    w.tick(&mut c);
    assert_eq!(c.reviews[0].w.agent.as_deref(), Some("triage-12-review"));
    w.finish(&c);
}

#[test]
fn replay_branch_rules_auto_merge_and_admin() {
    let mut w = World::new("approval");
    let mut c = w.case(&one());
    to_gate(&mut w, &mut c);
    w.hub.merge_errors = vec!["X Pull request acme/app#12 is not mergeable: the base branch policy prohibits the merge.\nTo have the pull request merged after all the requirements have been met, add the `--auto` flag.\nTo use administrator privileges to immediately merge the pull request, add the `--admin` flag.".into()];
    w.hub.repo_info = Some(
        r#"{"data": {"repository": {"viewerPermission": "ADMIN", "autoMergeAllowed": true}}}"#
            .into(),
    );
    w.hub.review = "REVIEW_REQUIRED";
    w.key('y');
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::Approval), "{}", c.why);
    assert_eq!(
        c.keys(Ask::Approval)
            .iter()
            .map(|k| k.0)
            .collect::<String>(),
        "rwma"
    );
    w.note("w: auto-merge, then someone approves and GitHub merges it");
    w.key('w');
    w.tick(&mut c);
    assert!(c.blocked.as_ref().is_some_and(|b| b.auto_on));
    w.tick(&mut c);
    w.later(61);
    w.hub.review = "APPROVED";
    w.tick(&mut c);
    w.later(61);
    w.hub.prs[0].state = "MERGED";
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Merged);
    w.finish(&c);
}

#[test]
fn replay_a_restart_mid_case() {
    let mut w = World::new("restore");
    let mut c = w.case(&TriageOpts::default());
    w.tick(&mut c);
    w.submit(&c, Role::Reviewer, FINDINGS).unwrap();
    w.tick(&mut c);
    w.restore(&mut c);
    w.tick(&mut c);
    w.submit(&c, Role::Reviewer, CONFIRMED).unwrap();
    w.submit(&c, Role::Reviewer2, NOTHING).unwrap();
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Fixing);
    w.state("triage-12-review", "done");
    w.state("triage-12-review-2", "done");
    w.tick(&mut c);
    let sha = w.work(
        &c,
        Role::Fixer,
        "rate-limits",
        "pub fn allowed() -> bool { reset() }\n",
    );
    w.restore(&mut c);
    w.submit(&c, Role::Fixer, &fixed(&sha, &["F1"])).unwrap();
    w.tick(&mut c);
    w.tick(&mut c);
    w.restore(&mut c);
    w.tick(&mut c);
    assert_eq!(c.w.agent.as_deref(), Some("triage-12-gate-1"));
    w.finish(&c);
}

fn issue_answers(
    n: u64,
    association: &str,
    links: &[u64],
    comment_by: &str,
) -> Vec<(String, String)> {
    let refs: Vec<serde_json::Value> = links
        .iter()
        .map(|l| serde_json::json!({"number": l, "url": "u", "repository": {"name": "app", "owner": {"login": "acme"}}}))
        .collect();
    vec![
        ("api repos/{owner}/{repo}".into(), r#"{"full_name": "acme/app", "default_branch": "main"}"#.into()),
        (
            format!("api repos/{{owner}}/{{repo}}/issues/{n}"),
            serde_json::json!({"number": n, "title": "Add rate limits!", "state": "open", "html_url": "https://github.com/acme/app/issues/31", "body": "Clients can call /api as often as they like.", "user": {"login": "ann"}, "author_association": association}).to_string(),
        ),
        (
            format!("api repos/{{owner}}/{{repo}}/issues/{n}/comments?per_page=100"),
            serde_json::json!([{"body": "me too", "user": {"login": "bob"}, "author_association": comment_by}]).to_string(),
        ),
        (
            format!("issue view {n} --json closedByPullRequestsReferences"),
            serde_json::json!({"closedByPullRequestsReferences": refs}).to_string(),
        ),
        (format!("pr list --state open --search {n} --json number,title,body --limit 100"), "[]".into()),
        ("pr list --state all --limit 100 --json headRefName".into(), "[]".into()),
        (format!("api repos/{{owner}}/{{repo}}/issues/{n}/timeline?per_page=100"), "[]".into()),
    ]
}

/// `trix triage <input>`, up to the stored case.
fn start(w: &mut World, input: &str, opts: &TriageOpts) -> Result<Case, String> {
    let (root, repo) = (w.root.clone(), w.repo.clone());
    w.note(&format!("trix triage {input}"));
    let made = {
        let opts = &super::triage::opts_of(&super::triage::bound(opts)).unwrap();
        let mut gh = w.gh_fn();
        case::resolve(&repo, input, &mut gh)
            .and_then(|t| case::open_case(&root, &repo, t, opts, T0, &mut gh))
    };
    match &made {
        Ok(c) => {
            case::store(&root, c).unwrap();
            w.note(&format!("case {} for {}: {:?}", c.id, c.subject(), c.stage));
        }
        Err(e) => w.note(&format!("refused: {e}")),
    }
    made
}

#[test]
fn replay_an_issue_with_several_prs_and_a_fork() {
    let mut w = World::new("choose");
    w.note("a second PR for the issue, on a branch of its own");
    let (repo, sha) = (w.repo.clone(), w.head("rate-limits"));
    w.sh(
        &repo,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("{sha}:refs/heads/limits-2"),
        ],
    );
    w.hub.prs.push(HubPr {
        number: 14,
        title: "Limits, another way".into(),
        head: "limits-2".into(),
        base: "main".into(),
        state: "OPEN",
        fork: false,
    });
    w.hub.prs.push(HubPr {
        number: 15,
        title: "From a fork".into(),
        head: "rate-limits".into(),
        base: "main".into(),
        state: "OPEN",
        fork: true,
    });
    w.hub.answers = issue_answers(31, "OWNER", &[12, 14, 15], "MEMBER");
    w.hub.ci = runs(&[]);
    let mut c = start(&mut w, "31", &one()).unwrap();
    assert_eq!((c.stage, c.choices.len()), (Stage::Choosing, 2));
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::Choose));
    w.op(&c, Op::Answer { key: '7' });
    w.op(&c, Op::Answer { key: '2' });
    w.tick(&mut c);
    assert_eq!(
        (c.stage, c.pr().number, c.ci_expected),
        (Stage::Reviewing, 14, false)
    );
    w.tick(&mut c);
    w.note("a PR from a fork is refused, and so is an issue whose only PR is one");
    assert!(start(&mut w, "15", &one()).is_err());
    w.hub.answers = issue_answers(32, "OWNER", &[15], "MEMBER");
    assert!(start(&mut w, "https://github.com/acme/app/issues/32", &one()).is_err());
    w.finish(&c);
}

#[test]
fn replay_an_issue_built_from_nothing() {
    let mut w = World::new("build");
    w.hub.answers = issue_answers(31, "NONE", &[], "MEMBER");
    let mut c = start(&mut w, "#31", &one()).unwrap();
    assert_eq!(c.stage, Stage::Building);
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::Outsider), "{}", c.why);
    w.key('s');
    w.tick(&mut c);
    w.tick(&mut c);
    assert_eq!(c.w.agent.as_deref(), Some("triage-issue-31-build"));
    let branch = c.issue.as_ref().unwrap().branch.clone();
    w.note("a report whose commits aren't pushed goes back to the builder");
    let unpushed = serde_json::json!({"branch": branch, "commits": ["abcdef1"], "tests": "cargo test: ok", "summary": "Adds a per-client limit."}).to_string();
    w.submit(&c, Role::Builder, &unpushed).unwrap();
    w.tick(&mut c);
    let sha = w.work(
        &c,
        Role::Builder,
        &branch,
        "pub fn allowed() -> bool { limit() }\n",
    );
    let built = serde_json::json!({"branch": branch, "commits": [sha], "tests": "cargo test: ok", "summary": "Adds a per-client limit."}).to_string();
    w.submit(&c, Role::Builder, &built).unwrap();
    w.tick(&mut c);
    assert_eq!((c.stage, c.pr().number), (Stage::Reviewing, 20));
    w.state("triage-issue-31-build", "done");
    w.tick(&mut c);
    w.note("the reviewer says the PR doesn't solve the issue: f fixes it in place");
    let no = r#"{"findings": [{"id": "F1", "file": "src/limit.rs", "line": 1, "summary": "no limit per client", "failure": "every client shares one", "proof": {"repro": "two clients, 61 calls", "observed": "the second is refused", "expected": "each has its own"}}], "solves": {"answer": "no", "missing": "a limit per client", "evidence": "ran two clients"}}"#;
    w.submit(&c, Role::Reviewer, no).unwrap();
    w.tick(&mut c);
    let sure = r#"{"findings": [{"id": "F1", "confirmed": true, "evidence": "the second client is refused"}], "solves": {"answer": "no", "missing": "a limit per client", "evidence": "ran two clients again"}}"#;
    w.submit(&c, Role::Reviewer, sure).unwrap();
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::Unsolved), "{}", c.why);
    w.key('f');
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Fixing);
    w.state("triage-issue-31-review", "done");
    w.tick(&mut c);
    let sha = w.work(
        &c,
        Role::Fixer,
        &branch,
        "pub fn allowed(client: u32) -> bool { limit(client) }\n",
    );
    w.submit(&c, Role::Fixer, &fixed(&sha, &["F1"])).unwrap();
    w.ticks(&mut c, 2);
    let pass = r#"{"verdict": "pass", "findings": [{"id": "F1", "ok": true}], "new": [], "optional": [], "ci": "green", "solves": {"answer": "yes", "evidence": "two clients, each its own limit"}}"#;
    w.submit(&c, Role::Gatekeeper, pass).unwrap();
    w.ticks(&mut c, 2);
    assert_eq!(c.ask, Some(Ask::Merge), "{}", c.why);
    w.key('y');
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Merged);
    w.finish(&c);
}

#[test]
fn replay_an_unsolved_issue_handed_to_a_fresh_case() {
    let mut w = World::new("rebuild");
    w.hub.answers = issue_answers(31, "OWNER", &[12], "MEMBER");
    let mut c = start(&mut w, "31", &one()).unwrap();
    assert_eq!((c.stage, c.pr().number), (Stage::Reviewing, 12));
    w.tick(&mut c);
    let no = r#"{"findings": [{"id": "F1", "file": "src/limit.rs", "line": 1, "summary": "no limit per client", "failure": "every client shares one", "proof": {"repro": "two clients, 61 calls", "observed": "the second is refused", "expected": "each has its own"}}], "solves": {"answer": "no", "missing": "a limit per client", "evidence": "ran two clients"}}"#;
    w.submit(&c, Role::Reviewer, no).unwrap();
    w.tick(&mut c);
    let sure = r#"{"findings": [{"id": "F1", "confirmed": true, "evidence": "the second client is refused"}], "solves": {"answer": "no", "missing": "a limit per client", "evidence": "ran two clients again"}}"#;
    w.submit(&c, Role::Reviewer, sure).unwrap();
    w.tick(&mut c);
    assert_eq!(c.ask, Some(Ask::Unsolved));
    w.key('b');
    w.tick(&mut c);
    assert_eq!(c.stage, Stage::Aborted);
    w.finish(&c);
}
