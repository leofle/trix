//! The terminal builder: how a workflow is made, with no YAML to write.
//! The one checker runs on every edit, so each problem shows on the node,
//! edge or rule it concerns while you build, with its fixes. Everything
//! works from the keyboard; the mouse is a shortcut.

use super::canvas::{Box as NodeBox, Canvas};
use super::caps::{Level, Matrix, Protection};
use super::check::{self, At, Ctx, FixOp, Report, Severity};
use super::model::{Brief, Edge, Node, Revisit, Role, Triggers, Workflow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use std::collections::BTreeMap;

/// What the builder is doing besides showing the canvas.
#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Canvas,
    /// One line being typed: its title, the text so far, what it's for.
    Input {
        title: String,
        text: String,
        then: Then,
    },
    /// A list to pick from.
    Pick {
        title: String,
        items: Vec<String>,
        at: usize,
        then: Pick,
    },
    /// The inspector as an overlay (under 100 columns).
    Inspect,
    /// The Approvals tab.
    Approvals,
    /// Every key there is.
    Keys,
}

/// What a typed line is.
#[derive(Clone, Debug, PartialEq)]
pub enum Then {
    Owns(String),
    MustNot(String),
    Done(String),
    Trigger { from: String, to: String },
    Run(String),
}

/// What a picked item is.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Backend(String),
    ConnectTo(String),
    Fix(String),
    Approver,
}

/// What a key came to, for whoever runs the builder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    Stay,
    Save,
    DryRun,
    Run,
    Quit,
}

/// How a backend fits a node's role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fit {
    Fits,
    Limits,
    Cant,
}

impl Fit {
    pub fn mark(self) -> &'static str {
        match self {
            Fit::Fits => "✓",
            Fit::Limits => "⚠",
            Fit::Cant => "✗",
        }
    }
}

pub struct Builder {
    pub w: Workflow,
    m: Matrix,
    ctx: Ctx,
    pub report: Report,
    pub selected: Option<String>,
    pub mode: Mode,
    undo: Vec<Workflow>,
    redo: Vec<Workflow>,
    /// Changed since it was saved.
    pub dirty: bool,
    pub message: String,
    canvas: Canvas,
    /// Where the canvas was last drawn: a mouse event is held against it.
    area: Rect,
}

/// The profile a role usually takes on a backend.
fn profile_for(role: Option<Role>, kind: &str) -> &'static str {
    match (role.is_some_and(|r| r.read_only()), kind) {
        (true, "claude") => "read-only",
        (true, _) => "instruct-read-only",
        (false, _) => "edit-and-test",
    }
}

impl Builder {
    pub fn new(w: Workflow, m: Matrix, ctx: Ctx) -> Builder {
        let report = check::check(&w, &m, &ctx);
        let selected = w.entries().first().map(|s| s.to_string()).or(w
            .nodes
            .keys()
            .next()
            .cloned());
        let mut b = Builder {
            w,
            m,
            ctx,
            report,
            selected,
            mode: Mode::Canvas,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
            message: String::new(),
            canvas: Canvas::new(&[], &[]),
            area: Rect::default(),
        };
        if b.w.nodes.keys().any(|id| !b.w.layout.contains_key(id)) {
            b.place();
        }
        b.sync();
        b
    }

    /// The problems the builder shows: the checker's report, which is what
    /// it draws from.
    #[cfg(test)]
    pub fn problems(&self) -> &Report {
        &self.report
    }

    /// One edit: kept for undo, checked at once, drawn again.
    fn edit(&mut self, f: impl FnOnce(&mut Workflow)) {
        self.undo.push(self.w.clone());
        self.redo.clear();
        f(&mut self.w);
        self.dirty = true;
        self.refresh();
    }

    fn refresh(&mut self) {
        self.report = check::check(&self.w, &self.m, &self.ctx);
        if self
            .selected
            .as_ref()
            .is_some_and(|s| !self.w.nodes.contains_key(s))
        {
            self.selected = self.w.nodes.keys().next().cloned();
        }
        self.sync();
    }

    /// A node's badges: what the matrix and the checker say of it, short.
    pub fn badges(&self, id: &str) -> Vec<(String, Severity)> {
        let mut out = Vec::new();
        let n = &self.w.nodes[id];
        if let Some(l) = self.report.labels.get(id) {
            if !l.hands_in {
                out.push(("hand-in ✗".to_string(), Severity::Error));
            }
            let (text, level) = match l.protection {
                Protection::Enforced => ("push enforced", Severity::Info),
                Protection::Guarded => ("push guarded", Severity::Warning),
                Protection::None => ("push none", Severity::Info),
            };
            out.push((text.to_string(), level));
            if !l.priced {
                out.push(("$ ?".to_string(), Severity::Info));
            }
            let approves = self
                .w
                .approvals
                .merge
                .as_ref()
                .and_then(|a| a.approver.as_deref())
                == Some(id);
            out.push((
                if approves {
                    "approve ⚡"
                } else {
                    "approve —"
                }
                .to_string(),
                Severity::Info,
            ));
        } else if n.kind == "check" {
            out.push(("guarded".to_string(), Severity::Info));
        }
        let at = At::Node(id.to_string());
        let errors = self
            .report
            .at(&at)
            .filter(|p| p.level == Severity::Error)
            .count();
        let warnings = self
            .report
            .at(&at)
            .filter(|p| p.level == Severity::Warning)
            .count();
        if errors > 0 {
            out.push((format!("✗{errors}"), Severity::Error));
        }
        if warnings > 0 {
            out.push((format!("⚠{warnings}"), Severity::Warning));
        }
        out
    }

    fn sync(&mut self) {
        let boxes: Vec<NodeBox> = self
            .w
            .nodes
            .iter()
            .map(|(id, n)| {
                let worst = self.badges(id).iter().map(|(_, s)| *s).min();
                let mark = match worst {
                    Some(Severity::Error) => " ✗",
                    Some(Severity::Warning) => " ⚠",
                    _ => "",
                };
                let who = n.agent.clone().unwrap_or_else(|| n.kind.clone());
                let owns = n
                    .brief
                    .as_ref()
                    .filter(|b| !b.owns.is_empty())
                    .map_or(String::new(), |b| format!("\n{}", b.owns.join(",")));
                NodeBox {
                    id: id.clone(),
                    at: self.w.layout.get(id).copied().unwrap_or([2, 1]),
                    label: format!("{id}{mark}\n{who}{owns}"),
                }
            })
            .collect();
        let edges: Vec<(String, String)> = self
            .w
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        self.canvas.set(&boxes, &edges, self.selected.as_deref());
    }

    /// Put every node somewhere: layered by the edges, top to bottom.
    pub fn place(&mut self) {
        let boxes: Vec<NodeBox> = self
            .w
            .nodes
            .keys()
            .map(|id| NodeBox {
                id: id.clone(),
                at: [0, 0],
                label: id.clone(),
            })
            .collect();
        // Only edges between nodes: `@done` and `@fail` aren't places.
        let known = |id: &String| self.w.nodes.contains_key(id);
        let edges: Vec<(String, String)> = self
            .w
            .edges
            .iter()
            .filter(|e| known(&e.from) && known(&e.to))
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        self.w.layout.retain(|id, _| known(id));
        for (id, at) in Canvas::layout(&boxes, &edges) {
            if known(&id) {
                self.w.layout.insert(id, at);
            }
        }
    }

    // ── what the keys do ──

    /// Add a node. An agent node asks for its brief before anything else.
    pub fn add(&mut self, kind: &str) -> String {
        let stem = match kind {
            "check" => "check",
            "judge" => "judge",
            "gh.gate" => "gate",
            "land" => "land",
            "supervisor" => "lead",
            _ => "agent",
        };
        let id = (1..)
            .map(|i| {
                if i == 1 {
                    stem.to_string()
                } else {
                    format!("{stem}-{i}")
                }
            })
            .find(|c| !self.w.nodes.contains_key(c))
            .unwrap_or_default();
        let at = self
            .selected
            .as_ref()
            .and_then(|s| self.w.layout.get(s))
            .map_or([2, 1], |p| [p[0] + 24, p[1]]);
        let node = match kind {
            "check" => Node {
                kind: "check".into(),
                run: vec!["npm".into(), "test".into()],
                ..Node::default()
            },
            "judge" => Node {
                kind: "judge".into(),
                agent: Some("claude".into()),
                profile: Some("read-only".into()),
                verdicts: vec!["pass".into(), "fail".into()],
                ..Node::default()
            },
            "agent" => Node {
                kind: "agent".into(),
                role: Some(Role::Implementer),
                agent: Some("claude".into()),
                profile: Some("edit-and-test".into()),
                verdicts: vec!["ready".into()],
                ..Node::default()
            },
            other => Node {
                kind: other.into(),
                ..Node::default()
            },
        };
        let agent = node.is_agent();
        self.edit(|w| {
            w.nodes.insert(id.clone(), node);
            w.layout.insert(id.clone(), at);
        });
        self.selected = Some(id.clone());
        self.sync();
        if agent {
            let suggested = check::suggest_brief(&self.w.nodes[&id]);
            self.mode = Mode::Input {
                title: format!(
                    "{id}: what it owns (paths or globs, comma-separated; empty: it only reads)"
                ),
                text: suggested.owns.join(", "),
                then: Then::Owns(id.clone()),
            };
        }
        id
    }

    pub fn connect(&mut self, from: &str, to: &str, on: &str) {
        let (from, to, on) = (from.to_string(), to.to_string(), on.to_string());
        self.edit(|w| {
            if let Some(n) = w
                .nodes
                .get_mut(&from)
                .filter(|n| n.is_agent() && !n.verdicts.contains(&on))
            {
                n.verdicts.push(on.clone());
            }
            w.edges.push(Edge {
                from,
                to,
                on: Triggers::One(on),
                guard: None,
                pass: BTreeMap::new(),
                ask: None,
            });
        });
    }

    /// The fixes for the selected node's problems, and the workflow's.
    pub fn fixes(&self) -> Vec<(String, FixOp)> {
        let here = |at: &At| match (at, &self.selected) {
            (At::Node(id), Some(s)) => id == s,
            (At::Node(_), None) => false,
            _ => true,
        };
        let mut found: Vec<&check::Problem> = self
            .report
            .problems
            .iter()
            .filter(|p| here(&p.at))
            .collect();
        found.sort_by_key(|p| (!matches!(p.at, At::Node(_)), p.level));
        found
            .iter()
            .flat_map(|p| {
                p.fixes
                    .iter()
                    .filter(|f| !matches!(f.op, FixOp::Manual))
                    .map(|f| (f.label.clone(), f.op.clone()))
            })
            .collect()
    }

    /// `f`: apply the first fix. False: there's none trix can make.
    pub fn fix(&mut self, i: usize) -> bool {
        let Some((label, op)) = self.fixes().into_iter().nth(i) else {
            return false;
        };
        if let FixOp::Probe(kind) = &op {
            self.message = format!("run: trix workflow capabilities --verify --kind {kind}");
            return false;
        }
        self.edit(|w| {
            check::apply(w, &op);
        });
        self.message = format!("fixed: {label}");
        true
    }

    /// `v`: a verifier for the selected writer: fresh context, read-only,
    /// on another backend than the writer's when one fits, its failures
    /// going back to the author.
    pub fn add_verifier(&mut self) -> Option<String> {
        let author = self.selected.clone().filter(|s| self.w.nodes[s].writes())?;
        let theirs = self.w.nodes[&author].agent.clone().unwrap_or_default();
        let kind = ["claude", "gemini", "codex"]
            .into_iter()
            .find(|k| *k != theirs && self.ctx.installed.0.contains_key(*k))
            .unwrap_or("claude");
        let id = (1..)
            .map(|i| {
                if i == 1 {
                    "verifier".to_string()
                } else {
                    format!("verifier-{i}")
                }
            })
            .find(|c| !self.w.nodes.contains_key(c))?;
        let verdict = self.w.nodes[&author]
            .verdicts
            .first()
            .cloned()
            .unwrap_or_else(|| "ready".into());
        let at = self
            .w
            .layout
            .get(&author)
            .map_or([26, 7], |p| [p[0] + 24, p[1] + 6]);
        let node = Node {
            kind: "agent".into(),
            preset: Some("verifier".into()),
            agent: Some(kind.into()),
            profile: Some(profile_for(Some(Role::Verifier), kind).into()),
            revisit: Some(Revisit::Fresh),
            verdicts: vec!["pass".into(), "changes".into()],
            output: BTreeMap::from([("findings".to_string(), "string".to_string())]),
            brief: Some(Brief {
                owns: vec![],
                must_not_touch: vec![],
                done: "Every must-fix finding has a proof, or there are none.".into(),
            }),
            ..Node::default()
        };
        let edge = |from: &str, to: &str, on: &str, pass: Option<&str>| Edge {
            from: from.into(),
            to: to.into(),
            on: Triggers::One(on.into()),
            guard: None,
            pass: pass
                .map(|p| BTreeMap::from([("inbox".to_string(), p.to_string())]))
                .unwrap_or_default(),
            ask: None,
        };
        let (vid, a) = (id.clone(), author.clone());
        self.edit(move |w| {
            // What the author led to now follows the verifier's pass.
            let after: Vec<String> = w
                .edges
                .iter()
                .filter(|e| e.from == a && e.on.list().contains(&verdict.as_str()))
                .map(|e| e.to.clone())
                .collect();
            w.edges
                .retain(|e| !(e.from == a && e.on.list().contains(&verdict.as_str())));
            w.nodes.insert(vid.clone(), node);
            w.layout.insert(vid.clone(), at);
            w.edges.push(edge(&a, &vid, &verdict, None));
            w.edges.push(edge(
                &vid,
                &a,
                "changes",
                Some(&format!("{{{{{vid}.findings}}}}")),
            ));
            for to in if after.is_empty() {
                vec![super::model::DONE.to_string()]
            } else {
                after
            } {
                w.edges.push(edge(&vid, &to, "pass", None));
            }
        });
        Some(id)
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(std::mem::replace(&mut self.w, prev));
        self.refresh();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.w, next));
        self.refresh();
        true
    }

    /// Every installed backend with how it fits the selected node's role,
    /// and why: those that fit first.
    pub fn backends(&self, id: &str) -> Vec<(String, Fit, String)> {
        let role = self.w.nodes[id].role();
        let mut out: Vec<(String, Fit, String)> = self
            .ctx
            .installed
            .0
            .keys()
            .filter(|k| self.m.get(k).is_some())
            .map(|kind| {
                let mut w = self.w.clone();
                let n = w.nodes.get_mut(id).unwrap();
                n.agent = Some(kind.clone());
                n.profile = Some(profile_for(role, kind).into());
                let r = check::check(&w, &self.m, &self.ctx);
                let at = At::Node(id.to_string());
                let worst = r
                    .at(&at)
                    .filter(|p| p.level != Severity::Info)
                    .min_by_key(|p| p.level);
                match worst {
                    Some(p) if p.level == Severity::Error => {
                        (kind.clone(), Fit::Cant, p.text.clone())
                    }
                    Some(p) => (kind.clone(), Fit::Limits, p.text.clone()),
                    None => (kind.clone(), Fit::Fits, "fits".to_string()),
                }
            })
            .collect();
        out.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        out
    }

    /// The inspector's needs-and-has panel: one row per capability the
    /// node's role needs, and what its backend and profile provide.
    pub fn needs_has(&self, id: &str) -> Vec<(String, String, bool)> {
        let n = &self.w.nodes[id];
        let Some(l) = self.report.labels.get(id) else {
            return match n.kind.as_str() {
                "check" => vec![(
                    "push".into(),
                    "guarded: it runs code the agents wrote".into(),
                    true,
                )],
                _ => Vec::new(),
            };
        };
        let level = match l.level {
            Level::Verified => "verified",
            Level::Documented => "documented",
            Level::Unverified => "unverified",
        };
        let mut rows = vec![
            (
                "backend".to_string(),
                format!("{} ({level})", l.kind),
                l.level.yes(),
            ),
            (
                "hand in".to_string(),
                if l.hands_in { "✓ yes" } else { "✗ no" }.to_string(),
                l.hands_in,
            ),
        ];
        if let Some(enforced) = l.read_only {
            rows.push((
                "read-only".into(),
                if enforced {
                    "✓ enforced"
                } else {
                    "⚠ by instruction"
                }
                .into(),
                enforced,
            ));
        }
        let wanted = self.w.policy.push;
        let ok = wanted != super::model::Push::GateOnly || l.protection == Protection::Enforced;
        rows.push((
            "blocks push".into(),
            format!("{} {}", if ok { "✓" } else { "✗" }, l.protection.label()),
            ok,
        ));
        if self
            .w
            .approvals
            .permissions
            .iter()
            .any(|r| r.node == "*" || r.node == id)
        {
            let ok = l.decision == Level::Verified;
            rows.push((
                "answers prompts".into(),
                format!("{} {}", if ok { "✓" } else { "✗" }, l.decision.label()),
                ok,
            ));
        }
        rows.push((
            "priced".into(),
            if l.priced { "✓ yes" } else { "? unknown" }.into(),
            l.priced,
        ));
        rows
    }

    /// Run and rehearse stay off while any error remains.
    pub fn can_run(&self) -> bool {
        self.report.runnable()
    }

    /// What launching would cost, as far as that can be known: said before
    /// every launch, and asked about when it's over `confirm_over`.
    pub fn launch_text(&self) -> String {
        let e = &self.report.estimate;
        let over = self
            .ctx
            .confirm_over
            .or(self.w.limits.budget_usd.as_ref().and_then(|b| b.get()))
            .unwrap_or(10.0);
        let confirm = if e.usd > over {
            format!(
                " That's over {}: press r again to launch anyway.",
                crate::cost::usd(over)
            )
        } else {
            String::new()
        };
        format!(
            "launch: at most {} at once, {} to {} cycles, cost {}.{confirm}",
            if e.peak_parallel == 1 {
                "1 agent".to_string()
            } else {
                format!("{} agents", e.peak_parallel)
            },
            e.min_cycles,
            e.ceiling,
            e.cost()
        )
    }

    fn step(&mut self, forward: bool) {
        let ids: Vec<&String> = self.w.nodes.keys().collect();
        if ids.is_empty() {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|s| ids.iter().position(|i| *i == s))
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % ids.len()
        } else {
            (at + ids.len() - 1) % ids.len()
        };
        self.selected = Some(ids[next].clone());
        self.sync();
    }

    fn nudge(&mut self, dx: i32, dy: i32) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.edit(|w| {
            let p = w.layout.entry(id).or_insert([2, 1]);
            *p = [(p[0] + dx).max(0), (p[1] + dy).max(0)];
        });
    }

    /// A key. Everything the builder does, a key does.
    pub fn key(&mut self, k: KeyEvent) -> Outcome {
        let mode = std::mem::replace(&mut self.mode, Mode::Canvas);
        match mode {
            Mode::Input {
                title,
                mut text,
                then,
            } => {
                match k.code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => self.typed(then, text),
                    KeyCode::Backspace => {
                        text.pop();
                        self.mode = Mode::Input { title, text, then };
                    }
                    KeyCode::Char(c) => {
                        text.push(c);
                        self.mode = Mode::Input { title, text, then };
                    }
                    _ => self.mode = Mode::Input { title, text, then },
                }
                Outcome::Stay
            }
            Mode::Pick {
                title,
                items,
                at,
                then,
            } => {
                match k.code {
                    KeyCode::Esc => {}
                    KeyCode::Up => {
                        self.mode = Mode::Pick {
                            title,
                            at: at.saturating_sub(1),
                            items,
                            then,
                        }
                    }
                    KeyCode::Down => {
                        self.mode = Mode::Pick {
                            title,
                            at: (at + 1).min(items.len().saturating_sub(1)),
                            items,
                            then,
                        }
                    }
                    KeyCode::Enter => {
                        self.picked(then, at, items.get(at).cloned().unwrap_or_default())
                    }
                    _ => {
                        self.mode = Mode::Pick {
                            title,
                            items,
                            at,
                            then,
                        }
                    }
                }
                Outcome::Stay
            }
            Mode::Inspect | Mode::Approvals | Mode::Keys => {
                if mode == Mode::Approvals && k.code == KeyCode::Char('m') {
                    let nodes: Vec<String> = self
                        .w
                        .nodes
                        .iter()
                        .filter(|(_, n)| n.is_agent() && !n.writes())
                        .map(|(id, _)| id.clone())
                        .collect();
                    self.mode = Mode::Pick {
                        title: "merge approver: only nodes that didn't write the change".into(),
                        items: nodes,
                        at: 0,
                        then: Pick::Approver,
                    };
                }
                Outcome::Stay
            }
            Mode::Canvas => self.canvas_key(k),
        }
    }

    fn canvas_key(&mut self, k: KeyEvent) -> Outcome {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.message.clear();
        match k.code {
            KeyCode::Char('q') => return Outcome::Quit,
            KeyCode::Char('s') => return Outcome::Save,
            KeyCode::Char('d') => return Outcome::DryRun,
            KeyCode::Char('r') if ctrl => {
                self.redo();
            }
            KeyCode::Char('r') => {
                if !self.can_run() {
                    self.message = format!(
                        "{} to fix first: run and rehearse stay off while any error remains",
                        self.report.errors()
                    );
                } else {
                    return Outcome::Run;
                }
            }
            KeyCode::Char('a') => {
                self.add("agent");
            }
            KeyCode::Char('k') => {
                self.add("check");
            }
            KeyCode::Char('j') => {
                self.add("judge");
            }
            KeyCode::Char('g') => {
                self.add("gh.gate");
            }
            KeyCode::Char('b') => {
                self.add("land");
            }
            KeyCode::Char('v') => {
                if self.add_verifier().is_none() {
                    self.message = "v adds a verifier for a writer: select one first".into();
                }
            }
            KeyCode::Char('c') => {
                if let Some(from) = self.selected.clone() {
                    let mut items: Vec<String> = self
                        .w
                        .nodes
                        .keys()
                        .filter(|id| **id != from)
                        .cloned()
                        .collect();
                    items.extend(["@done".to_string(), "@fail".to_string()]);
                    self.mode = Mode::Pick {
                        title: format!("connect {from} to"),
                        items,
                        at: 0,
                        then: Pick::ConnectTo(from),
                    };
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(id) = self.selected.clone() {
                    self.edit(|w| {
                        check::apply(w, &FixOp::RemoveNode(id));
                    });
                }
            }
            KeyCode::Char('f') => {
                if !self.fix(0) && self.message.is_empty() {
                    self.message = "nothing here trix can fix for you".into();
                }
            }
            KeyCode::Char('F') => {
                let items: Vec<String> = self.fixes().into_iter().map(|(l, _)| l).collect();
                if !items.is_empty() {
                    self.mode = Mode::Pick {
                        title: "fixes".into(),
                        items,
                        at: 0,
                        then: Pick::Fix(self.selected.clone().unwrap_or_default()),
                    };
                }
            }
            KeyCode::Char('L') => {
                self.undo.push(self.w.clone());
                self.place();
                self.dirty = true;
                self.refresh();
            }
            KeyCode::Char('u') => {
                self.undo();
            }
            KeyCode::Char('B') => {
                if let Some(id) = self.selected.clone().filter(|s| self.w.nodes[s].is_agent()) {
                    let items = self
                        .backends(&id)
                        .into_iter()
                        .map(|(k, fit, why)| format!("{} {k}: {why}", fit.mark()))
                        .collect();
                    self.mode = Mode::Pick {
                        title: format!("backend for {id}"),
                        items,
                        at: 0,
                        then: Pick::Backend(id),
                    };
                }
            }
            KeyCode::Char('e') => {
                if let Some(id) = self.selected.clone().filter(|s| self.w.nodes[s].is_agent()) {
                    let owns = self.w.nodes[&id]
                        .brief
                        .as_ref()
                        .map(|b| b.owns.join(", "))
                        .unwrap_or_default();
                    self.mode = Mode::Input {
                        title: format!("{id}: what it owns"),
                        text: owns,
                        then: Then::Owns(id),
                    };
                } else if let Some(id) = self
                    .selected
                    .clone()
                    .filter(|s| self.w.nodes[s].kind == "check")
                {
                    let run = shell_words::join(&self.w.nodes[&id].run);
                    self.mode = Mode::Input {
                        title: format!("{id}: the command it runs"),
                        text: run,
                        then: Then::Run(id),
                    };
                }
            }
            KeyCode::Char('A') => self.mode = Mode::Approvals,
            KeyCode::Char('?') => self.mode = Mode::Keys,
            KeyCode::Enter => self.mode = Mode::Inspect,
            KeyCode::Tab => self.step(true),
            KeyCode::BackTab => self.step(false),
            KeyCode::Left => self.nudge(if shift { -8 } else { -2 }, 0),
            KeyCode::Right => self.nudge(if shift { 8 } else { 2 }, 0),
            KeyCode::Up => self.nudge(0, if shift { -3 } else { -1 }),
            KeyCode::Down => self.nudge(0, if shift { 3 } else { 1 }),
            _ => {}
        }
        Outcome::Stay
    }

    fn typed(&mut self, then: Then, text: String) {
        let list = |t: &str| -> Vec<String> {
            t.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        };
        match then {
            // The brief, a line at a time: owns, must not touch, done.
            Then::Owns(id) => {
                let owns = list(&text);
                let reads = owns.is_empty();
                self.edit(|w| {
                    let n = w.nodes.get_mut(&id).unwrap();
                    n.brief.get_or_insert_with(Brief::default).owns = owns;
                    if reads && n.role == Some(Role::Implementer) {
                        n.role = Some(Role::Reviewer);
                    }
                });
                let had = self.w.nodes[&id]
                    .brief
                    .as_ref()
                    .map(|b| b.must_not_touch.join(", "))
                    .filter(|t| !t.is_empty());
                let text = had.unwrap_or_else(|| {
                    if reads {
                        String::new()
                    } else {
                        ".github/**".into()
                    }
                });
                self.mode = Mode::Input {
                    title: format!("{id}: what it must not touch"),
                    text,
                    then: Then::MustNot(id),
                };
            }
            Then::MustNot(id) => {
                self.edit(|w| {
                    w.nodes
                        .get_mut(&id)
                        .unwrap()
                        .brief
                        .get_or_insert_with(Brief::default)
                        .must_not_touch = list(&text)
                });
                let had = self.w.nodes[&id]
                    .brief
                    .as_ref()
                    .map(|b| b.done.clone())
                    .filter(|t| !t.is_empty());
                let text = had.unwrap_or_else(|| check::suggest_brief(&self.w.nodes[&id]).done);
                self.mode = Mode::Input {
                    title: format!("{id}: what done means"),
                    text,
                    then: Then::Done(id),
                };
            }
            Then::Done(id) => self.edit(|w| {
                w.nodes
                    .get_mut(&id)
                    .unwrap()
                    .brief
                    .get_or_insert_with(Brief::default)
                    .done = text.trim().to_string()
            }),
            Then::Trigger { from, to } => {
                if !text.trim().is_empty() {
                    self.connect(&from, &to, text.trim());
                }
            }
            Then::Run(id) => {
                if let Ok(argv) = shell_words::split(&text) {
                    self.edit(|w| w.nodes.get_mut(&id).unwrap().run = argv);
                }
            }
        }
    }

    fn picked(&mut self, then: Pick, at: usize, item: String) {
        match then {
            Pick::Backend(id) => {
                let Some((kind, fit, why)) = self.backends(&id).into_iter().nth(at) else {
                    return;
                };
                if fit == Fit::Cant {
                    self.message = format!("{kind} can't: {why}");
                    return;
                }
                let role = self.w.nodes[&id].role();
                self.edit(|w| {
                    let n = w.nodes.get_mut(&id).unwrap();
                    n.profile = Some(profile_for(role, &kind).into());
                    n.agent = Some(kind);
                });
            }
            Pick::ConnectTo(from) => {
                let verdicts = check::verdicts(&self.w.nodes[&from]);
                let unused = verdicts
                    .iter()
                    .find(|v| self.w.follow(&from, v).is_none())
                    .or(verdicts.first())
                    .cloned()
                    .unwrap_or_else(|| "ready".into());
                self.mode = Mode::Input {
                    title: format!("{from} → {item} on which verdict or event"),
                    text: unused,
                    then: Then::Trigger { from, to: item },
                };
            }
            Pick::Fix(_) => {
                self.fix(at);
            }
            Pick::Approver => self.edit(|w| {
                check::apply(w, &FixOp::SetApprover(Some(item)));
                w.policy.merge = super::model::Merge::Approver;
            }),
        }
    }

    /// A mouse event: on the canvas a click selects a node and a drag
    /// moves it; where it was dropped is kept.
    pub fn mouse(&mut self, ev: MouseEvent) {
        let a = self.area;
        if ev.column < a.x || ev.column >= a.x + a.width || ev.row < a.y || ev.row >= a.y + a.height
        {
            return;
        }
        self.canvas.mouse(ev);
        if let Some(id) = self.canvas.selected() {
            self.selected = Some(id);
        }
        let moved: Vec<(String, [i32; 2])> = self
            .canvas
            .positions()
            .into_iter()
            .filter(|(id, at)| self.w.layout.get(id) != Some(at))
            .collect();
        if !moved.is_empty() && matches!(ev.kind, crossterm::event::MouseEventKind::Up(_)) {
            self.undo.push(self.w.clone());
            for (id, at) in moved {
                self.w.layout.insert(id, [at[0].max(0), at[1].max(0)]);
            }
            self.dirty = true;
            self.refresh();
        }
    }

    // ── drawing ──

    fn colour(s: Severity) -> Color {
        match s {
            Severity::Error | Severity::Blocks => Color::Red,
            Severity::Warning => Color::Yellow,
            Severity::Info => Color::DarkGray,
        }
    }

    fn inspector(&self) -> Vec<Line<'static>> {
        let Some(id) = &self.selected else {
            return vec![Line::from("nothing selected")];
        };
        let n = &self.w.nodes[id];
        let mut lines = vec![
            Line::from(format!("role     {}", n.role().map_or("–", |r| r.name()))),
            Line::from(format!(
                "backend  {}",
                n.agent.as_deref().unwrap_or(&n.kind)
            )),
            Line::from(format!("profile  {}", n.profile.as_deref().unwrap_or("–"))),
        ];
        if let Some(b) = &n.brief {
            lines.push(Line::from(format!(
                "owns     {}",
                if b.owns.is_empty() {
                    "nothing".to_string()
                } else {
                    b.owns.join(",")
                }
            )));
        }
        lines.push(Line::from("── needs ───── has ──"));
        for (need, has, ok) in self.needs_has(id) {
            let style = if ok {
                Style::default()
            } else {
                Style::default().fg(Color::Yellow)
            };
            lines.push(Line::from(Span::styled(format!("{need:<14} {has}"), style)));
        }
        for p in self.report.at(&At::Node(id.clone())) {
            lines.push(Line::from(Span::styled(
                format!("{} {}", p.level.mark(), p.text),
                Style::default().fg(Builder::colour(p.level)),
            )));
            for (i, f) in p.fixes.iter().enumerate() {
                lines.push(Line::from(format!(
                    "  {} fix: {}",
                    if i == 0 { "f" } else { "F" },
                    f.label
                )));
            }
        }
        lines
    }

    fn approvals(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from("APPROVALS (m: pick the merge approver)")];
        match self
            .w
            .approvals
            .merge
            .as_ref()
            .and_then(|a| a.approver.as_ref())
        {
            Some(a) => lines.push(Line::from(format!(
                "⚡ merge: {a} approves, once done_when holds and CI is green on the checked commit"
            ))),
            None => lines.push(Line::from("merge: yours")),
        }
        for (i, r) in self.w.approvals.permissions.iter().enumerate() {
            let what = if r.tools.is_empty() {
                "any prompt".to_string()
            } else {
                r.tools.join(", ")
            };
            let mark = if r.mode == super::model::Allow::Allow {
                "⚡"
            } else {
                " "
            };
            lines.push(Line::from(format!(
                "{mark} rule {}: {} on {} → {}",
                i + 1,
                what,
                r.node,
                if r.mode == super::model::Allow::Allow {
                    "allow"
                } else {
                    "ask you"
                }
            )));
        }
        for p in self
            .report
            .problems
            .iter()
            .filter(|p| matches!(p.at, At::Rule(_) | At::Approvals))
        {
            lines.push(Line::from(Span::styled(
                format!("{} {}", p.level.mark(), p.text),
                Style::default().fg(Builder::colour(p.level)),
            )));
        }
        lines.push(Line::from("A permission rule works only where the backend's hook decision is verified for the installed version."));
        lines
    }

    /// Draw the builder. Under 100 columns the inspector is an overlay.
    pub fn draw(&mut self, f: &mut Frame) {
        let full = f.area();
        let wide = full.width >= 100;
        let bar = 4u16;
        let body = Rect {
            height: full.height.saturating_sub(bar + 1),
            y: full.y + 1,
            ..full
        };
        let (pal, insp) = (14u16.min(body.width / 4), if wide { 32u16 } else { 0 });
        let canvas = Rect {
            x: body.x + pal,
            width: body.width.saturating_sub(pal + insp),
            ..body
        };
        self.area = canvas;
        // The header: the name, the counts, and what a launch would cost.
        let e = &self.report.estimate;
        let costly = self.report.has("expensive-topology");
        let head = Line::from(vec![
            Span::raw(format!(
                " {} · builder{} ── ",
                self.w.name,
                if self.dirty { " *" } else { "" }
            )),
            Span::styled(
                format!(
                    "{} error{}",
                    self.report.errors(),
                    if self.report.errors() == 1 { "" } else { "s" }
                ),
                Style::default().fg(if self.report.errors() > 0 {
                    Color::Red
                } else {
                    Color::Reset
                }),
            ),
            Span::raw(" · "),
            Span::styled(
                format!(
                    "{} warning{}",
                    self.report.warnings(),
                    if self.report.warnings() == 1 { "" } else { "s" }
                ),
                Style::default().fg(if self.report.warnings() > 0 {
                    Color::Yellow
                } else {
                    Color::Reset
                }),
            ),
            Span::raw(" ── "),
            Span::styled(
                format!("peak {} · {}", e.peak_parallel, e.cost()),
                Style::default().fg(if costly { Color::Yellow } else { Color::Reset }),
            ),
        ]);
        f.render_widget(Paragraph::new(head), Rect { height: 1, ..full });
        let palette = "ADD\na agent\nk check\nj judge\ng gate\nb land\nv verifier\n\nTEMPLATES\ntriage\nissue-to-pr\nbug-repro\nnightly-deps";
        f.render_widget(
            Paragraph::new(palette).block(Block::default().borders(Borders::RIGHT)),
            Rect { width: pal, ..body },
        );
        self.canvas.draw(f, canvas);
        let title = self
            .selected
            .clone()
            .map_or("INSPECTOR".to_string(), |s| format!("INSPECTOR · {s}"));
        if wide {
            let area = Rect {
                x: canvas.x + canvas.width,
                width: insp,
                ..body
            };
            f.render_widget(
                Paragraph::new(self.inspector())
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::LEFT).title(title.clone())),
                area,
            );
        }
        // The problems bar: every error and warning in the workflow.
        let mut problems: Vec<Span> = Vec::new();
        for p in self
            .report
            .problems
            .iter()
            .filter(|p| p.level != Severity::Info)
        {
            let at = match &p.at {
                At::Node(id) => format!("{id}: "),
                _ => String::new(),
            };
            problems.push(Span::styled(
                format!("{} {at}{}   ", p.level.mark(), p.text),
                Style::default().fg(Builder::colour(p.level)),
            ));
        }
        let keys = if self.message.is_empty() {
            "a add  c connect  arrows move  tab next  enter inspect  B backend  e edit  f fix  v verifier  L layout  A approvals  d dry run  r run  u undo  s save  ? keys".to_string()
        } else {
            self.message.clone()
        };
        let bottom = Rect {
            y: full.y + full.height.saturating_sub(bar),
            height: bar,
            ..full
        };
        f.render_widget(
            Paragraph::new(vec![Line::from(problems), Line::from(keys)])
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::TOP)),
            bottom,
        );
        // Whatever is open on top.
        let overlay = |f: &mut Frame, title: String, lines: Vec<Line<'static>>, w: u16| {
            let w = w.min(full.width.saturating_sub(2));
            // As tall as its lines are once they wrap.
            let inner = w.saturating_sub(2).max(1) as usize;
            let rows: usize = lines.iter().map(|l| l.width().max(1).div_ceil(inner)).sum();
            let h = (rows as u16 + 2).min(full.height.saturating_sub(2));
            let area = Rect {
                x: full.x + full.width.saturating_sub(w + 1),
                y: full.y + 1,
                width: w,
                height: h,
            };
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title(title)),
                area,
            );
        };
        match &self.mode {
            Mode::Canvas => {}
            Mode::Inspect => overlay(f, title, self.inspector(), 44),
            Mode::Approvals => overlay(f, "APPROVALS".into(), self.approvals(), 70),
            Mode::Keys => {
                let keys = [
                    "a agent · k check · j judge · g gate · b land · v verifier for the selected writer",
                    "c connect · x delete · e edit the brief (or a check's command) · B backend, with each kind's fit",
                    "arrows move the node (shift: further) · tab / shift+tab select · enter inspect · L lay out",
                    "f apply the first fix · F list the fixes · A approvals · u undo · ctrl+r redo",
                    "d dry run · r run (off while any error remains) · s save · q quit",
                    "mouse: click selects, drag moves",
                ];
                overlay(
                    f,
                    "KEYS".into(),
                    keys.iter().map(|k| Line::from(*k)).collect(),
                    96,
                );
            }
            Mode::Input { title, text, .. } => overlay(
                f,
                title.clone(),
                vec![
                    Line::from(format!("{text}▏")),
                    Line::from("enter to keep · esc to leave it"),
                ],
                80,
            ),
            Mode::Pick {
                title, items, at, ..
            } => {
                let lines = items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| {
                        Line::from(Span::styled(
                            format!("{} {it}", if i == *at { "▶" } else { " " }),
                            if i == *at {
                                Style::default().add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            },
                        ))
                    })
                    .collect();
                overlay(f, title.clone(), lines, 90);
            }
        }
    }
}

// ───────────────────────────── the wizard ─────────────────────────────

/// What the wizard asks: a template, the goal, a backend per role, limits,
/// then supervised or autopilot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answers {
    pub template: String,
    pub name: String,
    pub goal: Option<String>,
    /// A backend for a node, by node.
    pub backends: BTreeMap<String, String>,
    pub wall_clock: Option<String>,
    pub budget: Option<f64>,
    pub autopilot: bool,
}

/// The workflow the wizard's answers make: the template under its new
/// name, with what you chose, and each backend given the profile its role
/// takes there.
pub fn wizard(a: &Answers, m: &Matrix, ctx: &Ctx) -> Result<Workflow, String> {
    let mut w = match a.template.as_str() {
        "blank" => Workflow {
            schema: super::model::SCHEMA,
            limits: super::model::Limits {
                wall_clock: Some("3h".into()),
                ..Default::default()
            },
            ..Workflow::default()
        }, // Triage's stages are its own: a copy of it couldn't run.
        "triage" => {
            return Err("triage isn't a template to copy: its stages run only as a triage case. Set its agents, rounds and budget under [triage] in your config, or start from issue-to-pr".into());
        }
        t => super::store::builtin(t).ok_or_else(|| {
            format!("'{t}' isn't a template (issue-to-pr, bug-repro, nightly-deps, blank)")
        })?,
    };
    w.name = a.name.clone();
    w.builtin = false;
    if let Some(goal) = a.goal.as_ref().filter(|g| !g.trim().is_empty()) {
        w.goal.text = goal.trim().to_string();
    }
    for (id, kind) in &a.backends {
        let n = w
            .nodes
            .get_mut(id)
            .ok_or_else(|| format!("'{id}' isn't a node of {}", a.template))?;
        if m.get(kind).is_none() {
            return Err(format!("'{kind}' isn't a backend trix knows"));
        }
        n.profile = Some(profile_for(n.role(), kind).into());
        n.agent = Some(kind.clone());
    }
    if let Some(t) = &a.wall_clock {
        super::model::duration(t)?;
        w.limits.wall_clock = Some(t.clone());
    }
    if let Some(b) = a.budget {
        w.limits.budget_usd = Some(super::model::Amount::Usd(b));
    }
    if a.autopilot {
        super::policy::autopilot(&mut w, m, ctx, "", "main");
    }
    Workflow::from_toml(&w.to_toml()?)
}
