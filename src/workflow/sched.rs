//! When runs start and stop on their own: cron starts, a deadline (no new
//! work starts, and the run parks) and a kill time (the run is aborted).
//! Opt-in units start the scheduler with no terminal: trix writes them, and
//! installs them only after you confirm.

use super::engine::WfRun;
use super::env::Env;
use super::policy::Stop;
use super::settings::clock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A workflow that starts on a schedule.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Schedule {
    pub workflow: String,
    /// Five fields: minute, hour, day of month, month, day of week.
    pub cron: String,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub kill_at: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    /// The repository it runs in.
    pub repo: PathBuf,
    /// The minute it last started in, so one minute starts one run.
    #[serde(default)]
    pub last: u64,
}

pub fn file() -> PathBuf {
    crate::worktree::state_dir().join("schedules.json")
}

pub fn load(file: &Path) -> Vec<Schedule> {
    std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(file: &Path, all: &[Schedule]) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    super::store::write_atomic(
        file,
        &serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?,
    )
}

/// One cron field against a value: `*`, `5`, `1-5`, `*/15`, `1,15`.
fn field(spec: &str, value: u32, min: u32, max: u32) -> Result<bool, String> {
    let bad = || format!("'{spec}' isn't a cron field ({min} to {max}, *, a-b, */n, a,b)");
    let mut hit = false;
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|s| *s > 0).ok_or_else(bad)?),
            None => (part, 1),
        };
        let (lo, hi) = match range {
            "*" => (min, max),
            r => match r.split_once('-') {
                Some((a, b)) => (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?),
                None => {
                    let n: u32 = r.parse().map_err(|_| bad())?;
                    (n, if part.contains('/') { max } else { n })
                }
            },
        };
        if lo < min || hi > max || lo > hi {
            return Err(bad());
        }
        hit |= value >= lo && value <= hi && (value - lo).is_multiple_of(step);
    }
    Ok(hit)
}

/// A moment, as cron sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Moment {
    pub minute: u32,
    pub hour: u32,
    pub day: u32,
    pub month: u32,
    /// 0 is Sunday.
    pub weekday: u32,
}

impl Moment {
    /// `at`, in the machine's own time.
    pub fn local(at: u64) -> Moment {
        let t = at as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&t, &mut tm) };
        Moment {
            minute: tm.tm_min as u32,
            hour: tm.tm_hour as u32,
            day: tm.tm_mday as u32,
            month: tm.tm_mon as u32 + 1,
            weekday: tm.tm_wday as u32,
        }
    }
}

/// Whether a cron expression names this moment.
pub fn matches(cron: &str, m: Moment) -> Result<bool, String> {
    let f: Vec<&str> = cron.split_whitespace().collect();
    let [min, hour, day, month, weekday] = f.as_slice() else {
        return Err(format!(
            "'{cron}' isn't a cron expression: five fields, minute hour day month weekday"
        ));
    };
    Ok(field(min, m.minute, 0, 59)?
        && field(hour, m.hour, 0, 23)?
        && field(day, m.day, 1, 31)?
        && field(month, m.month, 1, 12)?
        && field(weekday, m.weekday % 7, 0, 6)?)
}

/// The schedules due now: each starts once in a minute it names, however
/// often it's looked at.
pub fn due(all: &mut [Schedule], now: u64, at: Moment) -> Vec<Schedule> {
    let minute = now / 60;
    let mut out = Vec::new();
    for s in all.iter_mut() {
        if s.last != minute && matches(&s.cron, at).unwrap_or(false) {
            s.last = minute;
            out.push(s.clone());
        }
    }
    out
}

/// Seconds since local midnight at `at`.
fn day_secs(at: u64) -> u64 {
    crate::ledger::local(at).2 as u64
}

/// The first time after `from` that the clock reads `hhmm`.
pub fn next(from: u64, hhmm: &str) -> Option<u64> {
    let want = clock(hhmm).ok()? as u64 * 60;
    let have = day_secs(from);
    Some(from + (want + 86_400 - have) % 86_400)
}

/// A deadline or a kill time that has come. A deadline stops new work and
/// parks the run; a kill time aborts it. Both are times of day: the first
/// one after the run began.
pub fn time_of_day(run: &mut WfRun, env: &mut dyn Env) -> Option<Stop> {
    let now = env.now();
    let limits = &run.workflow.limits;
    if let Some(at) = limits
        .kill_at
        .as_deref()
        .and_then(|t| next(run.created, t))
        .filter(|at| now >= *at)
    {
        let _ = at;
        return Some(Stop::Kill(format!(
            "kill_at {}: the run is stopped",
            limits.kill_at.clone().unwrap_or_default()
        )));
    }
    if let Some(t) = limits.deadline.clone().filter(|_| !run.past_deadline)
        && next(run.created, &t).is_some_and(|at| now >= at)
    {
        run.past_deadline = true;
        return Some(Stop::Park(format!(
            "the deadline ({t}) has passed: no new work starts. trix workflow resume {} goes on anyway",
            run.id
        )));
    }
    None
}

/// The digest: what ended since `since`, one line a run.
pub fn digest(
    runs: &[super::view::RunView],
    ended: &BTreeMap<String, u64>,
    since: u64,
) -> Option<String> {
    let lines: Vec<String> = runs
        .iter()
        .filter(|r| {
            r.state.over()
                || matches!(
                    r.state,
                    super::view::RunState::Parked | super::view::RunState::NeedsYou
                )
        })
        .filter(|r| ended.get(&r.id).is_some_and(|at| *at >= since))
        .map(|r| {
            format!(
                "{} · {}{}",
                r.id,
                r.state.label(),
                if r.why.is_empty() {
                    String::new()
                } else {
                    format!(": {}", r.why)
                }
            )
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

// ───────────────────────────── units ─────────────────────────────

/// What starts the scheduler with no terminal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Unit {
    Systemd,
    Launchd,
}

impl Unit {
    pub fn parse(s: &str) -> Result<Unit, String> {
        match s {
            "systemd" => Ok(Unit::Systemd),
            "launchd" => Ok(Unit::Launchd),
            _ => Err(format!("'{s}' isn't a kind of unit: systemd or launchd")),
        }
    }

    /// Where it goes, under your home.
    pub fn path(self, home: &Path) -> PathBuf {
        match self {
            Unit::Systemd => home.join(".config/systemd/user/trix-workflows.service"),
            Unit::Launchd => home.join("Library/LaunchAgents/dev.trix.workflows.plist"),
        }
    }

    /// The unit's text: it runs `trix workflow scheduler`.
    pub fn text(self, exe: &Path) -> String {
        let exe = exe.display();
        match self {
            Unit::Systemd => format!(
                "[Unit]\nDescription=trix workflows: scheduled runs and triggers\n\n[Service]\nExecStart={exe} workflow scheduler\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n"
            ),
            Unit::Launchd => format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>dev.trix.workflows</string>\n  <key>ProgramArguments</key>\n  <array><string>{exe}</string><string>workflow</string><string>scheduler</string></array>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n</dict>\n</plist>\n"
            ),
        }
    }

    /// Write the unit: only when you confirmed. Without that, nothing is
    /// written, and what would be is said.
    pub fn install(self, home: &Path, exe: &Path, confirmed: bool) -> Result<String, String> {
        let path = self.path(home);
        let text = self.text(exe);
        if !confirmed {
            return Ok(format!(
                "{text}\nnot installed: this would be written to {}. Run it again with --yes to write it.\n",
                path.display()
            ));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        let then = match self {
            Unit::Systemd => "systemctl --user enable --now trix-workflows.service",
            Unit::Launchd => "launchctl load ~/Library/LaunchAgents/dev.trix.workflows.plist",
        };
        Ok(format!("wrote {}\nto start it: {then}\n", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: u32, hour: u32, day: u32, month: u32, weekday: u32) -> Moment {
        Moment {
            minute,
            hour,
            day,
            month,
            weekday,
        }
    }

    #[test]
    fn a_schedule_starts_once_in_each_minute_its_cron_names() {
        let two = at(0, 2, 14, 11, 6);
        assert_eq!(matches("0 2 * * *", two), Ok(true));
        assert_eq!(matches("0 2 * * *", at(1, 2, 14, 11, 6)), Ok(false));
        assert_eq!(matches("*/15 9-17 * * 1-5", at(30, 9, 3, 6, 3)), Ok(true));
        assert_eq!(
            matches("*/15 9-17 * * 1-5", at(30, 9, 3, 6, 0)),
            Ok(false),
            "not on a Sunday"
        );
        assert_eq!(matches("0,30 * 1 1 *", at(30, 5, 1, 1, 2)), Ok(true));
        assert_eq!(matches("5/20 * * * *", at(45, 0, 1, 1, 0)), Ok(true));
        for bad in [
            "0 2 * *",
            "60 2 * * *",
            "a b c d e",
            "*/0 * * * *",
            "5-1 * * * *",
        ] {
            assert!(matches(bad, two).is_err(), "{bad}");
        }
        let nightly = Schedule {
            workflow: "nightly-deps".into(),
            cron: "0 2 * * *".into(),
            deadline: Some("06:00".into()),
            kill_at: None,
            inputs: BTreeMap::new(),
            repo: "/r".into(),
            last: 0,
        };
        let mut all = vec![nightly];
        // Looked at every few seconds through 02:00: one start.
        let t = 1_795_000_020;
        assert_eq!(due(&mut all, t, two).len(), 1);
        assert_eq!(due(&mut all, t + 10, two).len(), 0);
        assert_eq!(due(&mut all, t + 30, two).len(), 0);
        // The next night: again.
        assert_eq!(due(&mut all, t + 86_400, two).len(), 1);
        assert_eq!(
            due(&mut all, t + 86_400 + 120, at(2, 2, 15, 11, 0)).len(),
            0
        );
        // Kept across a restart.
        let f =
            std::env::temp_dir().join(format!("trix-sched-{}/schedules.json", std::process::id()));
        save(&f, &all).unwrap();
        assert_eq!(load(&f), all);
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn units_arent_installed_without_confirmation() {
        let home = std::env::temp_dir().join(format!("trix-units-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let exe = Path::new("/opt/homebrew/bin/trix");
        for unit in [Unit::Systemd, Unit::Launchd] {
            // Not confirmed: the unit is shown, where it would go is said, nothing is written.
            let said = unit.install(&home, exe, false).unwrap();
            assert!(
                said.contains("workflow")
                    && said.contains("scheduler")
                    && said.contains("not installed"),
                "{said}"
            );
            assert!(said.contains(&unit.path(&home).display().to_string()));
            assert!(!unit.path(&home).exists() && !home.exists());
        }
        // Confirmed: written, and how to start it is said. Trix starts nothing itself.
        let said = Unit::Systemd.install(&home, exe, true).unwrap();
        let text = std::fs::read_to_string(Unit::Systemd.path(&home)).unwrap();
        assert!(
            text.contains("ExecStart=/opt/homebrew/bin/trix workflow scheduler")
                && said.contains("systemctl --user enable")
        );
        assert!(!Unit::Launchd.path(&home).exists());
        Unit::Launchd.install(&home, exe, true).unwrap();
        assert!(
            std::fs::read_to_string(Unit::Launchd.path(&home))
                .unwrap()
                .contains("<string>scheduler</string>")
        );
        assert!(Unit::parse("cron").is_err() && Unit::parse("launchd") == Ok(Unit::Launchd));
        let _ = std::fs::remove_dir_all(home);
    }
}
