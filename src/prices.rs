//! Live prices: LiteLLM's public list of model prices, fetched about once a
//! day in the background and kept on disk, so new models and price changes
//! reach trix without a new release.
//!
//! - The list is USD per token, by model; trix keeps Anthropic's and
//!   OpenAI's own entries (not Bedrock's, Azure's or a reseller's) and files
//!   them by `cost::canon`, so `anthropic/claude-opus-5-5` and a dated id
//!   meet the name an agent logs.
//! - The cache is `$XDG_CACHE_HOME/trix/prices.json` (`~/.cache/trix`): the
//!   prices trix read from the list, the URL and when. A download that fails
//!   or doesn't read as a price list leaves the last good one in place.
//! - The fetch runs `curl`, with a timeout, on a thread of its own: the UI
//!   never waits for it. `[cost] live_prices = false` stops it, and trix
//!   then uses only config.toml's prices and its own.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::app::AppEvent;
use crate::cost::{Price, Tiered, canon};

pub const DEFAULT_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
/// How old the list may get before it's fetched again.
const DAY: u64 = 24 * 60 * 60;
/// How soon to try again after a fetch failed.
const RETRY: u64 = 60 * 60;
/// How long a fetch may take.
const TIMEOUT: u64 = 30;
/// Fewer models than this isn't the list (an error page, `{}`, a truncated file).
const AT_LEAST: usize = 10;

/// The live list: each model's prices, by `cost::canon` name.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Live {
    pub url: String,
    /// Unix seconds.
    pub fetched_at: u64,
    pub models: HashMap<String, Tiered>,
}

impl Live {
    pub fn get(&self, canon_name: &str) -> Option<&Tiered> {
        self.models.get(canon_name)
    }

    #[cfg(test)]
    pub fn from_models(url: &str, fetched_at: u64, models: impl IntoIterator<Item = (String, Tiered)>) -> Live {
        Live { url: url.into(), fetched_at, models: models.into_iter().collect() }
    }
}

/// What the fetcher knows, for `trix ls --json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feed {
    pub live: Option<Arc<Live>>,
    /// The last fetch's failure, until one succeeds.
    pub error: Option<String>,
}

/// Whether to fetch, and from where: the config's, kept up to date by the app.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub url: String,
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub fn cache_path() -> PathBuf {
    crate::state::xdg_dir("XDG_CACHE_HOME", ".cache").join("trix/prices.json")
}

// ───────────────────────────── reading the list ─────────────────────────────

/// The rates LiteLLM gives, by its key's name (without `_above_Nk_tokens`).
const RATES: [&str; 5] = ["input_cost_per_token", "output_cost_per_token", "cache_read_input_token_cost", "cache_creation_input_token_cost", "cache_creation_input_token_cost_above_1hr"];

/// One entry's prices, per million tokens: its own rates, and those above
/// each prompt size it names. Rates it leaves out follow the provider's
/// rules: a cache write is input (Anthropic: 1.25× for 5 minutes, 2× for an
/// hour), and an uncached read is input.
fn entry(v: &serde_json::Map<String, Value>, anthropic: bool) -> Option<Tiered> {
    // tier (0: every prompt) → [input, output, read, write, write 1h]
    let mut tiers: std::collections::BTreeMap<u32, [Option<f64>; 5]> = Default::default();
    for (k, val) in v {
        let Some(x) = val.as_f64().filter(|x| x.is_finite() && *x >= 0.0) else { continue };
        let (name, tier) = match k.rsplit_once("_above_").and_then(|(n, t)| Some((n, t.strip_suffix("k_tokens")?.parse::<u32>().ok()?))) {
            Some((n, t)) => (n, t),
            None => (k.as_str(), 0),
        };
        if let Some(i) = RATES.iter().position(|r| *r == name) {
            // Per million, without the float noise per token leaves (1e-7 → 0.1).
            tiers.entry(tier).or_default()[i] = Some((x * 1e15).round() / 1e9);
        }
    }
    let rates = |r: &[Option<f64>; 5], under: Option<&Price>| -> Option<Price> {
        let input = r[0].or(under.map(|p| p.input))?;
        let output = r[1].or(under.map(|p| p.output))?;
        let (w5, w1) = if anthropic { (input * 1.25, input * 2.0) } else { (input, input) };
        let cache_write = r[3].unwrap_or(w5);
        Some(Price { input, output, cache_read: r[2].unwrap_or(input), cache_write, cache_write_1h: r[4].unwrap_or(if anthropic { w1 } else { cache_write }) })
    };
    let base = rates(tiers.get(&0)?, None)?;
    let above = tiers.iter().filter(|(t, _)| **t > 0).filter_map(|(t, r)| Some((*t, rates(r, Some(&base))?))).collect();
    Some(Tiered { base, above })
}

/// LiteLLM's list, as trix keeps it. Where several entries file under one
/// name, the undated, unprefixed one wins (`claude-opus-4-5` over
/// `anthropic/claude-opus-4-5-20251101`).
pub fn parse(bytes: &[u8], url: &str, fetched_at: u64) -> Result<Live, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("not a price list: {e}"))?;
    let all = v.as_object().ok_or("not a price list: not a JSON object")?;
    let mut models: HashMap<String, (usize, Tiered)> = HashMap::new();
    for (key, e) in all {
        let Some(e) = e.as_object() else { continue };
        let anthropic = match e.get("litellm_provider").and_then(Value::as_str) {
            Some("anthropic") => true,
            Some("openai") => false,
            _ => continue,
        };
        let Some(t) = entry(e, anthropic) else { continue };
        let name = canon(key);
        let rank = key.contains('/') as usize * 2 + (key.to_ascii_lowercase() != name) as usize;
        if models.get(&name).is_none_or(|(r, _)| rank < *r) {
            models.insert(name, (rank, t));
        }
    }
    if models.len() < AT_LEAST {
        return Err(format!("not a price list: {} Anthropic or OpenAI models in it", models.len()));
    }
    Ok(Live { url: url.into(), fetched_at, models: models.into_iter().map(|(k, (_, t))| (k, t)).collect() })
}

// ───────────────────────────── the cache ─────────────────────────────

pub fn load(path: &Path) -> Option<Live> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok().filter(|l: &Live| !l.models.is_empty())
}

/// Written whole or not at all: a crash halfway leaves the last one.
fn store(path: &Path, live: &Live) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder for the cache")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(live).unwrap_or_default()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Time to fetch: no list, a list from another URL, or one a day old, but
/// not within an hour of a failed try at the same URL.
pub fn due(live: Option<&Live>, url: &str, now: u64, tried: Option<&(String, u64)>) -> bool {
    let stale = live.is_none_or(|l| l.url != url || now.saturating_sub(l.fetched_at) >= DAY);
    stale && tried.is_none_or(|(u, at)| u != url || now.saturating_sub(*at) >= RETRY)
}

/// Fetch, read and keep the list. On any failure the cache is untouched.
pub fn refresh(url: &str, path: &Path, now: u64, fetch: impl Fn(&str) -> Result<Vec<u8>, String>) -> Result<Live, String> {
    let live = parse(&fetch(url)?, url, now)?;
    store(path, &live)?;
    Ok(live)
}

/// `curl`, which macOS and Linux have: no TLS in trix itself.
fn curl(url: &str) -> Result<Vec<u8>, String> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--proto", "=https,http", "--max-time", &TIMEOUT.to_string(), "--max-filesize", "100000000", "--", url])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if why.is_empty() { format!("curl exited with {}", out.status) } else { why });
    }
    Ok(out.stdout)
}

/// Read the cache, then keep it fresh, on a thread of its own; tell the app
/// each time what it has.
pub fn spawn_fetcher(settings: Arc<Mutex<Settings>>, tx: Sender<AppEvent>) {
    let _ = std::thread::Builder::new().name("trix-prices".into()).spawn(move || {
        let path = cache_path();
        let mut feed = Feed { live: load(&path).map(Arc::new), error: None };
        if tx.send(AppEvent::Prices(feed.clone())).is_err() {
            return;
        }
        let mut tried: Option<(String, u64)> = None;
        loop {
            let s = settings.lock().unwrap().clone();
            if s.enabled && due(feed.live.as_deref(), &s.url, now(), tried.as_ref()) {
                tried = Some((s.url.clone(), now()));
                match refresh(&s.url, &path, now(), curl) {
                    Ok(l) => feed = Feed { live: Some(Arc::new(l)), error: None },
                    Err(e) => feed.error = Some(e),
                }
                if tx.send(AppEvent::Prices(feed.clone())).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(60));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("trix-test-prices-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A list like LiteLLM's: per token, with the models trix should skip.
    fn list() -> Value {
        let mut v = json!({
            "sample_spec": {"input_cost_per_token": 0, "output_cost_per_token": 0, "litellm_provider": "one of the providers"},
            "claude-opus-5-5": {"litellm_provider": "anthropic", "input_cost_per_token": 4e-6, "output_cost_per_token": 2e-5,
                "cache_read_input_token_cost": 2e-7, "cache_creation_input_token_cost": 5e-6, "cache_creation_input_token_cost_above_1hr": 8e-6},
            "anthropic/claude-opus-5-5-20260101": {"litellm_provider": "anthropic", "input_cost_per_token": 9e-6, "output_cost_per_token": 9e-5},
            "bedrock/us.anthropic.claude-opus-5-5": {"litellm_provider": "bedrock", "input_cost_per_token": 7e-6, "output_cost_per_token": 7e-5},
            "claude-haiku-5-5": {"litellm_provider": "anthropic", "input_cost_per_token": 1e-7, "output_cost_per_token": 5e-7,
                "input_cost_per_token_above_100k_tokens": 5e-7, "output_cost_per_token_above_100k_tokens": 2.5e-6,
                "cache_creation_input_token_cost_above_1hr_above_100k_tokens": 1e-6},
            "gpt-6.1-sol": {"litellm_provider": "openai", "input_cost_per_token": 2e-6, "output_cost_per_token": 1e-5, "cache_read_input_token_cost": 1e-7,
                "input_cost_per_token_above_272k_tokens": 4e-6, "output_cost_per_token_above_272k_tokens": 1.5e-5, "cache_read_input_token_cost_above_272k_tokens": 2e-7,
                "cache_creation_input_token_cost_above_12hr_above_272k_tokens": 1e-5, "input_cost_per_token_priority": 1.0},
            "gpt-5-codex": {"litellm_provider": "openai", "input_cost_per_token": 1.25e-6, "output_cost_per_token": 1e-5},
            "openai/gpt-image-1.5": {"litellm_provider": "openai", "input_cost_per_image": 0.01},
        });
        for i in 0..AT_LEAST {
            v[format!("filler-{i}")] = json!({"litellm_provider": "openai", "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-6});
        }
        v
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn reads_litellms_list() {
        let l = parse(list().to_string().as_bytes(), "u", 7).unwrap();
        let opus = &l.get("claude-opus-5-5").unwrap().base;
        assert!(close(opus.input, 4.0) && close(opus.output, 20.0) && close(opus.cache_read, 0.2) && close(opus.cache_write, 5.0) && close(opus.cache_write_1h, 8.0), "per million, the undated anthropic one: {opus:?}");
        let haiku = l.get("claude-haiku-5-5").unwrap();
        assert_eq!(haiku.base.input, 0.1, "exactly");
        assert!(close(haiku.base.cache_write, 0.125) && close(haiku.base.cache_write_1h, 0.2), "Anthropic's cache writes when the list leaves them out");
        let (k, long) = &haiku.above[0];
        assert!(*k == 100 && close(long.input, 0.5) && close(long.output, 2.5) && close(long.cache_write_1h, 1.0), "{long:?}");
        let sol = l.get("gpt-6.1-sol").unwrap();
        assert!(close(sol.base.cache_write, 2.0), "OpenAI's: plain input");
        assert_eq!(sol.above.len(), 1, "an unknown rate (above_12hr) is left out");
        assert!(close(sol.above[0].1.input, 4.0) && close(sol.above[0].1.cache_read, 0.2) && close(sol.above[0].1.cache_write, 4.0), "the tier's own input");
        let codex = &l.get("gpt-5-codex").unwrap().base;
        assert!(close(codex.cache_read, 1.25), "no cached rate: no discount");
        assert!(l.get("gpt-image-1.5").is_none() && l.get("sample_spec").is_none(), "not priced per token");
        assert_eq!((l.url.as_str(), l.fetched_at), ("u", 7));
    }

    #[test]
    fn a_bad_download_keeps_the_last_good_list() {
        let dir = tmp();
        let path = dir.join("trix/prices.json");
        let good = list().to_string().into_bytes();
        let l = refresh("u", &path, 100, |_| Ok(good.clone())).unwrap();
        assert_eq!(load(&path), Some(l.clone()), "kept on disk");
        for bad in [Err("curl: (28) Operation timed out".to_string()), Ok(b"<html>rate limited</html>".to_vec()), Ok(b"{}".to_vec()), Ok(good[..good.len() / 2].to_vec())] {
            assert!(refresh("u", &path, 200, |_| bad.clone()).is_err());
            assert_eq!(load(&path), Some(l.clone()), "untouched");
        }
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(load(&path), None, "a broken cache is no cache");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fetched_once_a_day_and_retried_hourly() {
        let l = Live { url: "u".into(), fetched_at: 1000, ..Default::default() };
        assert!(due(None, "u", 0, None), "no list yet: at startup");
        assert!(!due(Some(&l), "u", 1000 + DAY - 1, None), "fresh");
        assert!(due(Some(&l), "u", 1000 + DAY, None), "a day old");
        assert!(due(Some(&l), "v", 1001, None), "another URL");
        let tried = ("u".to_string(), 1000 + DAY);
        assert!(!due(Some(&l), "u", 1000 + DAY + 60, Some(&tried)), "not hammering after a failure");
        assert!(due(Some(&l), "u", 1000 + DAY + RETRY, Some(&tried)));
        assert!(due(Some(&l), "v", 1000 + DAY + 60, Some(&tried)), "a new URL is tried at once");
    }
}
