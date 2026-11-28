//! The one checker. The builder runs it on every edit; `trix workflow
//! check`, import, dry run, rehearsal and run start call the same function,
//! so they never disagree. It holds what each node's role needs against
//! what its backend and profile provide (the capability matrix), and the
//! graph against the rules every run relies on, and says what to do about
//! each problem.

use super::caps::{Installed, Level, Matrix, Profile, Protection, shipped_profiles};
use super::model::{
    Allow, Brief, Count, DONE, EVENTS, FAIL, Merge, Node, Push, Revisit, Role, Workflow, Workspace,
    duration,
};
use std::collections::{BTreeMap, BTreeSet};

/// What the checker knows about this machine and your config.
#[derive(Clone, Debug, PartialEq)]
pub struct Ctx {
    pub installed: Installed,
    /// Codex's hooks were trusted (`/hooks`): without it, state is notify only.
    pub codex_hooks_trusted: bool,
    /// `[workflows] ci_paths`: more paths that count as CI configuration.
    pub ci_paths: Vec<String>,
    /// `[cost] confirm_over`. Unset: the workflow's budget, or $10.
    pub confirm_over: Option<f64>,
    /// `[approvals] allow`: the workflows whose approvals are switched on.
    pub approvals_allow: Vec<String>,
    /// Whether the file is trusted as it is now. None: not asked.
    pub trusted: Option<bool>,
}

impl Ctx {
    /// A machine with every backend installed in range and Codex's hooks
    /// trusted: how a workflow checks for its author.
    pub fn ideal(m: &Matrix) -> Ctx {
        Ctx {
            installed: Installed::all(m),
            codex_hooks_trusted: true,
            ci_paths: Vec::new(),
            confirm_over: None,
            approvals_allow: Vec::new(),
            trusted: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// The workflow can't run.
    Error,
    /// It isn't trusted as it is: it can be edited and checked, not run.
    Blocks,
    Warning,
    Info,
}

impl Severity {
    pub fn mark(self) -> &'static str {
        match self {
            Severity::Error | Severity::Blocks => "✗",
            Severity::Warning => "⚠",
            Severity::Info => "ℹ",
        }
    }
}

/// Where a problem is, so the builder can draw it there.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum At {
    Workflow,
    Node(String),
    Edge(usize),
    Rule(usize),
    Approvals,
}

/// A change that would fix a problem. `Manual` ones only say what to do.
#[derive(Clone, Debug, PartialEq)]
pub enum FixOp {
    SetAgent {
        node: String,
        kind: String,
    },
    SetProfile {
        node: String,
        profile: String,
    },
    SetPush(Push),
    SetApprover(Option<String>),
    RemoveRule(usize),
    RuleApproverRule(usize),
    SetBrief {
        node: String,
        brief: Brief,
    },
    SetRevisit {
        node: String,
        revisit: Revisit,
    },
    SetMaxVisits {
        node: String,
        n: u32,
    },
    SetWallClock(String),
    SetMaxIterations(u32),
    AddLand,
    AddEdge {
        from: String,
        to: String,
        on: String,
    },
    RemoveNode(String),
    /// Run the probe for a kind: `trix workflow capabilities --verify`.
    Probe(String),
    Manual,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fix {
    pub label: String,
    pub op: FixOp,
}

fn fix(label: impl Into<String>, op: FixOp) -> Fix {
    Fix {
        label: label.into(),
        op,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub level: Severity,
    /// A short stable name for the rule: `codex-read-only`, `push-guarded`.
    pub code: &'static str,
    pub at: At,
    pub text: String,
    pub fixes: Vec<Fix>,
}

/// What the matrix makes of one agent node: its badges.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub kind: String,
    pub level: Level,
    pub hands_in: bool,
    pub protection: Protection,
    /// For a read-only role: whether that's enforced, or by instruction.
    pub read_only: Option<bool>,
    pub decision: Level,
    pub priced: bool,
}

/// What a run would cost, as far as that can be known before it starts.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Estimate {
    /// The most agents at work at once.
    pub peak_parallel: u32,
    /// The fewest cycles that reach done_when: 1 for a linear flow.
    pub min_cycles: u32,
    pub ceiling: u32,
    /// For priced kinds only.
    pub usd: f64,
    /// Some node runs on a kind trix can't price: the total is unknown.
    pub unknown: bool,
}

impl Estimate {
    /// `~$1.50`, `~$1.50 + ?`, `?`.
    pub fn cost(&self) -> String {
        match (self.usd > 0.0, self.unknown) {
            (true, true) => format!("~{} + ?", crate::cost::usd(self.usd)),
            (true, false) => format!("~{}", crate::cost::usd(self.usd)),
            _ => "?".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub name: String,
    pub problems: Vec<Problem>,
    pub labels: BTreeMap<String, Label>,
    pub estimate: Estimate,
}

impl Report {
    pub fn count(&self, level: Severity) -> usize {
        self.problems.iter().filter(|p| p.level == level).count()
    }
    pub fn errors(&self) -> usize {
        self.count(Severity::Error)
    }
    pub fn warnings(&self) -> usize {
        self.count(Severity::Warning)
    }
    /// It may run: no error, and nothing blocks it.
    pub fn runnable(&self) -> bool {
        self.errors() == 0 && self.count(Severity::Blocks) == 0
    }
    pub fn at(&self, at: &At) -> impl Iterator<Item = &Problem> {
        let at = at.clone();
        self.problems.iter().filter(move |p| p.at == at)
    }
    pub fn has(&self, code: &str) -> bool {
        self.problems.iter().any(|p| p.code == code)
    }
    pub fn find(&self, code: &str) -> Option<&Problem> {
        self.problems.iter().find(|p| p.code == code)
    }

    /// The result as text: the same wherever it's shown.
    pub fn render(&self) -> String {
        let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        let mut s = format!(
            "{}: {}, {}\n",
            self.name,
            plural(self.errors(), "error"),
            plural(self.warnings(), "warning")
        );
        for p in &self.problems {
            let at = match &p.at {
                At::Node(id) => format!("{id}: "),
                At::Edge(i) => format!("edge {}: ", i + 1),
                At::Rule(i) => format!("permission rule {}: ", i + 1),
                At::Approvals => "approvals: ".into(),
                At::Workflow => String::new(),
            };
            s.push_str(&format!("{} {at}{}\n", p.level.mark(), p.text));
            for f in &p.fixes {
                s.push_str(&format!("    fix: {}\n", f.label));
            }
        }
        s.push_str(&format!(
            "peak {} · cost {} · cycles {} to {}\n",
            plural(self.estimate.peak_parallel as usize, "agent"),
            self.estimate.cost(),
            self.estimate.min_cycles,
            self.estimate.ceiling
        ));
        s
    }
}

// ───────────────────────────── paths and scopes ─────────────────────────────

/// Whether two path globs can name the same file. `**` is any folders,
/// `*` anything within one.
pub fn overlap(a: &str, b: &str) -> bool {
    let seg = |p: &str| -> Vec<String> {
        p.trim_start_matches("./")
            .split('/')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };
    fn part(a: &str, b: &str) -> bool {
        // Two segment patterns meet if what's fixed at their ends agrees.
        let (ap, bp) = (
            a.split('*').next().unwrap_or(""),
            b.split('*').next().unwrap_or(""),
        );
        let (az, bz) = (
            a.rsplit('*').next().unwrap_or(""),
            b.rsplit('*').next().unwrap_or(""),
        );
        match (a.contains('*'), b.contains('*')) {
            (false, false) => a == b,
            (true, false) => b.len() >= ap.len() + az.len() && b.starts_with(ap) && b.ends_with(az),
            (false, true) => part(b, a),
            (true, true) => {
                (ap.starts_with(bp) || bp.starts_with(ap)) && (az.ends_with(bz) || bz.ends_with(az))
            }
        }
    }
    fn go(a: &[String], b: &[String]) -> bool {
        match (a.first().map(String::as_str), b.first().map(String::as_str)) {
            (None, None) => true,
            (Some("**"), _) => go(&a[1..], b) || (!b.is_empty() && go(a, &b[1..])),
            (_, Some("**")) => go(b, a),
            (Some(x), Some(y)) => part(x, y) && go(&a[1..], &b[1..]),
            _ => false,
        }
    }
    go(&seg(a), &seg(b))
}

/// Whether a path is inside a scope: any of its globs names it.
pub fn in_scope(scope: &[String], path: &str) -> bool {
    scope.iter().any(|g| overlap(g, path))
}

/// The paths that are CI configuration: a branch trix pushes runs them on
/// GitHub with the repository's secrets.
pub fn ci_globs(extra: &[String]) -> Vec<String> {
    let mut v = vec![
        ".github/workflows/**".to_string(),
        ".github/actions/**".to_string(),
    ];
    v.extend(extra.iter().cloned());
    v
}

// ───────────────────────────── the graph ─────────────────────────────

/// What can be reached from `from` along edges, itself not counted unless
/// a loop leads back.
pub fn reach(w: &Workflow, from: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![from.to_string()];
    while let Some(n) = todo.pop() {
        for e in w.out(&n) {
            if w.nodes.contains_key(&e.to) && seen.insert(e.to.clone()) {
                todo.push(e.to.clone());
            }
        }
    }
    seen
}

/// The edges that lead back to a node already on the way: each is a loop.
pub fn back_edges(w: &Workflow) -> Vec<usize> {
    fn visit(
        w: &Workflow,
        n: &str,
        stack: &mut Vec<String>,
        done: &mut BTreeSet<String>,
        back: &mut BTreeSet<usize>,
    ) {
        stack.push(n.to_string());
        for (i, e) in w
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.from == n || e.from == "*")
        {
            if !w.nodes.contains_key(&e.to) {
                continue;
            }
            if stack.contains(&e.to) {
                back.insert(i);
            } else if !done.contains(&e.to) {
                visit(w, &e.to, stack, done, back);
            }
        }
        stack.pop();
        done.insert(n.to_string());
    }
    let (mut done, mut back) = (BTreeSet::new(), BTreeSet::new());
    for start in w.entries() {
        visit(w, start, &mut Vec::new(), &mut done, &mut back);
    }
    back.into_iter().collect()
}

/// What can be reached from `from` going forward only: no edge that
/// leads back is followed.
fn forward(w: &Workflow, from: &str, back: &[usize]) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![from.to_string()];
    while let Some(n) = todo.pop() {
        for (i, e) in w
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.from == n || e.from == "*")
        {
            if !back.contains(&i) && w.nodes.contains_key(&e.to) && seen.insert(e.to.clone()) {
                todo.push(e.to.clone());
            }
        }
    }
    seen
}

/// The nodes on the loop a back edge closes: from where it leads, forward,
/// to where it starts.
fn loop_nodes(w: &Workflow, edge: usize, back: &[usize]) -> BTreeSet<String> {
    let e = &w.edges[edge];
    let mut nodes: BTreeSet<String> = forward(w, &e.to, back)
        .into_iter()
        .filter(|n| *n == e.from || forward(w, n, back).contains(&e.from))
        .collect();
    nodes.insert(e.to.clone());
    if e.from != "*" {
        nodes.insert(e.from.clone());
    }
    nodes
}

/// No edge puts these two in order: they can run at the same time. An
/// edge that leads back doesn't order anything.
pub fn concurrent(w: &Workflow, a: &str, b: &str) -> bool {
    let back = back_edges(w);
    a != b && !forward(w, a, &back).contains(b) && !forward(w, b, &back).contains(a)
}

/// The verdicts a node can give: declared, or what its kind always gives.
pub fn verdicts(n: &Node) -> Vec<String> {
    let mut v = n.verdicts.clone();
    let implied: &[&str] = match n.kind.as_str() {
        "check" => &["pass", "fail"],
        "land" => &["landed"],
        _ => &[],
    };
    for i in implied {
        if !v.iter().any(|x| x == i) {
            v.push(i.to_string());
        }
    }
    v
}

/// What a node's report can be asked for in a template.
fn fields(n: &Node) -> Vec<String> {
    let mut f: Vec<String> = n
        .output
        .keys()
        .cloned()
        .chain(n.outputs.iter().cloned())
        .collect();
    let implied: &[&str] = match n.kind.as_str() {
        "check" => &["exit_code", "log_tail", "commit", "verdict"],
        "land" => &["head", "verdict"],
        "for_each" | "join" => &["results", "verdict", "head"],
        _ => &["verdict", "summary", "head", "commit"],
    };
    f.extend(implied.iter().map(|s| s.to_string()));
    f
}

/// Fields of an input that are safe unfenced: they can't carry instructions.
const SAFE_FIELDS: [&str; 7] = ["number", "id", "url", "sha", "branch", "base", "head"];

/// The `{{…}}` in a template: what each names, and whether it's fenced.
pub fn refs(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        let Some(j) = rest[i..].find("}}") else { break };
        let inner = rest[i + 2..i + j].trim();
        rest = &rest[i + j + 2..];
        let (fenced, name) = match inner.split_once(' ') {
            Some(("fence", n)) => (true, n.trim()),
            Some(("#if", n)) => (true, n.trim()),
            _ => (false, inner),
        };
        if !name.starts_with('/') && !name.is_empty() {
            out.push((name.to_string(), fenced));
        }
    }
    out
}

/// Whether a reference names something a run will have.
fn known(w: &Workflow, name: &str) -> Result<(), String> {
    let mut parts = name.split('.');
    let root = parts.next().unwrap_or_default();
    let field = parts.next();
    match (root, field) {
        ("goal", Some("text"))
        | ("run", Some("id" | "state"))
        | ("inbox" | "item" | "node" | "worktree" | "repo", None) => Ok(()),
        ("config" | "board", Some(_)) => Ok(()),
        ("inputs", Some(i)) if w.inputs.contains_key(i) => Ok(()),
        ("inputs", Some(i)) => Err(format!("there's no input '{i}'")),
        (node, Some(f)) if w.nodes.contains_key(node) => {
            if fields(&w.nodes[node]).iter().any(|x| x == f) {
                Ok(())
            } else {
                Err(format!("{node} has no output '{f}'"))
            }
        }
        _ => Err(format!("'{name}' isn't something a run has")),
    }
}

// ───────────────────────────── the nodes ─────────────────────────────

/// The profile a node starts its backend with: the workflow's own, else a
/// shipped one. None: the profile has no arguments for that kind.
pub fn profile_of(w: &Workflow, n: &Node) -> Option<Profile> {
    let kind = n.agent.clone()?;
    let Some(name) = &n.profile else {
        return Some(Profile {
            kind,
            args: n.args.as_ref().map(|a| a.list()).unwrap_or_default(),
        });
    };
    let shipped = shipped_profiles();
    let args = w
        .profiles
        .get(name)
        .and_then(|p| p.get(&kind))
        .or_else(|| shipped.get(name).and_then(|p| p.get(&kind)))?;
    Some(Profile {
        kind,
        args: args.clone(),
    })
}

/// The tools that permission rules with `mode: allow` let through for a node.
fn rule_tools(w: &Workflow, id: &str) -> Vec<String> {
    w.approvals
        .permissions
        .iter()
        .filter(|r| r.mode == Allow::Allow && (r.node == "*" || r.node == id))
        .flat_map(|r| r.tools.iter().cloned())
        .collect()
}

/// What the matrix and its profile make of an agent node.
pub fn label(w: &Workflow, m: &Matrix, ctx: &Ctx, id: &str) -> Option<Label> {
    let n = w.nodes.get(id)?;
    let kind = n.agent.clone()?;
    let entry = m.get(&kind)?;
    let profile = profile_of(w, n);
    let level = m.level(&kind, &ctx.installed);
    let hands_in = level.yes()
        && profile.as_ref().is_some_and(|p| !p.cant_hand_in())
        && !matches!(entry.hand_in, super::caps::HandIn::All(false));
    let protection = match (w.policy.push, &profile) {
        (Push::Allow, _) => Protection::None,
        (_, Some(p)) if level.yes() => p.protection(m, &rule_tools(w, id)),
        _ => Protection::Guarded,
    };
    let read_only = n
        .role()
        .filter(|r| r.read_only())
        .map(|_| profile.as_ref().is_some_and(|p| p.read_only_enforced(m)));
    Some(Label {
        kind: kind.clone(),
        level,
        hands_in,
        protection,
        read_only,
        decision: m.decision(&kind, &ctx.installed),
        priced: entry.priced,
    })
}

fn writers(w: &Workflow) -> Vec<&str> {
    w.nodes
        .iter()
        .filter(|(_, n)| n.writes())
        .map(|(id, _)| id.as_str())
        .collect()
}

/// A writer that gets a worktree of its own: it asked for one, it fans
/// out, or another writer can run at the same time.
pub fn isolated(w: &Workflow, id: &str) -> bool {
    let n = &w.nodes[id];
    n.writes()
        && (n.workspace == Some(Workspace::Isolated)
            || (n.kind == "for_each" && n.parallel != Some(1))
            || writers(w).iter().any(|o| concurrent(w, id, o)))
}

/// How many writers a node stands for: a fan-out is as many as run at once.
fn weight(n: &Node) -> u32 {
    if n.kind == "for_each" {
        n.parallel.unwrap_or(5)
    } else {
        1
    }
}

// ───────────────────────────── the checker ─────────────────────────────

/// Check a workflow: every problem the matrix and the graph can predict,
/// each with what to do about it.
pub fn check(w: &Workflow, m: &Matrix, ctx: &Ctx) -> Report {
    let mut p: Vec<Problem> = Vec::new();
    let mut add = |level: Severity, code: &'static str, at: At, text: String, fixes: Vec<Fix>| {
        p.push(Problem {
            level,
            code,
            at,
            text,
            fixes,
        });
    };
    use Severity::{Blocks, Error, Info, Warning};
    let node = |id: &str| At::Node(id.to_string());
    let labels: BTreeMap<String, Label> = w
        .nodes
        .keys()
        .filter_map(|id| Some((id.clone(), label(w, m, ctx, id)?)))
        .collect();
    // A kind that fits where another doesn't: installed, hands in.
    let other = |not: &str| -> Option<String> {
        ["claude", "codex", "gemini", "opencode"]
            .iter()
            .find(|k| **k != not && ctx.installed.0.contains_key(**k) && m.get(k).is_some())
            .map(|k| k.to_string())
    };

    if ctx.trusted == Some(false) {
        add(Blocks, "untrusted", At::Workflow, "untrusted, or changed since trust was given: it can't run until you trust it (trix workflow trust)".into(), vec![]);
    }

    // ── backends ──
    let mut versions_said = BTreeSet::new();
    let mut untrusted_codex = Vec::new();
    for (id, n) in w.nodes.iter().filter(|(_, n)| n.is_agent()) {
        let Some(kind) = n.agent.as_deref() else {
            add(
                Error,
                "no-backend",
                node(id),
                "no backend chosen".into(),
                vec![fix("pick a backend", FixOp::Manual)],
            );
            continue;
        };
        // Bound when the run starts ({{config.…}}): nothing to hold it against yet.
        if kind.starts_with("{{") {
            continue;
        }
        let Some(entry) = m.get(kind) else {
            add(
                Error,
                "backend-unknown",
                node(id),
                format!("'{kind}' isn't a backend trix knows"),
                vec![fix("pick a backend", FixOp::Manual)],
            );
            continue;
        };
        if !ctx.installed.0.contains_key(kind) {
            let mut fixes = vec![fix(format!("install {kind}"), FixOp::Manual)];
            fixes.extend(other(kind).map(|k| {
                fix(
                    format!("switch to {k}"),
                    FixOp::SetAgent {
                        node: id.clone(),
                        kind: k,
                    },
                )
            }));
            add(
                Error,
                "backend-missing",
                node(id),
                format!("{kind} isn't installed"),
                fixes,
            );
            continue;
        }
        if m.in_range(kind, &ctx.installed) == Some(false) && versions_said.insert(kind.to_string())
        {
            let have = ctx.installed.0.get(kind).cloned().unwrap_or_default();
            let range = entry
                .versions
                .as_ref()
                .map_or(String::new(), |v| format!("{} to {}", v.min, v.max));
            add(
                Warning,
                "backend-version",
                At::Workflow,
                format!(
                    "{kind} {have} is outside the matrix's range ({range}): its entry counts as documented, and rule approvals on it are suspended"
                ),
                vec![fix(
                    format!("trix workflow capabilities --verify --kind {kind}"),
                    FixOp::Probe(kind.into()),
                )],
            );
        }
        if kind == "codex" && !ctx.codex_hooks_trusted {
            untrusted_codex.push(id.clone());
        }
        let Some(profile) = profile_of(w, n) else {
            let name = n.profile.clone().unwrap_or_default();
            if kind == "codex" && name == "read-only" {
                add(
                    Error,
                    "codex-read-only",
                    node(id),
                    "Codex in read-only can't reach Trix, so it can never hand in".into(),
                    vec![
                        fix(
                            "switch to claude",
                            FixOp::SetAgent {
                                node: id.clone(),
                                kind: "claude".into(),
                            },
                        ),
                        fix(
                            "use instruct-read-only (⚠ not enforced)",
                            FixOp::SetProfile {
                                node: id.clone(),
                                profile: "instruct-read-only".into(),
                            },
                        ),
                    ],
                );
            } else {
                add(
                    Error,
                    "profile-kind",
                    node(id),
                    format!("the profile '{name}' has no arguments for {kind}"),
                    vec![fix(
                        "pick another profile, or another backend",
                        FixOp::Manual,
                    )],
                );
            }
            continue;
        };
        if profile.cant_hand_in() {
            add(
                Error,
                "codex-read-only",
                node(id),
                "Codex in read-only can't reach Trix, so it can never hand in".into(),
                vec![
                    fix(
                        "switch to claude",
                        FixOp::SetAgent {
                            node: id.clone(),
                            kind: "claude".into(),
                        },
                    ),
                    fix(
                        "use instruct-read-only (⚠ not enforced)",
                        FixOp::SetProfile {
                            node: id.clone(),
                            profile: "instruct-read-only".into(),
                        },
                    ),
                ],
            );
        }
        let l = &labels[id];
        if w.policy.push == Push::GateOnly && l.protection != Protection::Enforced {
            let why = match profile.off_safe_list().as_slice() {
                [] if kind != "claude" => format!("{kind} can't be kept from pushing"),
                [] => "its profile doesn't name the tools it allows".into(),
                off => format!(
                    "its profile allows {}, which isn't on the safe list",
                    off.join(", ")
                ),
            };
            add(
                Error,
                "gate-only-not-enforced",
                node(id),
                format!(
                    "push: gate_only needs every agent enforced, and this one's push protection is {}: {why}",
                    l.protection.label()
                ),
                vec![
                    fix(
                        "use the edit-only profile on claude, and let a check node run the tests",
                        FixOp::SetProfile {
                            node: id.clone(),
                            profile: "edit-only".into(),
                        },
                    ),
                    fix(
                        "set push: guarded (⚠ guarded, not enforced)",
                        FixOp::SetPush(Push::Guarded),
                    ),
                ],
            );
        }
        if l.read_only == Some(false) {
            let why = if kind == "claude" {
                "its profile runs repository code, or doesn't name its tools"
            } else {
                "only a Claude safe-list profile is enforced"
            };
            add(
                Warning,
                "read-only-by-instruction",
                node(id),
                format!("read-only by instruction only ({kind}): {why}"),
                vec![fix(
                    "switch to claude with read-only, or accept",
                    FixOp::Manual,
                )],
            );
        }
        if entry.quota_signal == "none"
            && n.on_quota
                .as_ref()
                .and_then(|q| q.fallback.as_ref())
                .is_none()
        {
            add(
                Warning,
                "no-quota-signal",
                node(id),
                format!(
                    "{kind} gives no quota or rate-limit signal: a quota stop looks like an idle agent"
                ),
                vec![fix(
                    "accept (the nudge and the stall monitor catch it), or set on_quota.fallback",
                    FixOp::Manual,
                )],
            );
        }
        if n.revisit() == Revisit::Reuse && entry.resume == "no" {
            add(
                Warning,
                "reuse-cant-resume",
                node(id),
                format!("{kind} can't resume, so a reused pane that exits starts over"),
                vec![fix(
                    "set revisit: fresh",
                    FixOp::SetRevisit {
                        node: id.clone(),
                        revisit: Revisit::Fresh,
                    },
                )],
            );
        }
    }
    if !untrusted_codex.is_empty() {
        add(
            Warning,
            "codex-hooks",
            At::Workflow,
            format!(
                "Codex's hooks aren't trusted yet, so state comes from notify only: {}",
                untrusted_codex.join(", ")
            ),
            vec![fix(
                "open a Codex pane, run /hooks, and trust Trix's hooks",
                FixOp::Manual,
            )],
        );
    }

    // ── push ──
    if w.policy.push == Push::Guarded {
        let guarded: Vec<&str> = w
            .nodes
            .iter()
            .filter(|(id, n)| {
                n.kind == "check"
                    || labels
                        .get(*id)
                        .is_some_and(|l| l.protection == Protection::Guarded)
            })
            .map(|(id, _)| id.as_str())
            .collect();
        if !guarded.is_empty() {
            add(
                Warning,
                "push-guarded",
                At::Workflow,
                format!("push guarded, not enforced: {}", guarded.join(", ")),
                vec![fix(
                    "accept, or move to gate_only with safe-list profiles",
                    FixOp::Manual,
                )],
            );
        }
    }
    for (id, _) in w.nodes.iter().filter(|(_, n)| n.kind == "check") {
        add(
            Info,
            "check-guarded",
            node(id),
            "guarded: it runs code the agents wrote, in a process of its own with the guard on"
                .into(),
            vec![],
        );
    }
    if w.policy.push != Push::Allow {
        let ci = ci_globs(&ctx.ci_paths);
        for id in writers(w) {
            let owns = &w.nodes[id].brief.as_ref().unwrap().owns;
            if owns.iter().any(|o| ci.iter().any(|c| overlap(o, c))) {
                add(Warning, "ci-config-owned", node(id), "its write scope covers CI configuration: the CI-config floor will park the gate for you before trix pushes such a change".into(), vec![fix("narrow owns, or accept", FixOp::Manual)]);
            }
        }
    }

    // ── briefs and scopes ──
    for (id, n) in w.nodes.iter().filter(|(_, n)| n.is_agent()) {
        match &n.brief {
            None => add(Error, "brief-missing", node(id), "an agent node needs a brief: what it owns, what it must not touch, what done means".into(), vec![fix("fill in the brief", FixOp::SetBrief { node: id.clone(), brief: suggest_brief(n) })]),
            Some(b) if b.done.trim().is_empty() => add(Error, "brief-no-done", node(id), "its brief doesn't say what done means".into(), vec![fix("fill in done", FixOp::SetBrief { node: id.clone(), brief: Brief { done: suggest_brief(n).done, ..b.clone() } })]),
            Some(b) if b.owns.is_empty() && matches!(n.role, Some(Role::Implementer | Role::Worker)) => add(Error, "writer-no-scope", node(id), "a writer with no write scope: say what it owns".into(), vec![fix("fill in owns", FixOp::SetBrief { node: id.clone(), brief: Brief { owns: suggest_brief(n).owns, ..b.clone() } })]),
            Some(b) => {
                if let Some(bad) = b.owns.iter().find(|o| b.must_not_touch.iter().any(|x| overlap(o, x))) {
                    add(Error, "must-not-touch", node(id), format!("it owns {bad}, which its own brief says it must not touch"), vec![fix("narrow the scope", FixOp::Manual)]);
                }
            }
        }
    }
    let ws = writers(w);
    for (i, a) in ws.iter().enumerate() {
        let owns = &w.nodes[*a].brief.as_ref().unwrap().owns;
        for (other_id, o) in w.nodes.iter().filter(|(id, _)| id.as_str() != *a) {
            let Some(b) = &o.brief else { continue };
            if let Some(bad) = owns
                .iter()
                .find(|x| b.must_not_touch.iter().any(|y| overlap(x, y)))
            {
                add(
                    Error,
                    "must-not-touch",
                    node(a),
                    format!("it owns {bad}, which {other_id}'s brief says must not be touched"),
                    vec![fix(
                        "narrow the scope, or change the other brief",
                        FixOp::Manual,
                    )],
                );
            }
        }
        for b in &ws[i + 1..] {
            let theirs = &w.nodes[*b].brief.as_ref().unwrap().owns;
            if concurrent(w, a, b) && owns.iter().any(|x| theirs.iter().any(|y| overlap(x, y))) {
                add(
                    Warning,
                    "scope-overlap",
                    node(a),
                    format!(
                        "{a} and {b} can run at the same time and their write scopes overlap: a likely land conflict (they get separate worktrees either way)"
                    ),
                    vec![fix(
                        format!("split the scopes, or order them with an edge from {a} to {b}"),
                        FixOp::Manual,
                    )],
                );
            }
        }
    }

    // ── verifiers ──
    let verifiers: Vec<&str> = w
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == Some(Role::Verifier) && n.is_agent())
        .map(|(id, _)| id.as_str())
        .collect();
    for id in &verifiers {
        let n = &w.nodes[*id];
        if n.writes() || n.revisit == Some(Revisit::Reuse) {
            let why = if n.writes() {
                "it has a write scope, so it's an author too"
            } else {
                "it keeps its pane between visits, and so its author's answers"
            };
            add(
                Error,
                "verifier-is-author",
                node(id),
                format!(
                    "a verifier must not be the author of what it verifies, or share its context: {why}"
                ),
                vec![fix(
                    "use a separate verifier node with revisit: fresh",
                    FixOp::SetRevisit {
                        node: id.to_string(),
                        revisit: Revisit::Fresh,
                    },
                )],
            );
        }
    }
    let writing: u32 = ws.iter().map(|id| weight(&w.nodes[*id])).sum();
    if writing > 4 * verifiers.len().max(1) as u32 {
        add(
            Warning,
            "verifier-ratio",
            At::Workflow,
            format!(
                "{writing} writers for {}: about one verifier per 2 to 4 writers",
                if verifiers.is_empty() {
                    "no verifier".into()
                } else {
                    format!("{} verifier(s)", verifiers.len())
                }
            ),
            vec![fix("add a verifier (v in the builder)", FixOp::Manual)],
        );
    }
    let writer_kinds: BTreeSet<&str> = ws
        .iter()
        .filter_map(|id| w.nodes[*id].agent.as_deref())
        .collect();
    for id in &verifiers {
        let kind = w.nodes[*id].agent.as_deref().unwrap_or_default();
        if writer_kinds.len() == 1 && writer_kinds.contains(kind) {
            let mut fixes = vec![fix("move it to another backend", FixOp::Manual)];
            if let Some(k) = other(kind) {
                fixes.insert(
                    0,
                    fix(
                        format!("move it to {k}"),
                        FixOp::SetAgent {
                            node: id.to_string(),
                            kind: k,
                        },
                    ),
                );
            }
            add(
                Warning,
                "verifier-same-backend",
                node(id),
                format!(
                    "it runs on {kind}, as every writer does: another backend sees what this one misses"
                ),
                fixes,
            );
        }
    }

    // ── workspaces ──
    let lands: Vec<&str> = w
        .nodes
        .iter()
        .filter(|(_, n)| n.kind == "land")
        .map(|(id, _)| id.as_str())
        .collect();
    for id in ws.iter().filter(|id| isolated(w, id)) {
        let reached = reach(w, id);
        if !lands.iter().any(|l| reached.contains(*l)) {
            let text = if lands.is_empty() {
                "it works in a worktree of its own, and there's no land step to bring its commits in"
            } else {
                "it works in a worktree of its own, and no land step can be reached from it"
            };
            add(
                Error,
                "land-missing",
                node(id),
                text.into(),
                vec![fix("add the land step", FixOp::AddLand)],
            );
        }
    }

    // ── approvals ──
    let can_approve = |id: &str| {
        w.nodes.get(id).is_some_and(|n| {
            n.is_agent()
                && !n.writes()
                && labels
                    .get(id)
                    .is_some_and(|l| l.hands_in && m.get(&l.kind).is_some_and(|e| e.merge_approver))
        })
    };
    // One order: a judge, else a verifier, else a gatekeeper.
    let suggested = [Role::Judge, Role::Verifier, Role::Gatekeeper]
        .iter()
        .find_map(|r| {
            w.nodes
                .iter()
                .find(|(id, n)| n.role() == Some(*r) && can_approve(id))
                .map(|(id, _)| id.clone())
        });
    let wants_approver = w.policy.merge == Merge::Approver
        || w.approvals
            .merge
            .as_ref()
            .is_some_and(|a| a.mode == Allow::Allow);
    if wants_approver {
        let named = w.approvals.merge.as_ref().and_then(|a| a.approver.clone());
        let why = match &named {
            None => Some("no merge approver is named".to_string()),
            Some(a) if !w.nodes.contains_key(a) => Some(format!(
                "the merge approver '{a}' isn't a node of this workflow"
            )),
            Some(a) if !w.nodes[a].is_agent() => {
                Some(format!("the merge approver '{a}' isn't an agent node"))
            }
            Some(a) if w.nodes[a].writes() => Some(format!(
                "the merge approver '{a}' wrote the change: an approver never approves its own work"
            )),
            Some(a) if !can_approve(a) => Some(format!(
                "the merge approver '{a}' can't hand in, so it could never approve"
            )),
            Some(_) => None,
        };
        if let Some(why) = why {
            let fixes = match &suggested {
                Some(s) => vec![fix(
                    format!("make {s} the approver"),
                    FixOp::SetApprover(Some(s.clone())),
                )],
                None => vec![fix(
                    "you approve the merge (no judge, verifier or gatekeeper to pick)",
                    FixOp::SetApprover(None),
                )],
            };
            add(Error, "merge-approver", At::Approvals, why, fixes);
        }
    }
    for (i, r) in w.approvals.permissions.iter().enumerate() {
        if r.approver.as_deref().is_some_and(|a| a != "rule") {
            add(
                Error,
                "rule-agent-approver",
                At::Rule(i),
                format!(
                    "'{}' can't approve a permission prompt: agent approvers approve merges only",
                    r.approver.clone().unwrap_or_default()
                ),
                vec![fix(
                    "change the approver to rule",
                    FixOp::RuleApproverRule(i),
                )],
            );
            continue;
        }
        if r.mode != Allow::Allow {
            continue;
        }
        if r.node != "*" && !w.nodes.contains_key(&r.node) {
            add(
                Error,
                "rule-node",
                At::Rule(i),
                format!("'{}' isn't a node of this workflow", r.node),
                vec![fix("delete the rule", FixOp::RemoveRule(i))],
            );
            continue;
        }
        for (id, n) in w
            .nodes
            .iter()
            .filter(|(id, n)| n.is_agent() && (r.node == "*" || r.node == **id))
        {
            let (Some(l), Some(profile)) = (labels.get(id), profile_of(w, n)) else {
                continue;
            };
            match l.decision {
                Level::Unverified => add(
                    Error,
                    "rule-no-decision",
                    At::Rule(i),
                    format!(
                        "{id} runs on {}, which has no verified way to answer a permission prompt: a rule there could never be trusted",
                        l.kind
                    ),
                    vec![
                        fix(
                            "remove the rule and let the profile decide",
                            FixOp::RemoveRule(i),
                        ),
                        fix(
                            "switch to claude",
                            FixOp::SetAgent {
                                node: id.clone(),
                                kind: "claude".into(),
                            },
                        ),
                    ],
                ),
                Level::Documented => add(
                    Warning,
                    "rule-documented",
                    At::Rule(i),
                    format!(
                        "{}'s hook decision is documented but not verified for the installed version: the rule is suspended, and prompts it would answer park the run",
                        l.kind
                    ),
                    vec![fix(
                        format!("run the probe for {} now (spends a little quota)", l.kind),
                        FixOp::Probe(l.kind.clone()),
                    )],
                ),
                Level::Verified => {}
            }
            let allowed = profile.allowed().unwrap_or_default();
            if profile.never_prompts() {
                add(
                    Warning,
                    "dead-rule",
                    At::Rule(i),
                    format!(
                        "it can never fire on {id}: its profile never shows a prompt (what isn't allowed is refused)"
                    ),
                    vec![fix(
                        "switch the profile to a prompting mode, or delete the rule",
                        FixOp::RemoveRule(i),
                    )],
                );
            } else if !r.tools.is_empty() && r.tools.iter().all(|t| allowed.contains(t)) {
                add(
                    Warning,
                    "dead-rule",
                    At::Rule(i),
                    format!(
                        "it can never fire on {id}: its profile already allows {}",
                        r.tools.join(", ")
                    ),
                    vec![fix("delete the rule", FixOp::RemoveRule(i))],
                );
            }
        }
    }
    if !w.approvals.is_empty() && !ctx.approvals_allow.contains(&w.name) {
        add(
            Info,
            "approvals-off",
            At::Approvals,
            format!(
                "approvals stay off until '{}' is in [approvals] allow in your config: a file can't switch them on alone",
                w.name
            ),
            vec![],
        );
    }

    // ── the graph ──
    let entries = w.entries();
    if entries.is_empty() || entries.iter().any(|e| !w.nodes.contains_key(*e)) {
        add(
            Error,
            "no-entry",
            At::Workflow,
            "nowhere to start: every node has an edge leading to it, and entry names none".into(),
            vec![fix("name the first node in entry", FixOp::Manual)],
        );
    }
    let reached: BTreeSet<String> = entries
        .iter()
        .flat_map(|e| reach(w, e).into_iter().chain([e.to_string()]))
        .collect();
    for id in w
        .nodes
        .keys()
        .filter(|id| !reached.contains(*id) && w.nodes[*id].kind != "supervisor")
    {
        add(
            Error,
            "unreachable",
            node(id),
            "no edge leads here: it can never run".into(),
            vec![fix(
                "connect it, or remove it",
                FixOp::RemoveNode(id.clone()),
            )],
        );
    }
    for (id, n) in &w.nodes {
        for v in verdicts(n) {
            if EVENTS.contains(&v.as_str()) {
                add(
                    Error,
                    "verdict-is-event",
                    node(id),
                    format!("'{v}' is a runtime event: a verdict can't share its name"),
                    vec![],
                );
            } else if w.follow(id, &v).is_none() {
                add(
                    Error,
                    "verdict-no-edge",
                    node(id),
                    format!(
                        "its verdict '{v}' has no edge: a run that gets it would have nowhere to go"
                    ),
                    vec![fix(
                        format!("add an edge on {v}"),
                        FixOp::AddEdge {
                            from: id.clone(),
                            to: FAIL.into(),
                            on: v.clone(),
                        },
                    )],
                );
            }
        }
    }
    for (i, e) in w.edges.iter().enumerate() {
        if e.from != "*" && !w.nodes.contains_key(&e.from) {
            add(
                Error,
                "edge-unknown",
                At::Edge(i),
                format!("it starts at '{}', which isn't a node", e.from),
                vec![],
            );
        } else if e.to != DONE && e.to != FAIL && !w.nodes.contains_key(&e.to) {
            add(
                Error,
                "edge-unknown",
                At::Edge(i),
                format!("it leads to '{}', which isn't a node", e.to),
                vec![],
            );
        } else if e.from != "*" {
            let gives = verdicts(&w.nodes[&e.from]);
            let implicit = matches!(
                w.nodes[&e.from].kind.as_str(),
                "gh.gate" | "gh.merge" | "workflow" | "join" | "for_each"
            );
            for t in e.on.list() {
                if !EVENTS.contains(&t) && !gives.iter().any(|v| v == t) && !implicit {
                    add(
                        Error,
                        "edge-unknown",
                        At::Edge(i),
                        format!(
                            "'{t}' isn't a verdict {} declares, or a runtime event",
                            e.from
                        ),
                        vec![],
                    );
                }
            }
        }
    }
    match w.limits.wall_clock.as_deref().map(duration) {
        Some(Ok(_)) => {}
        Some(Err(e)) => add(
            Error,
            "no-wall-clock",
            At::Workflow,
            format!("wall_clock: {e}"),
            vec![fix("set wall_clock: 3h", FixOp::SetWallClock("3h".into()))],
        ),
        None => add(
            Error,
            "no-wall-clock",
            At::Workflow,
            "no wall_clock: every run has a time limit, counted only while a node or check runs"
                .into(),
            vec![fix("set wall_clock: 3h", FixOp::SetWallClock("3h".into()))],
        ),
    }
    let back = back_edges(w);
    for i in &back {
        let on = loop_nodes(w, *i, &back);
        if w.limits.max_iterations.is_none() && on.iter().any(|n| w.nodes[n].max_visits.is_none()) {
            add(Error, "loop-no-ceiling", At::Edge(*i), "this edge leads back, and nothing stops the loop: no max_iterations, and a node on it has no max_visits".into(), vec![fix("set max_iterations: 3", FixOp::SetMaxIterations(3))]);
        }
        if !on
            .iter()
            .any(|n| w.nodes[n].writes() || w.nodes[n].kind == "check")
        {
            add(Warning, "loop-no-progress", At::Edge(*i), "progress on this loop can't be measured: nothing on it changes the diff or runs a check, so a wasted cycle can't be told from a useful one".into(), vec![fix("put a writer or a check on the loop", FixOp::Manual)]);
        }
        for n in on.iter().filter(|n| {
            w.nodes[*n].max_visits.is_none()
                && w.limits.max_iterations.is_none()
                && w.limits.budget_usd.is_some()
        }) {
            if labels.get(n).is_some_and(|l| !l.priced) {
                add(Warning, "budget-only-unpriced", node(n), "the budget is its only limit, and its backend isn't priced: nothing would stop it".into(), vec![fix("add max_visits", FixOp::SetMaxVisits { node: n.clone(), n: 3 })]);
            }
        }
    }
    for (id, _) in w.nodes.iter().filter(|(_, n)| {
        n.kind == "for_each" && n.max_visits.is_none() && w.limits.budget_usd.is_some()
    }) {
        if labels.get(id).is_some_and(|l| !l.priced) {
            add(
                Warning,
                "budget-only-unpriced",
                node(id),
                "the budget is its only limit, and its backend isn't priced: nothing would stop it"
                    .into(),
                vec![fix(
                    "add max_visits",
                    FixOp::SetMaxVisits {
                        node: id.clone(),
                        n: 2,
                    },
                )],
            );
        }
    }
    // A land conflict goes back to its author: a loop no edge draws.
    for (id, _) in w
        .nodes
        .iter()
        .filter(|(_, n)| n.kind == "land" && n.on_conflict.as_deref() != Some("fail"))
    {
        if w.limits.max_iterations.is_none() {
            add(
                Error,
                "loop-no-ceiling",
                node(id),
                "a land conflict goes back to its author, and nothing stops that loop: no max_iterations".into(),
                vec![fix("set max_iterations: 3", FixOp::SetMaxIterations(3))],
            );
        }
    }
    if w.goal.done_when.mechanical.is_empty() {
        add(
            Error,
            "no-mechanical",
            At::Workflow,
            "done_when has no mechanical condition: an agent's vote is never enough alone".into(),
            vec![fix(
                "add a check node, and name it in done_when.mechanical",
                FixOp::Manual,
            )],
        );
    }
    for c in w
        .goal
        .done_when
        .mechanical
        .iter()
        .chain(&w.goal.done_when.attested)
    {
        if let Some(name) = c
            .check
            .as_ref()
            .filter(|n| w.nodes.get(*n).is_none_or(|x| x.kind != "check"))
        {
            add(
                Error,
                "unknown-ref",
                At::Workflow,
                format!("done_when names the check '{name}', which isn't a check node"),
                vec![],
            );
        }
        if let Some(name) = c
            .judge
            .as_ref()
            .filter(|n| w.nodes.get(*n).is_none_or(|x| !x.is_agent()))
        {
            add(
                Error,
                "unknown-ref",
                At::Workflow,
                format!("done_when names '{name}' as a judge, which isn't an agent node"),
                vec![],
            );
        }
        if c.ci.is_some() && !w.nodes.values().any(|n| n.kind == "gh.gate") {
            add(
                Error,
                "unknown-ref",
                At::Workflow,
                "done_when asks for CI, and there's no gh.gate to push the commit CI runs on"
                    .into(),
                vec![],
            );
        }
    }

    // ── templates ──
    let mut texts: Vec<(At, &str)> = vec![(At::Workflow, w.goal.text.as_str())];
    for (id, n) in &w.nodes {
        texts.extend(n.prompt.as_deref().map(|t| (node(id), t)));
        texts.extend(n.run.iter().map(|t| (node(id), t.as_str())));
    }
    for (i, e) in w.edges.iter().enumerate() {
        texts.extend(e.pass.values().map(|t| (At::Edge(i), t.as_str())));
    }
    for (at, text) in texts {
        for (name, fenced) in refs(text) {
            if let Err(e) = known(w, &name) {
                add(
                    Error,
                    "unknown-ref",
                    at.clone(),
                    format!("{{{{{name}}}}}: {e}"),
                    vec![],
                );
                continue;
            }
            let mut parts = name.split('.');
            if let (Some("inputs"), Some(i), Some(field)) =
                (parts.next(), parts.next(), parts.next())
                && !fenced
                && w.inputs[i].resolve.is_some()
                && !SAFE_FIELDS.contains(&field)
            {
                add(
                    Error,
                    "unfenced",
                    at.clone(),
                    format!(
                        "{{{{{name}}}}} is text from outside, and isn't fenced: an agent would read it as instructions"
                    ),
                    vec![fix(format!("write {{{{fence {name}}}}}"), FixOp::Manual)],
                );
            }
        }
    }
    for (id, n) in &w.nodes {
        for bare in n
            .commit
            .iter()
            .chain(n.worktree.as_ref().and_then(|t| t.checkout.as_ref()))
            .chain(n.over.as_ref())
        {
            if let Err(e) = known(w, bare) {
                add(
                    Error,
                    "unknown-ref",
                    node(id),
                    format!("{bare}: {e}"),
                    vec![],
                );
            }
        }
    }

    // ── cost ──
    let estimate = estimate(w, &labels, &back);
    let over = ctx
        .confirm_over
        .or(w.limits.budget_usd.as_ref().and_then(|b| b.get()))
        .unwrap_or(10.0);
    let mut why = Vec::new();
    if estimate.peak_parallel >= 4 {
        why.push(format!("{} agents at once", estimate.peak_parallel));
    }
    if estimate.peak_parallel > 1 && estimate.ceiling * estimate.peak_parallel >= 12 {
        why.push(format!(
            "a ceiling of {} cycles times {} parallel agents",
            estimate.ceiling, estimate.peak_parallel
        ));
    }
    if estimate.usd > over {
        why.push(format!(
            "an estimate of {} against {}",
            estimate.cost(),
            crate::cost::usd(over)
        ));
    }
    if !why.is_empty() {
        add(
            Warning,
            "expensive-topology",
            At::Workflow,
            format!("an expensive topology: {}", why.join(", ")),
            vec![fix(
                "lower parallelism, use cheaper backends for workers, or cap for_each",
                FixOp::Manual,
            )],
        );
    }

    p.sort_by(|a, b| (a.level, &a.at).cmp(&(b.level, &b.at)));
    Report {
        name: w.name.clone(),
        problems: p,
        labels,
        estimate,
    }
}

/// A brief to start from: the role's usual scope and what done means for it.
pub fn suggest_brief(n: &Node) -> Brief {
    match n.role() {
        Some(Role::Implementer | Role::Worker) | None => Brief {
            owns: vec!["src/**".into(), "tests/**".into()],
            must_not_touch: vec![".github/**".into()],
            done: "The change is committed and its tests pass.".into(),
        },
        Some(Role::Judge) => Brief {
            owns: vec![],
            must_not_touch: vec![],
            done: "A pass or fail vote on the checked commit, with reasons.".into(),
        },
        Some(Role::Supervisor) => Brief {
            owns: vec![],
            must_not_touch: vec![],
            done: "Each question answered, or escalated.".into(),
        },
        Some(_) => Brief {
            owns: vec![],
            must_not_touch: vec![],
            done: "Every must-fix finding has a proof, or there are none.".into(),
        },
    }
}

/// What a priced agent visit is taken to cost before there's history.
const VISIT_USD: f64 = 0.5;

fn estimate(w: &Workflow, labels: &BTreeMap<String, Label>, back: &[usize]) -> Estimate {
    let agents: Vec<&str> = w
        .nodes
        .iter()
        .filter(|(_, n)| n.is_agent())
        .map(|(id, _)| id.as_str())
        .collect();
    // The widest set of agents no edge puts in order, counted greedily,
    // with a fan-out counting for as many as it runs at once.
    let mut peak = 0u32;
    for a in &agents {
        let side: u32 = agents
            .iter()
            .filter(|b| concurrent(w, a, b))
            .map(|b| weight(&w.nodes[*b]))
            .sum();
        peak = peak.max(weight(&w.nodes[*a]) + side);
    }
    let ceiling = w
        .limits
        .max_iterations
        .as_ref()
        .and_then(Count::get)
        .unwrap_or(1)
        .max(1);
    let looped: BTreeSet<String> = back.iter().flat_map(|i| loop_nodes(w, *i, back)).collect();
    let (mut usd, mut unknown) = (0.0, false);
    for a in &agents {
        let visits = weight(&w.nodes[*a]) as f64 * if looped.contains(*a) { 2.0 } else { 1.0 };
        match labels.get(*a) {
            Some(l) if l.priced => usd += visits * VISIT_USD,
            _ => unknown = true,
        }
    }
    Estimate {
        peak_parallel: peak.max(u32::from(!agents.is_empty())),
        min_cycles: 1,
        ceiling: if back.is_empty() { 1 } else { ceiling },
        usd,
        unknown,
    }
}

/// Apply a fix to a workflow. False: it's one only you can make.
pub fn apply(w: &mut Workflow, op: &FixOp) -> bool {
    match op {
        FixOp::SetAgent { node, kind } => w
            .nodes
            .get_mut(node)
            .map(|n| n.agent = Some(kind.clone()))
            .is_some(),
        FixOp::SetProfile { node, profile } => w
            .nodes
            .get_mut(node)
            .map(|n| n.profile = Some(profile.clone()))
            .is_some(),
        FixOp::SetPush(p) => {
            w.policy.push = *p;
            true
        }
        FixOp::SetApprover(Some(a)) => {
            w.approvals
                .merge
                .get_or_insert_with(Default::default)
                .approver = Some(a.clone());
            true
        }
        FixOp::SetApprover(None) => {
            w.policy.merge = Merge::You;
            w.approvals.merge = None;
            true
        }
        FixOp::RemoveRule(i) if *i < w.approvals.permissions.len() => {
            w.approvals.permissions.remove(*i);
            true
        }
        FixOp::RuleApproverRule(i) => w
            .approvals
            .permissions
            .get_mut(*i)
            .map(|r| r.approver = Some("rule".into()))
            .is_some(),
        FixOp::SetBrief { node, brief } => w
            .nodes
            .get_mut(node)
            .map(|n| n.brief = Some(brief.clone()))
            .is_some(),
        FixOp::SetRevisit { node, revisit } => w
            .nodes
            .get_mut(node)
            .map(|n| n.revisit = Some(*revisit))
            .is_some(),
        FixOp::SetMaxVisits { node, n } => w
            .nodes
            .get_mut(node)
            .map(|x| x.max_visits = Some(*n))
            .is_some(),
        FixOp::SetWallClock(t) => {
            w.limits.wall_clock = Some(t.clone());
            true
        }
        FixOp::SetMaxIterations(n) => {
            w.limits.max_iterations = Some(Count::N(*n));
            true
        }
        FixOp::AddEdge { from, to, on } => {
            w.edges.push(super::model::Edge {
                from: from.clone(),
                to: to.clone(),
                on: super::model::Triggers::One(on.clone()),
                guard: None,
                pass: BTreeMap::new(),
                ask: None,
            });
            true
        }
        FixOp::RemoveNode(id) => {
            w.edges.retain(|e| e.from != *id && e.to != *id);
            w.layout.remove(id);
            w.nodes.remove(id).is_some()
        }
        FixOp::AddLand => add_land(w),
        FixOp::RemoveRule(_) | FixOp::Probe(_) | FixOp::Manual => false,
    }
}

/// Put a land step after the writers that work in worktrees of their own:
/// what they led to now follows the landing.
fn add_land(w: &mut Workflow) -> bool {
    let iso: Vec<String> = writers(w)
        .into_iter()
        .filter(|id| isolated(w, id))
        .map(String::from)
        .collect();
    if iso.is_empty() || w.nodes.contains_key("land") {
        return false;
    }
    w.nodes.insert(
        "land".into(),
        Node {
            kind: "land".into(),
            on_conflict: Some("author".into()),
            ..Node::default()
        },
    );
    let mut after: BTreeSet<String> = BTreeSet::new();
    for e in w
        .edges
        .iter_mut()
        .filter(|e| iso.contains(&e.from) && !iso.contains(&e.to))
    {
        after.insert(std::mem::replace(&mut e.to, "land".into()));
    }
    if after.is_empty() {
        after.insert(DONE.into());
        for id in &iso {
            for v in verdicts(&w.nodes[id]) {
                w.edges.push(super::model::Edge {
                    from: id.clone(),
                    to: "land".into(),
                    on: super::model::Triggers::One(v),
                    guard: None,
                    pass: BTreeMap::new(),
                    ask: None,
                });
            }
        }
    }
    for to in after {
        w.edges.push(super::model::Edge {
            from: "land".into(),
            to,
            on: super::model::Triggers::One("landed".into()),
            guard: None,
            pass: BTreeMap::new(),
            ask: None,
        });
    }
    true
}

/// `trix workflow check --fix`: the first fix of every problem that has one
/// trix can make, again until nothing changes. What it did, in order.
pub fn fix_all(w: &mut Workflow, m: &Matrix, ctx: &Ctx) -> Vec<String> {
    let mut done = Vec::new();
    for _ in 0..20 {
        let report = check(w, m, ctx);
        let Some((text, f)) = report
            .problems
            .iter()
            .filter(|p| p.level == Severity::Error)
            .find_map(|p| {
                p.fixes
                    .iter()
                    .find(|f| !matches!(f.op, FixOp::Manual | FixOp::Probe(_)))
                    .map(|f| (p.text.clone(), f.clone()))
            })
        else {
            break;
        };
        if !apply(w, &f.op) {
            break;
        }
        done.push(format!("{text} → {}", f.label));
    }
    done
}
