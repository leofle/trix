//! What agents' tokens cost, and the budget they're held to.
//!
//! `usage.rs` totals each agent's tokens from its own conversation logs, per
//! model; this prices them. The prices are list prices per million tokens,
//! so for an agent on a subscription the figure is what the same work would
//! cost on the API, not what is billed. A model with no price still has its
//! tokens counted: its cost is left out and said to be (`unpriced`).
//!
//! A model's price comes from `[cost.prices]` in config.toml, then the live
//! list trix fetches (`prices.rs`), then the table here, for when trix has
//! never been online.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::prices::Live;
use crate::usage::{Spend, Tokens};

/// USD per million tokens.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// Writing the prompt cache for 5 minutes (Claude), or at all (others).
    pub cache_write: f64,
    /// Writing it for an hour (Claude).
    pub cache_write_1h: f64,
}

impl Price {
    /// Anthropic's: cache writes 1.25× input for 5 minutes, 2× for an hour.
    pub const fn claude(input: f64, output: f64, cache_read: f64) -> Price {
        Price { input, output, cache_read, cache_write: input * 1.25, cache_write_1h: input * 2.0 }
    }
    /// OpenAI's: a cache write is plain input.
    pub const fn openai(input: f64, output: f64, cache_read: f64) -> Price {
        Price { input, output, cache_read, cache_write: input, cache_write_1h: input }
    }

    fn of(&self, t: &Tokens) -> f64 {
        (t.input as f64 * self.input
            + t.output as f64 * self.output
            + t.cache_read as f64 * self.cache_read
            + t.cache_write_5m as f64 * self.cache_write
            + t.cache_write_1h as f64 * self.cache_write_1h)
            / 1_000_000.0
    }
}

/// A model's prices: for most prompts, and for prompts over so many
/// thousand tokens, where it charges more (smallest first).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tiered {
    pub base: Price,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub above: Vec<(u32, Price)>,
}

impl Tiered {
    /// The rates for a prompt in `tier` (see [`tier`]).
    pub fn at(&self, tier: u32) -> &Price {
        self.above.iter().rev().find(|(k, _)| tier >= *k).map_or(&self.base, |(_, p)| p)
    }
}

/// The prompt sizes, in thousands of tokens, past which some model charges
/// more: the ones LiteLLM's list has.
pub const TIERS: [u32; 7] = [32, 100, 128, 200, 256, 272, 512];

/// The largest of `TIERS` a prompt of `tokens` is over, or 0: a call is
/// counted apart by it, so each tier's price applies to just its calls.
pub fn tier(tokens: u64) -> u32 {
    TIERS.iter().rev().copied().find(|&k| tokens > u64::from(k) * 1000).unwrap_or(0)
}

/// A model's name, its price, and its prices past prompt sizes.
type BuiltIn = (&'static str, Price, &'static [(u32, Price)]);

/// List prices, from Anthropic's model table (November 2026) and for the
/// GPT-5 Codex models: what trix uses with no live list.
const PRICES: &[BuiltIn] = &[
    ("claude-fable-5-2", Price::claude(8.0, 40.0, 0.20), &[]),
    ("claude-mythos-5-2", Price::claude(8.0, 40.0, 0.20), &[]),
    ("claude-fable-5-1", Price::claude(10.0, 50.0, 0.25), &[]),
    ("claude-mythos-5-1", Price::claude(10.0, 50.0, 0.25), &[]),
    ("claude-fable-5", Price::claude(10.0, 50.0, 1.0), &[]),
    ("claude-mythos-5", Price::claude(10.0, 50.0, 1.0), &[]),
    ("claude-opus-5-5", Price::claude(4.0, 20.0, 0.20), &[]),
    ("claude-opus-5", Price::claude(5.0, 25.0, 0.50), &[]),
    ("claude-opus-4-8", Price::claude(5.0, 25.0, 0.50), &[]),
    ("claude-opus-4-7", Price::claude(5.0, 25.0, 0.50), &[]),
    ("claude-opus-4-6", Price::claude(5.0, 25.0, 0.50), &[]),
    ("claude-opus-4-5", Price::claude(5.0, 25.0, 0.50), &[]),
    ("claude-opus-4-1", Price::claude(15.0, 75.0, 1.50), &[]),
    ("claude-opus-4", Price::claude(15.0, 75.0, 1.50), &[]),
    ("claude-sonnet-5-5", Price::claude(2.0, 10.0, 0.10), &[]),
    ("claude-sonnet-5", Price::claude(2.0, 10.0, 0.20), &[]),
    ("claude-sonnet-4-6", Price::claude(3.0, 15.0, 0.30), &[]),
    ("claude-sonnet-4-5", Price::claude(3.0, 15.0, 0.30), &[]),
    ("claude-sonnet-4", Price::claude(3.0, 15.0, 0.30), &[]),
    ("claude-3-7-sonnet", Price::claude(3.0, 15.0, 0.30), &[]),
    ("claude-haiku-5-5", Price::claude(0.10, 0.50, 0.01), &[(100, Price::claude(0.50, 2.50, 0.05))]),
    ("claude-haiku-4-5", Price::claude(1.0, 5.0, 0.10), &[]),
    ("claude-3-5-haiku", Price::claude(0.80, 4.0, 0.08), &[]),
    ("gpt-5.3-codex", Price::openai(1.75, 14.0, 0.175), &[]),
    ("gpt-5.2-codex", Price::openai(1.75, 14.0, 0.175), &[]),
    ("gpt-5.2", Price::openai(1.75, 14.0, 0.175), &[]),
    ("gpt-5.1-codex", Price::openai(1.25, 10.0, 0.125), &[]),
    ("gpt-5.1", Price::openai(1.25, 10.0, 0.125), &[]),
    ("gpt-5-codex", Price::openai(1.25, 10.0, 0.125), &[]),
    ("gpt-5", Price::openai(1.25, 10.0, 0.125), &[]),
];

/// A model's name as price lists file it, so the names agents log and the
/// names a list uses meet: lowercase, without a provider (`anthropic/`,
/// `openai/`, Bedrock's `us.anthropic.`), Claude's `[1m]` variant, a
/// version or region (`@20251101`, `-v1:0`) or a date
/// (`-20250929`, `-2025-08-07`). `claude-opus-5-5` stays itself, not
/// `claude-opus-5`: only a whole date goes.
pub fn canon(name: &str) -> String {
    let mut m = name.trim().to_ascii_lowercase().replace("[1m]", "");
    if let Some(i) = m.rfind('/') {
        m.drain(..=i);
    }
    if let Some(i) = m.find("claude-") {
        m.drain(..i);
    }
    if let Some(i) = m.find(['@', ':']) {
        m.truncate(i);
    }
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some((rest, v)) = m.rsplit_once("-v")
        && digits(v)
    {
        m.truncate(rest.len());
    }
    // -YYYY-MM-DD (OpenAI), then -YYYYMMDD (Anthropic).
    let parts: Vec<&str> = m.rsplitn(4, '-').collect();
    if parts.len() == 4 && [(0, 2), (1, 2), (2, 4)].iter().all(|&(i, n)| parts[i].len() == n && digits(parts[i])) {
        m.truncate(parts[3].len());
    } else if let Some((rest, d)) = m.rsplit_once('-')
        && d.len() == 8
        && digits(d)
    {
        m.truncate(rest.len());
    }
    m
}

/// Where a price comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    Config,
    Live,
    BuiltIn,
}

/// The price of `model`: config.toml's first (by exact name, else the
/// longest name it starts with), then the live list's, then trix's own.
pub fn price(model: &str, own: &BTreeMap<String, Price>, live: Option<&Live>) -> Option<(Tiered, Source)> {
    let m = canon(model);
    let mine = own.get(model).or_else(|| own.get(&m)).or_else(|| own.iter().filter(|(k, _)| m.starts_with(&canon(k))).max_by_key(|(k, _)| k.len()).map(|(_, p)| p));
    if let Some(p) = mine {
        return Some((Tiered { base: *p, above: Vec::new() }, Source::Config));
    }
    if let Some(t) = live.and_then(|l| l.get(&m)) {
        return Some((t.clone(), Source::Live));
    }
    PRICES.iter().find(|(n, _, _)| *n == m).map(|(_, p, above)| (Tiered { base: *p, above: above.to_vec() }, Source::BuiltIn))
}

/// What some spend cost, in USD, and how many of its tokens had no price.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub usd: f64,
    pub unpriced: u64,
}

pub fn cost(spend: &Spend, own: &BTreeMap<String, Price>, live: Option<&Live>) -> Cost {
    let mut c = Cost::default();
    for (b, t) in spend {
        match price(&b.model, own, live) {
            // Fast mode is twice the rate wherever it's offered.
            Some((p, _)) => c.usd += p.at(b.prompt).of(t) * if b.fast { 2.0 } else { 1.0 },
            None => c.unpriced += t.total(),
        }
    }
    c
}

/// "$0.0042", "$0.42", "$12.40", "$1234". Under a dollar, at least two
/// significant digits (up to six decimals), so a small amount isn't "$0.00".
pub fn usd(x: f64) -> String {
    if x >= 100.0 {
        return format!("${}", x.round() as u64);
    }
    if x <= 0.0 || x >= 1.0 {
        return format!("${:.2}", x.max(0.0));
    }
    let decimals = ((-x.log10()).ceil() as usize + 1).clamp(2, 6);
    let s = format!("{x:.decimals$}");
    // Zeros past the cents say nothing: "$0.020" is "$0.02".
    let keep = s.trim_end_matches('0').len().max(s.find('.').unwrap() + 3);
    format!("${}", &s[..keep])
}

/// The cost as the screen shows it: a `+` when some tokens had no price.
pub fn label(c: &Cost) -> String {
    format!("{}{}", usd(c.usd), if c.unpriced > 0 { "+" } else { "" })
}

/// How near a budget: 0 under the warning, 1 past it, 2 over the budget.
pub fn level(usd: f64, budget: Option<f64>, warn_at: u8) -> u8 {
    match budget {
        Some(b) if usd >= b => 2,
        Some(b) if usd >= b * f64::from(warn_at) / 100.0 => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Bucket;

    fn spend(model: &str, fast: bool, prompt: u32, t: Tokens) -> Spend {
        Spend::from([(Bucket { model: model.into(), fast, prompt }, t)])
    }
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }
    fn input(model: &str, own: &BTreeMap<String, Price>) -> Option<f64> {
        price(model, own, None).map(|(p, _)| p.base.input)
    }

    #[test]
    fn names_meet_whatever_their_provider_or_date() {
        for (name, want) in [
            ("claude-opus-5-5", "claude-opus-5-5"),
            ("anthropic/claude-opus-5-5", "claude-opus-5-5"),
            ("Claude-Sonnet-5-5[1m]", "claude-sonnet-5-5"),
            ("claude-sonnet-4-5-20250929", "claude-sonnet-4-5"),
            ("us.anthropic.claude-sonnet-4-5-20250929-v1:0", "claude-sonnet-4-5"),
            ("bedrock/eu.anthropic.claude-opus-4-1-20250805-v2:0", "claude-opus-4-1"),
            ("vertex_ai/claude-opus-4-5@20251101", "claude-opus-4-5"),
            ("claude-opus-4-20250514", "claude-opus-4"),
            ("openai/gpt-5-codex", "gpt-5-codex"),
            ("gpt-5-2025-08-07", "gpt-5"),
            ("gpt-6.1-sol", "gpt-6.1-sol"),
            ("gpt-4o-mini-2024-07-18", "gpt-4o-mini"),
            ("claude-3-5-haiku-latest", "claude-3-5-haiku-latest"),
        ] {
            assert_eq!(canon(name), want, "{name}");
        }
        assert_ne!(canon("claude-opus-5-5"), canon("claude-opus-5"), "a version isn't a date");
        assert_ne!(canon("gpt-5.1-codex-max"), canon("gpt-5.1-codex"));
    }

    #[test]
    fn models_find_their_price() {
        let none = BTreeMap::new();
        assert_eq!(input("claude-opus-5-5", &none), Some(4.0));
        assert_eq!(input("claude-opus-5", &none), Some(5.0), "not Opus 5.5's");
        assert_eq!(input("claude-sonnet-4-5-20250929", &none), Some(3.0), "a dated id");
        assert_eq!(price("us.anthropic.claude-sonnet-4-5-20250929-v1:0", &none, None).map(|(p, _)| p.base.output), Some(15.0), "Bedrock's");
        assert_eq!(input("claude-opus-4-5@20251101", &none), Some(5.0), "Vertex's");
        assert_eq!(input("claude-opus-4-20250514", &none), Some(15.0));
        assert_eq!(price("claude-sonnet-5-5[1m]", &none, None).map(|(p, s)| (p.base.cache_read, s)), Some((0.10, Source::BuiltIn)));
        assert_eq!(input("claude-opus-5-6", &none), None, "a newer model isn't guessed at");
        assert_eq!(price("gpt-5.1-codex", &none, None).map(|(p, _)| p.base.cache_read), Some(0.125));
        assert_eq!(input("gpt-6.1-sol", &none), None);
        let own = BTreeMap::from([("gpt-6".to_string(), Price::openai(2.0, 10.0, 0.1)), ("gpt-6.1-sol".to_string(), Price::openai(3.0, 10.0, 0.1))]);
        assert_eq!(input("gpt-6.1-sol", &own), Some(3.0), "the exact name");
        assert_eq!(input("gpt-6-luna", &own), Some(2.0), "the name it starts with");
        let own = BTreeMap::from([("claude-opus-5-5".to_string(), Price::claude(1.0, 1.0, 1.0))]);
        assert_eq!(price("claude-opus-5-5", &own, None).map(|(p, s)| (p.base.input, s)), Some((1.0, Source::Config)), "config.toml's price wins");
    }

    #[test]
    fn config_then_the_live_list_then_built_in() {
        let live = Live::from_models("u", 0, [("claude-opus-5-5".to_string(), Tiered { base: Price::claude(3.0, 15.0, 0.3), above: vec![] }), ("gpt-6.1-sol".to_string(), Tiered { base: Price::openai(2.0, 10.0, 0.1), above: vec![] })]);
        let none = BTreeMap::new();
        let src = |m: &str, own: &BTreeMap<String, Price>| price(m, own, Some(&live)).map(|(p, s)| (p.base.input, s));
        assert_eq!(src("claude-opus-5-5-20261101", &none), Some((3.0, Source::Live)), "the live list over the table");
        assert_eq!(src("openai/gpt-6.1-sol", &none), Some((2.0, Source::Live)), "a model only the live list has");
        assert_eq!(src("claude-haiku-5-5", &none), Some((0.10, Source::BuiltIn)), "the table for what the list lacks");
        let own = BTreeMap::from([("claude-opus-5-5".to_string(), Price::claude(1.0, 1.0, 1.0))]);
        assert_eq!(src("claude-opus-5-5", &own), Some((1.0, Source::Config)));
    }

    #[test]
    fn tokens_are_priced_by_kind() {
        let none = BTreeMap::new();
        let t = Tokens { input: 1_000_000, output: 1_000_000, cache_read: 1_000_000, cache_write_5m: 1_000_000, cache_write_1h: 1_000_000 };
        // Opus 5.5: 4 + 20 + 0.20 + 5 + 8.
        assert!(close(cost(&spend("claude-opus-5-5", false, 0, t), &none, None).usd, 37.2));
        assert!(close(cost(&spend("claude-opus-5-5", true, 0, t), &none, None).usd, 74.4), "fast mode: twice");
        let small = Tokens { input: 1_000_000, ..Default::default() };
        assert!(close(cost(&spend("claude-haiku-5-5", false, 100, small), &none, None).usd, 0.5), "Haiku 5.5's long prompts");
        assert!(close(cost(&spend("claude-haiku-5-5", false, 32, small), &none, None).usd, 0.1), "not long enough");
        assert!(close(cost(&spend("claude-opus-5-5", false, 512, small), &none, None).usd, 4.0), "others' aren't dearer");
        let codex = Tokens { input: 1_000_000, cache_read: 1_000_000, output: 1_000_000, ..Default::default() };
        assert!(close(cost(&spend("gpt-5-codex", false, 0, codex), &none, None).usd, 11.375));
        let mut both = spend("claude-sonnet-5-5", false, 0, small);
        both.extend(spend("mystery", false, 0, Tokens { output: 42, ..Default::default() }));
        let c = cost(&both, &none, None);
        assert!(close(c.usd, 2.0));
        assert_eq!(c.unpriced, 42, "counted, not priced");
        assert_eq!(label(&c), "$2.00+");
    }

    #[test]
    fn prompts_fall_in_tiers() {
        assert_eq!((tier(0), tier(32_000), tier(32_001), tier(150_000), tier(272_001), tier(2_000_000)), (0, 0, 32, 128, 272, 512));
        let p = Tiered { base: Price::openai(1.0, 1.0, 1.0), above: vec![(128, Price::openai(2.0, 2.0, 2.0)), (272, Price::openai(3.0, 3.0, 3.0))] };
        assert_eq!([0, 100, 128, 256, 272, 512].map(|t| p.at(t).input), [1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
    }

    #[test]
    fn money_and_budget_levels() {
        assert_eq!(usd(0.0), "$0.00");
        assert_eq!(usd(0.02), "$0.02", "not $0.020");
        assert_eq!(usd(0.004), "$0.004");
        assert_eq!(usd(0.0042), "$0.0042");
        assert_eq!(usd(0.01), "$0.01");
        assert_eq!(usd(0.015), "$0.015");
        assert_eq!(usd(0.123), "$0.12");
        assert_eq!(usd(0.999), "$1.00");
        assert_eq!(usd(0.000_000_4), "$0.00", "past six decimals");
        assert_eq!(format!("{} of {}", usd(0.0042), usd(0.02)), "$0.0042 of $0.02");
        assert_eq!(usd(12.4), "$12.40");
        assert_eq!(usd(1234.4), "$1234");
        assert_eq!(level(5.0, None, 80), 0);
        assert_eq!(level(7.9, Some(10.0), 80), 0);
        assert_eq!(level(8.0, Some(10.0), 80), 1);
        assert_eq!(level(10.0, Some(10.0), 80), 2);
    }
}
