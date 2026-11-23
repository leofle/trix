//! How long a session has been left alone: when someone was last there and
//! when something last happened, in wall-clock time (`SystemTime`), so a
//! laptop closed overnight still counts the night. `std`'s `Instant` stops
//! while the machine sleeps.
//!
//! Two clocks, as tmux has: someone was there (a controller attached, or it
//! just left), and something happened (an agent working, input, a `trix
//! send` or `spawn`). Raw output doesn't count: TUIs redraw on their own.

use std::time::{Duration, SystemTime};

/// A session's times. Every method takes `now`, so tests need no sleeps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clock {
    pub started_at: SystemTime,
    pub attached_at: Option<SystemTime>,
    pub detached_at: Option<SystemTime>,
    pub active_at: SystemTime,
}

/// `now - then`, never negative: a clock that went back counts as zero.
fn since(now: SystemTime, then: SystemTime) -> Duration {
    now.duration_since(then).unwrap_or_default()
}

impl Clock {
    pub fn new(now: SystemTime) -> Clock {
        Clock { started_at: now, attached_at: None, detached_at: None, active_at: now }
    }

    pub fn attached(&mut self, now: SystemTime) {
        self.attached_at = Some(now);
    }

    pub fn detached(&mut self, now: SystemTime) {
        self.detached_at = Some(now);
    }

    /// Something happened: an agent working, input, `trix send`, `spawn`.
    pub fn active(&mut self, now: SystemTime) {
        self.active_at = self.active_at.max(now);
    }

    /// How long nobody and nothing has used it. `None` while a controller is
    /// attached: someone is there. The last someone-was-there is when the
    /// controller attached or, later, when it left.
    pub fn idle(&self, now: SystemTime, attached: bool) -> Option<Duration> {
        if attached {
            return None;
        }
        let last = [Some(self.active_at), self.attached_at, self.detached_at].into_iter().flatten().max().unwrap_or(self.started_at);
        Some(since(now, last))
    }

    /// How long since the controller left. `None` while one is attached, or
    /// when none ever was.
    pub fn detached_for(&self, now: SystemTime, attached: bool) -> Option<Duration> {
        if attached {
            return None;
        }
        self.detached_at.map(|d| since(now, d))
    }

    pub fn started_for(&self, now: SystemTime) -> Duration {
        since(now, self.started_at)
    }
}

/// `"8h"`, `"1d"`, `"90m"`, `"45s"`, or `"off"` (`None`).
pub fn parse_after(s: &str) -> Result<Option<Duration>, String> {
    let s = s.trim();
    if s == "off" {
        return Ok(None);
    }
    let bad = || format!("'{s}' is not a time like 30m, 8h or 1d, or off");
    let unit = s.chars().last().ok_or_else(bad)?;
    let n: u64 = s[..s.len() - unit.len_utf8()].parse().map_err(|_| bad())?;
    let secs = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86400,
        _ => return Err(bad()),
    };
    if n == 0 {
        return Err(bad());
    }
    Ok(Some(Duration::from_secs(n.saturating_mul(secs))))
}

/// Coarse, for "idle 27h": seconds, minutes, hours up to two days, then days.
pub fn coarse(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..172_800 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// What to type to reach `session`.
pub fn trix_for(session: &str) -> String {
    if session == crate::session::DEFAULT { "trix".into() } else { format!("trix -s {session}") }
}

/// `trix ls`'s hint for a session left alone longer than `after`, if it
/// was. `blocked`: its agents that need you, and for how long. An agent
/// needing you that long is said first: that's the worst of being forgotten.
pub fn hint(session: &str, idle: Option<Duration>, blocked: &[(String, Duration)], after: Option<Duration>) -> Vec<String> {
    let Some(after) = after else { return Vec::new() };
    let trix = trix_for(session);
    let mut out = Vec::new();
    let waiting: Vec<&(String, Duration)> = blocked.iter().filter(|(_, d)| *d > after).collect();
    if let Some(longest) = waiting.iter().map(|(_, d)| *d).max() {
        let names: Vec<&str> = waiting.iter().map(|(n, _)| n.as_str()).collect();
        let (has, it) = if names.len() == 1 { ("has", "it") } else { ("have", "them") };
        out.push(format!("! {} in session {session} {has} needed you for {}: `{trix}` to answer {it}", names.join(", "), coarse(longest)));
    }
    if let Some(idle) = idle.filter(|i| *i > after) {
        out.push(format!("session {session} has had nobody and nothing going on for {}: `{trix}` to look, `trix kill -s {session}` to stop it (a restore brings it back)", coarse(idle)));
    }
    out
}

/// One stopped session's saved workspace, for the footer.
#[derive(Debug, Clone, PartialEq)]
pub struct Stopped {
    pub session: String,
    pub agents: usize,
    /// Of their folders, how many are gone.
    pub gone: usize,
    pub age: Option<Duration>,
}

/// The sessions in `saved` (`state::saved_workspaces`) that aren't in
/// `running`, for the footer. `age`: how old a session's file is.
pub fn stopped(saved: &[(String, crate::state::Saved)], running: &[String], age: &dyn Fn(&str) -> Option<Duration>) -> Vec<Stopped> {
    saved
        .iter()
        .filter(|(s, _)| !running.contains(s))
        .map(|(s, w)| Stopped { session: s.clone(), agents: w.agents.len(), gone: w.agents.iter().filter(|a| !a.cwd.is_dir()).count(), age: age(s) })
        .collect()
}

/// `trix ls`'s footer: the stopped sessions that still have a saved
/// workspace, which keep their worktrees in use.
pub fn footer(stopped: &[Stopped]) -> Option<String> {
    if stopped.is_empty() {
        return None;
    }
    let each: Vec<String> = stopped
        .iter()
        .map(|s| {
            let mut bits = vec![format!("{} agent{}", s.agents, if s.agents == 1 { "" } else { "s" })];
            if let Some(age) = s.age {
                bits.push(format!("{} ago", coarse(age)));
            }
            if s.gone > 0 && s.gone == s.agents {
                bits.push(if s.agents == 1 { "its folder is gone".into() } else { format!("all {} folders gone", s.agents) });
            }
            format!("{} ({})", s.session, bits.join(", "))
        })
        .collect();
    let mut out = format!("stopped, with a saved workspace: {}\n", each.join(" · "));
    let first = &stopped[0].session;
    out.push_str(&format!("  `{}` brings one back; `trix forget -s {first}` drops its saved workspace, which keeps its worktrees in use\n", trix_for(first)));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: SystemTime = SystemTime::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        T0 + Duration::from_secs(secs)
    }

    #[test]
    fn idle_counts_from_the_last_of_someone_there_and_something_happening() {
        let mut c = Clock::new(at(0));
        assert_eq!(c.idle(at(100), false), Some(Duration::from_secs(100)), "never attached: since it started");
        c.attached(at(200));
        assert_eq!(c.idle(at(5000), true), None, "someone is there");
        assert_eq!(c.detached_for(at(5000), true), None);
        c.detached(at(1000));
        assert_eq!(c.idle(at(4600), false), Some(Duration::from_secs(3600)), "from when they left");
        assert_eq!(c.detached_for(at(4600), false), Some(Duration::from_secs(3600)));
        c.active(at(4000));
        assert_eq!(c.idle(at(4600), false), Some(Duration::from_secs(600)), "an agent worked since");
        c.active(at(3000));
        assert_eq!(c.active_at, at(4000), "never goes back");
        assert_eq!(c.idle(at(10), false), Some(Duration::ZERO), "a clock that went back is zero, not negative");
        assert_eq!(c.started_for(at(4600)), Duration::from_secs(4600));
    }

    #[test]
    fn idle_after_is_a_time_or_off() {
        assert_eq!(parse_after("1d"), Ok(Some(Duration::from_secs(86400))));
        assert_eq!(parse_after(" 8h "), Ok(Some(Duration::from_secs(8 * 3600))));
        assert_eq!(parse_after("90m"), Ok(Some(Duration::from_secs(5400))));
        assert_eq!(parse_after("off"), Ok(None));
        for bad in ["", "1", "d", "1w", "-1h", "0h", "1.5h", "eight"] {
            assert!(parse_after(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn times_are_said_coarsely() {
        assert_eq!(coarse(Duration::from_secs(42)), "42s");
        assert_eq!(coarse(Duration::from_secs(600)), "10m");
        assert_eq!(coarse(Duration::from_secs(27 * 3600 + 100)), "27h");
        assert_eq!(coarse(Duration::from_secs(7 * 86400)), "7d");
    }

    #[test]
    fn a_session_left_alone_past_idle_after_gets_a_hint() {
        let day = Some(Duration::from_secs(86400));
        let h = |n: u64| Duration::from_secs(n * 3600);
        assert!(hint("sidebar", Some(h(20)), &[], day).is_empty(), "not yet");
        assert!(hint("sidebar", Some(h(30)), &[], None).is_empty(), "off");
        assert!(hint("sidebar", None, &[], day).is_empty(), "someone is attached");
        let said = hint("sidebar", Some(h(27)), &[], day);
        assert_eq!(said, ["session sidebar has had nobody and nothing going on for 27h: `trix -s sidebar` to look, `trix kill -s sidebar` to stop it (a restore brings it back)"]);
        assert!(hint("default", Some(h(27)), &[], day)[0].contains("`trix` to look"));
        let said = hint("x", Some(h(30)), &[("tester".into(), h(27)), ("quick".into(), h(1))], day);
        assert_eq!(said.len(), 2);
        assert_eq!(said[0], "! tester in session x has needed you for 27h: `trix -s x` to answer it", "first, and only the one waiting that long");
        let said = hint("x", None, &[("a".into(), h(30)), ("b".into(), h(26))], day);
        assert_eq!(said, ["! a, b in session x have needed you for 30h: `trix -s x` to answer them"], "even with someone attached");
    }

    #[test]
    fn stopped_sessions_come_from_their_saved_files() {
        let dir = std::env::temp_dir().join(format!("trix-idle-stopped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let here = dir.display().to_string();
        let saved = |agents: &[(&str, &str)]| crate::state::Saved::new(0, agents.iter().map(|(n, c)| crate::state::SavedAgent { kind: "claude".into(), name: (*n).into(), cwd: c.into(), command: None, session: None, pinned: false, inherited: Vec::new(), family: None }).collect());
        std::fs::write(dir.join("workspace-sidebar.json"), serde_json::to_vec(&saved(&[("a", "/no/such/a"), ("b", "/no/such/b")])).unwrap()).unwrap();
        std::fs::write(dir.join("workspace-home.json"), serde_json::to_vec(&saved(&[("c", &here), ("d", "/no/such/d")])).unwrap()).unwrap();
        std::fs::write(dir.join("workspace.json"), serde_json::to_vec(&saved(&[("e", &here)])).unwrap()).unwrap();
        let all = crate::state::saved_workspaces_in(&dir);
        let got = stopped(&all, &["default".to_string()], &|s| (s == "home").then(|| Duration::from_secs(3600)));
        assert_eq!(got, [Stopped { session: "home".into(), agents: 2, gone: 1, age: Some(Duration::from_secs(3600)) }, Stopped { session: "sidebar".into(), agents: 2, gone: 2, age: None }], "the running one isn't stopped");
        assert!(footer(&got).unwrap().contains("sidebar (2 agents, all 2 folders gone)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_footer_names_stopped_sessions_and_what_they_keep() {
        assert_eq!(footer(&[]), None);
        let s = footer(&[
            Stopped { session: "home".into(), agents: 3, gone: 1, age: Some(Duration::from_secs(2 * 86400)) },
            Stopped { session: "sidebar".into(), agents: 4, gone: 4, age: Some(Duration::from_secs(7 * 86400)) },
        ])
        .unwrap();
        assert!(s.starts_with("stopped, with a saved workspace: home (3 agents, 2d ago) · sidebar (4 agents, 7d ago, all 4 folders gone)\n"), "{s}");
        assert!(s.contains("`trix forget -s home`"), "{s}");
        let one = footer(&[Stopped { session: "default".into(), agents: 1, gone: 1, age: None }]).unwrap();
        assert!(one.starts_with("stopped, with a saved workspace: default (1 agent, its folder is gone)\n") && one.contains("`trix` brings"), "{one}");
    }
}
