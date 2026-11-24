//! What trix's agents spent each month, kept on disk, so a month's cost can
//! be seen per model and held to a budget (`[cost] monthly_budget`).
//!
//! The usage watcher (`usage.rs`) records each turn's tokens as it reads
//! them, on the day the log line itself says, in local time. Tokens are
//! kept, not dollars: they are priced when shown, so a corrected price
//! reaches past months too.
//!
//! Nothing counts twice. For each log, the record remembers how far into it
//! was counted, so a restored agent reading its log again adds nothing. A log
//! not counted before counts only from when its agent started: a clone's log
//! opens with its original's conversation, and a resumed one with turns from
//! before trix, or this record, was there.
//!
//! - `<state>/spend/YYYY-MM.json`: `{"days": {"24": [[bucket, tokens], ...]}}`.
//! - `<state>/spend/logs.json`: each log's path, and the byte counted up to.
//!
//! Every trix session writes them, one at a time (`.lock`).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::usage::{Bucket, Spend, Tokens};

/// A turn's tokens, as a log has them.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Where its line ends in the log.
    pub end: u64,
    /// When it was logged, in Unix seconds.
    pub at: u64,
    pub bucket: Bucket,
    pub tokens: Tokens,
}

/// What one log read since the last look.
#[derive(Clone, Debug, PartialEq)]
pub struct Read {
    pub path: PathBuf,
    /// The end of its last whole line.
    pub done: u64,
    /// When its agent started, in Unix seconds: a log not counted before
    /// counts only from then.
    pub since: u64,
    pub entries: Vec<Entry>,
}

/// A month of the year, in local time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ym {
    pub year: i32,
    /// 1 to 12.
    pub month: u32,
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

impl Ym {
    pub fn now() -> Ym {
        local(crate::prices::now()).0
    }
    pub fn prev(self) -> Ym {
        if self.month == 1 { Ym { year: self.year - 1, month: 12 } } else { Ym { month: self.month - 1, ..self } }
    }
    pub fn next(self) -> Ym {
        if self.month == 12 { Ym { year: self.year + 1, month: 1 } } else { Ym { month: self.month + 1, ..self } }
    }
    pub fn days(self) -> u32 {
        match self.month {
            2 if (self.year % 4 == 0 && self.year % 100 != 0) || self.year % 400 == 0 => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        }
    }
    /// "November 2026".
    pub fn label(self) -> String {
        format!("{} {}", MONTHS[(self.month.clamp(1, 12) - 1) as usize], self.year)
    }
    /// "2026-11".
    pub fn key(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }
    pub fn parse(s: &str) -> Option<Ym> {
        let (y, m) = s.trim().split_once('-')?;
        let (year, month) = (y.parse().ok()?, m.parse().ok()?);
        (y.len() == 4 && (1..=12).contains(&month)).then_some(Ym { year, month })
    }
}

/// `at` (Unix seconds) in local time: its month, its day, and how many
/// seconds into that day.
pub fn local(at: u64) -> (Ym, u32, u32) {
    let t = at as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    let secs = (tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec).max(0) as u32;
    (Ym { year: tm.tm_year + 1900, month: (tm.tm_mon + 1) as u32 }, tm.tm_mday as u32, secs)
}

/// A log line's `timestamp` ("2026-11-24T22:35:41.654Z", or with an
/// offset like "+02:00") in Unix seconds.
pub fn parse_time(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r).filter(|d| d.bytes().all(|c| c.is_ascii_digit())).and_then(|d| d.parse().ok()) };
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d, h, mi, sec) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // The zone, after any fraction of a second.
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    let offset = match b.get(i) {
        None | Some(b'Z' | b'z') => 0,
        Some(&c @ (b'+' | b'-')) => {
            let (oh, om) = (num(i + 1..i + 3)?, num(i + 4..i + 6)?);
            (oh * 3600 + om * 60) * if c == b'-' { -1 } else { 1 }
        }
        _ => return None,
    };
    // Days since 1970-01-01, from the civil date (Howard Hinnant's).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + sec - offset).ok()
}

/// A month's spend, day by day.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Month {
    pub days: BTreeMap<u32, Spend>,
}

impl Month {
    pub fn total(&self) -> Spend {
        let mut s = Spend::new();
        for d in self.days.values() {
            crate::usage::merge(&mut s, d);
        }
        s
    }

    /// Each model's spend, every kind of its calls together.
    pub fn by_model(&self) -> BTreeMap<String, Spend> {
        let mut out: BTreeMap<String, Spend> = BTreeMap::new();
        for d in self.days.values() {
            for (b, t) in d {
                crate::usage::merge(out.entry(b.model.clone()).or_default(), &Spend::from([(b.clone(), *t)]));
            }
        }
        out
    }

    /// What the month will have cost by its end, if it goes on as it has
    /// since its first day with spend: `None` until there's a day of it to
    /// go on, or for a month that isn't this one (`today`, and how far
    /// into it).
    pub fn pace(&self, usd: f64, ym: Ym, today: (Ym, u32, u32)) -> Option<f64> {
        let (now, day, secs) = today;
        let first = *self.days.keys().next()?;
        if now != ym || day < first {
            return None;
        }
        let into = f64::from(secs) / 86_400.0;
        let elapsed = f64::from(day - first) + into;
        let left = f64::from(ym.days().saturating_sub(day)) + (1.0 - into);
        (elapsed >= 1.0).then(|| usd + usd / elapsed * left)
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct MonthFile {
    #[serde(default)]
    days: BTreeMap<String, Vec<(Bucket, Tokens)>>,
}

/// Where the record is kept.
pub fn dir() -> PathBuf {
    crate::worktree::state_dir().join("spend")
}

fn month_path(dir: &Path, ym: Ym) -> PathBuf {
    dir.join(format!("{}.json", ym.key()))
}

/// A month as recorded: empty if nothing was.
pub fn load(dir: &Path, ym: Ym) -> Month {
    let file: MonthFile = std::fs::read(month_path(dir, ym)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let days = file.days.into_iter().filter_map(|(d, list)| Some((d.parse().ok()?, list.into_iter().collect()))).collect();
    Month { days }
}

/// When a month's file last changed, to see another session's writes.
pub fn changed(dir: &Path, ym: Ym) -> Option<std::time::SystemTime> {
    std::fs::metadata(month_path(dir, ym)).and_then(|m| m.modified()).ok()
}

fn store_month(dir: &Path, ym: Ym, m: &Month) -> std::io::Result<()> {
    let days = m.days.iter().map(|(d, s)| (d.to_string(), crate::usage::to_list(s))).collect();
    write_atomic(&month_path(dir, ym), &serde_json::to_vec(&MonthFile { days })?)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Holds the record's lock until dropped.
struct Lock(std::fs::File);

impl Lock {
    fn take(dir: &Path) -> std::io::Result<Lock> {
        use std::os::fd::AsRawFd;
        let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(".lock"))?;
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Lock(f))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Add what `reads` hold that wasn't counted yet. Returns the months that
/// changed.
pub fn record(dir: &Path, reads: &[Read]) -> std::io::Result<Vec<Ym>> {
    if reads.iter().all(|r| r.entries.is_empty()) {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(dir)?;
    let _lock = Lock::take(dir)?;
    let logs_path = dir.join("logs.json");
    let mut logs: BTreeMap<String, u64> = std::fs::read(&logs_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut months: BTreeMap<Ym, Month> = BTreeMap::new();
    let mut touched = false;
    for r in reads.iter().filter(|r| !r.entries.is_empty()) {
        let key = r.path.display().to_string();
        // A log shorter than what was counted was written anew.
        let counted = logs.get(&key).copied().filter(|&c| c <= r.done);
        let fresh = r.entries.iter().filter(|e| match counted {
            Some(c) => e.end > c,
            None => e.at >= r.since,
        });
        let mut any = false;
        for e in fresh {
            let (ym, day, _) = local(e.at);
            let m = months.entry(ym).or_insert_with(|| load(dir, ym));
            crate::usage::merge(m.days.entry(day).or_default(), &Spend::from([(e.bucket.clone(), e.tokens)]));
            any = true;
        }
        if any || counted.is_some() {
            logs.insert(key, r.done);
            touched = true;
        }
    }
    for (ym, m) in &months {
        store_month(dir, *ym, m)?;
    }
    if touched {
        write_atomic(&logs_path, &serde_json::to_vec(&logs)?)?;
    }
    Ok(months.into_keys().collect())
}

/// Forget logs that are gone: the record of how far each was counted
/// would otherwise only grow.
pub fn prune(dir: &Path) -> std::io::Result<()> {
    let logs_path = dir.join("logs.json");
    if !logs_path.exists() {
        return Ok(());
    }
    let _lock = Lock::take(dir)?;
    let mut logs: BTreeMap<String, u64> = std::fs::read(&logs_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let before = logs.len();
    logs.retain(|p, _| Path::new(p).exists());
    if logs.len() != before {
        write_atomic(&logs_path, &serde_json::to_vec(&logs)?)?;
    }
    Ok(())
}

/// A month as `trix spend --json` and `trix ls --json` give it.
#[derive(serde::Serialize, Debug)]
pub struct Summary {
    /// "2026-11".
    pub month: String,
    pub total_tokens: u64,
    /// At list prices; for a subscription, what the same work would cost.
    pub cost_usd: f64,
    /// False when some tokens are from a model with no price.
    pub cost_complete: bool,
    /// This month only: what today has cost so far.
    pub today_usd: Option<f64>,
    /// This month only: what it will have cost at its end, at its pace so far.
    pub pace_usd: Option<f64>,
    pub budget_usd: Option<f64>,
    pub over_budget: bool,
    /// Costliest first.
    pub models: Vec<ModelSummary>,
}

#[derive(serde::Serialize, Debug)]
pub struct ModelSummary {
    pub model: String,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub cost_complete: bool,
}

fn cents4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// `month` (`ym`), priced by `price`. The budget counts for this month only.
pub fn summary(month: &Month, ym: Ym, price: &dyn Fn(&Spend) -> crate::cost::Cost, budget: Option<f64>) -> Summary {
    let today = local(crate::prices::now());
    let current = today.0 == ym;
    let total = month.total();
    let c = price(&total);
    let mut models: Vec<ModelSummary> = month
        .by_model()
        .into_iter()
        .map(|(model, s)| {
            let c = price(&s);
            ModelSummary { model, total_tokens: crate::usage::total(&s).total(), cost_usd: cents4(c.usd), cost_complete: c.unpriced == 0 }
        })
        .collect();
    models.sort_by(|a, b| b.cost_usd.total_cmp(&a.cost_usd).then(b.total_tokens.cmp(&a.total_tokens)));
    let budget = budget.filter(|_| current);
    Summary {
        month: ym.key(),
        total_tokens: crate::usage::total(&total).total(),
        cost_usd: cents4(c.usd),
        cost_complete: c.unpriced == 0,
        today_usd: current.then(|| cents4(month.days.get(&today.1).map_or(0.0, |s| price(s).usd))),
        pace_usd: month.pace(c.usd, ym, today).map(cents4),
        budget_usd: budget,
        over_budget: budget.is_some_and(|b| c.usd >= b),
        models,
    }
}

const SPEND_HELP: &str = "usage: trix spend [YYYY-MM] [--json]\n  what trix's agents spent in a month (this one by default), per model\n";

/// `trix spend [YYYY-MM] [--json]`: a month's spend per model, read from the
/// record. No server needed.
pub fn run(args: &[String]) -> i32 {
    let mut json = false;
    let mut ym = None;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                print!("{SPEND_HELP}");
                return 0;
            }
            s => match Ym::parse(s) {
                Some(m) if ym.is_none() => ym = Some(m),
                _ => {
                    eprint!("trix: spend: '{s}' isn't a month like 2026-11\n{SPEND_HELP}");
                    return 2;
                }
            },
        }
    }
    let ym = ym.unwrap_or_else(Ym::now);
    let config = crate::config::load().unwrap_or_else(|e| {
        eprintln!("trix: {e} (using the default prices)");
        Default::default()
    });
    let cost = &config.cost;
    let live = if cost.live_prices { crate::prices::load(&crate::prices::cache_path()) } else { None };
    let price = |s: &Spend| crate::cost::cost(s, &cost.prices, live.as_ref());
    let dir = dir();
    let month = load(&dir, ym);
    let sum = summary(&month, ym, &price, cost.monthly_budget);
    if json {
        println!("{}", serde_json::to_string_pretty(&sum).unwrap_or_default());
        return 0;
    }
    print!("{}", text(&sum, ym, &load(&dir, ym.prev()), &price));
    0
}

fn text(sum: &Summary, ym: Ym, before: &Month, price: &dyn Fn(&Spend) -> crate::cost::Cost) -> String {
    let today = local(crate::prices::now());
    let usd = |x: f64, complete: bool| format!("{}{}", crate::cost::usd(x), if complete { "" } else { "+" });
    let mut out = format!("{}{}\n", ym.label(), if today.0 == ym { format!(" · day {} of {}", today.1, ym.days()) } else { String::new() });
    if sum.models.is_empty() {
        out.push_str("  nothing spent\n");
    }
    let nw = sum.models.iter().map(|m| m.model.len()).max().unwrap_or(0).max(5);
    for m in &sum.models {
        let share = if sum.cost_usd > 0.0 && m.cost_usd > 0.0 { format!("{:>5}", format!("{}%", (m.cost_usd / sum.cost_usd * 100.0).round() as u64)) } else { String::new() };
        let c = if !m.cost_complete && m.cost_usd == 0.0 { "no price".to_string() } else { usd(m.cost_usd, m.cost_complete) };
        out.push_str(&format!("  {:<nw$}  {:>6}  {:>9}  {share}\n", m.model, crate::usage::tokens(m.total_tokens), c));
    }
    if !sum.models.is_empty() {
        out.push_str(&format!("  {:<nw$}  {:>6}  {:>9}\n", "total", crate::usage::tokens(sum.total_tokens), usd(sum.cost_usd, sum.cost_complete)));
    }
    let mut facts = Vec::new();
    if let Some(t) = sum.today_usd {
        facts.push(format!("today {}", crate::cost::usd(t)));
    }
    if !before.days.is_empty() {
        let c = price(&before.total());
        facts.push(format!("{} {}", if today.0 == ym { "last month" } else { "month before" }, usd(c.usd, c.unpriced == 0)));
    }
    if let Some(p) = sum.pace_usd {
        facts.push(format!("on pace for ~{}", crate::cost::usd(p)));
    }
    if let Some(b) = sum.budget_usd {
        facts.push(format!("{}% of the {} budget", (sum.cost_usd / b * 100.0).round() as u64, crate::cost::usd(b)));
    }
    if !facts.is_empty() {
        out.push_str(&format!("  {}\n", facts.join(" · ")));
    }
    out.push_str("At list prices: on a subscription, not what you're billed.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("trix-ledger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }
    fn entry(end: u64, at: u64, model: &str, output: u64) -> Entry {
        Entry { end, at, bucket: Bucket { model: model.into(), fast: false, prompt: 0 }, tokens: Tokens { output, ..Default::default() } }
    }
    fn read(path: &str, done: u64, since: u64, entries: Vec<Entry>) -> Read {
        Read { path: path.into(), done, since, entries }
    }
    fn output(m: &Month) -> u64 {
        crate::usage::total(&m.total()).output
    }

    #[test]
    fn timestamps_read_as_unix_seconds() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-11-24T22:35:41.654Z"), Some(1_795_559_741));
        assert_eq!(parse_time("2026-11-25T00:35:41+02:00"), Some(1_795_559_741));
        assert_eq!(parse_time("2024-02-29T12:00:00Z"), Some(1_709_208_000), "a leap day");
        for bad in ["", "yesterday", "2026-13-01T00:00:00Z", "2026-11-24", "2026-11-24T22:35:41Q"] {
            assert_eq!(parse_time(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn months_step_and_name() {
        let nov = Ym { year: 2026, month: 11 };
        assert_eq!((nov.next().next(), nov.prev()), (Ym { year: 2027, month: 1 }, Ym { year: 2026, month: 10 }));
        assert_eq!((nov.label(), nov.key(), Ym::parse("2026-11")), ("November 2026".to_string(), "2026-11".to_string(), Some(nov)));
        assert_eq!((Ym::parse("2026-13"), Ym::parse("26-11"), Ym::parse("nov")), (None, None, None));
        assert_eq!((Ym { year: 2024, month: 2 }.days(), Ym { year: 2026, month: 2 }.days(), nov.days()), (29, 28, 30));
    }

    #[test]
    fn a_log_counts_once_however_often_it_is_read() {
        let d = dir("once");
        let at = parse_time("2026-11-24T12:00:00Z").unwrap();
        let (ym, _, _) = local(at);
        let all = vec![entry(100, at, "m", 10), entry(200, at + 60, "m", 5)];
        assert_eq!(record(&d, &[read("/l.jsonl", 200, 0, all.clone())]).unwrap(), vec![ym]);
        assert_eq!(output(&load(&d, ym)), 15);
        // Read again from the start (a restored agent): nothing new.
        record(&d, &[read("/l.jsonl", 200, 0, all.clone())]).unwrap();
        assert_eq!(output(&load(&d, ym)), 15);
        // Read on: only what's past the counted byte.
        let mut more = all;
        more.push(entry(300, at + 120, "m", 7));
        record(&d, &[read("/l.jsonl", 300, 0, more)]).unwrap();
        assert_eq!(output(&load(&d, ym)), 22);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_log_counts_from_when_its_agent_started() {
        let d = dir("since");
        let start = parse_time("2026-11-24T12:00:00Z").unwrap();
        let (ym, _, _) = local(start);
        // A clone: its log opens with the original's turns, from before it started.
        let copied = vec![entry(100, start - 3600, "m", 50), entry(200, start - 60, "m", 50), entry(300, start + 30, "m", 4)];
        record(&d, &[read("/clone.jsonl", 300, start, copied)]).unwrap();
        assert_eq!(output(&load(&d, ym)), 4, "only the clone's own turn");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn spend_goes_to_the_day_and_month_it_was_logged() {
        let d = dir("days");
        let a = parse_time("2026-10-31T12:00:00Z").unwrap();
        let b = parse_time("2026-11-02T12:00:00Z").unwrap();
        let changed = record(&d, &[read("/x.jsonl", 200, 0, vec![entry(100, a, "opus", 3), entry(200, b, "sonnet", 4)])]).unwrap();
        let ((oct, da, _), (nov, db, _)) = (local(a), local(b));
        assert_eq!(changed, vec![oct, nov]);
        let (o, n) = (load(&d, oct), load(&d, nov));
        assert_eq!((o.days.keys().copied().collect::<Vec<_>>(), n.days.keys().copied().collect::<Vec<_>>()), (vec![da], vec![db]));
        assert_eq!(n.by_model().keys().cloned().collect::<Vec<_>>(), vec!["sonnet".to_string()]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_pace_runs_from_the_first_day_with_spend() {
        let nov = Ym { year: 2026, month: 11 };
        let m = Month { days: BTreeMap::from([(11, Spend::new())]) };
        // From the 11th to noon on the 21st: 10.5 days for $105, 9.5 left.
        let p = m.pace(105.0, nov, (nov, 21, 43_200)).unwrap();
        assert!((p - 200.0).abs() < 1e-9, "{p}");
        assert_eq!(m.pace(5.0, nov, (nov, 11, 3600)), None, "not a day of it yet");
        assert_eq!(m.pace(5.0, nov, (nov.next(), 3, 0)), None, "a month gone by");
        assert_eq!(Month::default().pace(5.0, nov, (nov, 21, 0)), None);
    }
}
