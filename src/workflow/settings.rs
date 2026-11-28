//! What your `config.toml` says about workflows: `[workflows]`,
//! `[workflows.agents]`, `[approvals]`, `[capabilities.<kind>]` and
//! `[cost] confirm_over`.

use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FileWorkflows {
    ci_paths: Option<Vec<String>>,
    on_finish: Option<Vec<String>>,
    digest: Option<String>,
    agents: Option<FileAgents>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FileAgents {
    run: Option<String>,
    stop: Option<String>,
    resume: Option<String>,
    scope: Option<String>,
    max_depth: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FileApprovals {
    allow: Option<Vec<String>>,
    allow_sandboxed: Option<bool>,
    notify: Option<String>,
    floors: Option<Vec<String>>,
}

/// What an agent pane may do with workflows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum May {
    Allow,
    /// You confirm, in the run's pane.
    #[default]
    Ask,
    Deny,
}

impl May {
    fn parse(key: &str, s: Option<&str>) -> Result<May, String> {
        match s.map(str::trim) {
            None | Some("ask") => Ok(May::Ask),
            Some("allow") => Ok(May::Allow),
            Some("deny") => Ok(May::Deny),
            Some(x) => Err(format!(
                "workflows.agents.{key}: '{x}' is not allow, ask or deny"
            )),
        }
    }
}

/// `[workflows.agents]`. Never from an agent pane, whatever this says:
/// approving a gate, importing or trusting a file.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentPolicy {
    pub run: May,
    /// Pause and abort.
    pub stop: May,
    pub resume: May,
    /// Only runs this agent started (`own`), or any.
    pub own_only: bool,
    /// How deep a run started by a run started by an agent may go.
    pub max_depth: u32,
}

impl Default for AgentPolicy {
    fn default() -> AgentPolicy {
        AgentPolicy {
            run: May::Ask,
            stop: May::Ask,
            resume: May::Ask,
            own_only: true,
            max_depth: 2,
        }
    }
}

/// The floors that ship on and can't be taken off: they bind what trix
/// itself does.
pub const FLOORS: [&str; 3] = [
    "force_push",
    "merge_protected_without_ci_green_on_checked_commit",
    "push_ci_config_without_you",
];

/// Floors you or a workflow can add on top.
pub const MORE_FLOORS: [&str; 2] = ["merge_without_you", "push_without_you"];

/// Each floor is a known one. A workflow or a config can add floors, never
/// remove one: naming none still leaves the three.
pub fn floors(named: &[String]) -> Result<Vec<String>, String> {
    let mut all: Vec<String> = FLOORS.iter().map(|f| f.to_string()).collect();
    for f in named {
        if !FLOORS.contains(&f.as_str()) && !MORE_FLOORS.contains(&f.as_str()) {
            return Err(format!(
                "'{f}' isn't a floor: {}",
                FLOORS
                    .iter()
                    .chain(&MORE_FLOORS)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !all.contains(f) {
            all.push(f.clone());
        }
    }
    Ok(all)
}

/// `[approvals]`: the master opt-in for approver nodes and rule approvals.
#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalOpts {
    /// Workflows by name, pinned to their trusted content hash.
    pub allow: Vec<String>,
    pub allow_sandboxed: bool,
    /// Tell you of each approval as it's made, or only in the summary.
    pub notify_each: bool,
    pub floors: Vec<String>,
}

impl Default for ApprovalOpts {
    fn default() -> ApprovalOpts {
        ApprovalOpts {
            allow: Vec::new(),
            allow_sandboxed: false,
            notify_each: true,
            floors: floors(&[]).unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Settings {
    /// More paths that count as CI configuration.
    pub ci_paths: Vec<String>,
    /// A command run when a run ends, as argv.
    pub on_finish: Vec<String>,
    /// When the daily digest is sent: `08:00`.
    pub digest: Option<String>,
    pub agents: AgentPolicy,
    pub approvals: ApprovalOpts,
    /// `[capabilities.<kind>]` overrides of the shipped matrix.
    pub capabilities: toml::Table,
    /// `[cost] confirm_over`.
    pub confirm_over: Option<f64>,
}

/// `HH:MM`, as minutes since midnight.
pub fn clock(text: &str) -> Result<u32, String> {
    let bad = || format!("'{text}' isn't a time of day: 08:00, 23:30");
    let (h, m) = text.trim().split_once(':').ok_or_else(bad)?;
    let (h, m): (u32, u32) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok(h * 60 + m)
}

pub fn settings(
    w: FileWorkflows,
    a: FileApprovals,
    capabilities: toml::Table,
    confirm_over: Option<f64>,
) -> Result<Settings, String> {
    let fa = w.agents.unwrap_or_default();
    let agents = AgentPolicy {
        run: May::parse("run", fa.run.as_deref())?,
        stop: May::parse("stop", fa.stop.as_deref())?,
        resume: May::parse("resume", fa.resume.as_deref())?,
        own_only: match fa.scope.as_deref().map(str::trim) {
            None | Some("own") => true,
            Some("any") => false,
            Some(x) => return Err(format!("workflows.agents.scope: '{x}' is not own or any")),
        },
        max_depth: match fa.max_depth {
            None => 2,
            Some(n) if (1..=8).contains(&n) => n as u32,
            Some(n) => {
                return Err(format!(
                    "workflows.agents.max_depth: {n} is not from 1 to 8"
                ));
            }
        },
    };
    if let Some(d) = &w.digest {
        clock(d).map_err(|e| format!("workflows.digest: {e}"))?;
    }
    let approvals = ApprovalOpts {
        allow: a.allow.unwrap_or_default(),
        allow_sandboxed: a.allow_sandboxed.unwrap_or(false),
        notify_each: match a.notify.as_deref().map(str::trim) {
            None | Some("each") => true,
            Some("summary") => false,
            Some(x) => return Err(format!("approvals.notify: '{x}' is not each or summary")),
        },
        floors: floors(&a.floors.unwrap_or_default())
            .map_err(|e| format!("approvals.floors: {e}"))?,
    };
    match confirm_over {
        Some(x) if !(x.is_finite() && x > 0.0) => {
            return Err(format!(
                "cost.confirm_over: {x} is not an amount (USD, more than 0)"
            ));
        }
        _ => {}
    }
    super::caps::Matrix::load(&capabilities, &Default::default())?;
    Ok(Settings {
        ci_paths: w.ci_paths.unwrap_or_default(),
        on_finish: w.on_finish.unwrap_or_default(),
        digest: w.digest,
        agents,
        approvals,
        capabilities,
        confirm_over,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Result<Settings, String> {
        crate::config::parse(text, std::path::Path::new("/")).map(|c| c.workflows)
    }

    #[test]
    fn the_defaults_are_the_conservative_ones() {
        let s = read("").unwrap();
        // Agents ask before they start, stop or resume a run, and only their own.
        assert_eq!(
            s.agents,
            AgentPolicy {
                run: May::Ask,
                stop: May::Ask,
                resume: May::Ask,
                own_only: true,
                max_depth: 2
            }
        );
        // Approvals are off for every workflow, and for sandboxed agents.
        assert!(
            s.approvals.allow.is_empty() && !s.approvals.allow_sandboxed && s.approvals.notify_each
        );
        // All three floors are on.
        assert_eq!(s.approvals.floors, FLOORS);
        assert!(
            s.ci_paths.is_empty()
                && s.on_finish.is_empty()
                && s.digest.is_none()
                && s.confirm_over.is_none()
        );
    }

    #[test]
    fn every_key_is_read_and_checked() {
        let s = read(
            "[workflows]\nci_paths = [\"ci/**\"]\non_finish = [\"~/bin/notify-run\", \"{{run.id}}\"]\ndigest = \"08:00\"\n\
             [workflows.agents]\nrun = \"allow\"\nstop = \"deny\"\nresume = \"ask\"\nscope = \"any\"\nmax_depth = 3\n\
             [approvals]\nallow = [\"issue-to-pr\"]\nallow_sandboxed = true\nnotify = \"summary\"\nfloors = [\"merge_without_you\"]\n\
             [capabilities.codex]\nhook_timeout_s = 4\n[cost]\nconfirm_over = 25.0\n",
        )
        .unwrap();
        assert_eq!(
            s.agents,
            AgentPolicy {
                run: May::Allow,
                stop: May::Deny,
                resume: May::Ask,
                own_only: false,
                max_depth: 3
            }
        );
        assert_eq!(
            (s.ci_paths.as_slice(), s.digest.as_deref(), s.confirm_over),
            (&["ci/**".to_string()][..], Some("08:00"), Some(25.0))
        );
        assert_eq!(s.on_finish, ["~/bin/notify-run", "{{run.id}}"]);
        assert_eq!(s.approvals.allow, ["issue-to-pr"]);
        assert!(s.approvals.allow_sandboxed && !s.approvals.notify_each);
        // A floor can be added, never removed: the three are still there.
        assert_eq!(s.approvals.floors.len(), 4);
        assert!(
            FLOORS
                .iter()
                .all(|f| s.approvals.floors.iter().any(|x| x == f))
        );
        assert!(s.capabilities.contains_key("codex"));
        for (bad, says) in [
            (
                "[workflows.agents]\nrun = \"yes\"\n",
                "workflows.agents.run",
            ),
            (
                "[workflows.agents]\nscope = \"all\"\n",
                "workflows.agents.scope",
            ),
            (
                "[workflows.agents]\nmax_depth = 0\n",
                "workflows.agents.max_depth",
            ),
            ("[workflows]\ndigest = \"8am\"\n", "workflows.digest"),
            ("[approvals]\nnotify = \"never\"\n", "approvals.notify"),
            ("[approvals]\nfloors = [\"none\"]\n", "approvals.floors"),
            ("[capabilities.hal]\npriced = true\n", "capabilities.hal"),
            ("[cost]\nconfirm_over = 0\n", "cost.confirm_over"),
        ] {
            let e = read(bad).unwrap_err();
            assert!(e.contains(says), "{e}");
        }
        assert!(read("[workflows]\ncolour = 1\n").is_err());
        assert_eq!((clock("06:00"), clock("23:59")), (Ok(360), Ok(1439)));
        assert!(clock("24:00").is_err());
    }
}
