//! What agents' tokens cost, and the budget they're held to.
//!
//! `usage.rs` totals each agent's tokens from its own conversation logs, per
//! model; this prices them. The prices are list prices per million tokens,
//! so for an agent on a subscription the figure is what the same work would
//! cost on the API, not what is billed. A model trix has no price for still
//! has its tokens counted: its cost is left out and said to be (`unpriced`).
//! `[cost.prices]` in config.toml adds or corrects prices.

use std::collections::BTreeMap;

use crate::usage::{Spend, Tokens};

/// USD per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// Writing the prompt cache for 5 minutes (Claude), or at all (others).
    pub cache_write: f64,
    /// Writing it for an hour (Claude).
    pub cache_write_1h: f64,
    /// Every rate times this for a prompt over 100K tokens (Haiku 5.5).
    pub long: f64,
}

impl Price {
    /// Anthropic's: cache writes 1.25× input for 5 minutes, 2× for an hour.
    const fn claude(input: f64, output: f64, cache_read: f64) -> Price {
        Price { input, output, cache_read, cache_write: input * 1.25, cache_write_1h: input * 2.0, long: 1.0 }
    }
    /// OpenAI's: a cache write is plain input.
    const fn openai(input: f64, output: f64, cache_read: f64) -> Price {
        Price { input, output, cache_read, cache_write: input, cache_write_1h: input, long: 1.0 }
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

/// List prices, most specific name first. Anthropic's from its model table
/// (November 2026); OpenAI's for the Codex models whose prices are known.
const PRICES: &[(&str, Price)] = &[
    ("claude-fable-5-2", Price::claude(8.0, 40.0, 0.20)),
    ("claude-mythos-5-2", Price::claude(8.0, 40.0, 0.20)),
    ("claude-fable-5-1", Price::claude(10.0, 50.0, 0.25)),
    ("claude-mythos-5-1", Price::claude(10.0, 50.0, 0.25)),
    ("claude-fable-5", Price::claude(10.0, 50.0, 1.0)),
    ("claude-mythos-5", Price::claude(10.0, 50.0, 1.0)),
    ("claude-opus-5-5", Price::claude(4.0, 20.0, 0.20)),
    ("claude-opus-5", Price::claude(5.0, 25.0, 0.50)),
    ("claude-opus-4-8", Price::claude(5.0, 25.0, 0.50)),
    ("claude-opus-4-7", Price::claude(5.0, 25.0, 0.50)),
    ("claude-opus-4-6", Price::claude(5.0, 25.0, 0.50)),
    ("claude-opus-4-5", Price::claude(5.0, 25.0, 0.50)),
    ("claude-opus-4-1", Price::claude(15.0, 75.0, 1.50)),
    ("claude-opus-4", Price::claude(15.0, 75.0, 1.50)),
    ("claude-sonnet-5-5", Price::claude(2.0, 10.0, 0.10)),
    ("claude-sonnet-5", Price::claude(2.0, 10.0, 0.20)),
    ("claude-sonnet-4-6", Price::claude(3.0, 15.0, 0.30)),
    ("claude-sonnet-4-5", Price::claude(3.0, 15.0, 0.30)),
    ("claude-sonnet-4", Price::claude(3.0, 15.0, 0.30)),
    ("claude-3-7-sonnet", Price::claude(3.0, 15.0, 0.30)),
    ("claude-haiku-5-5", Price { long: 5.0, ..Price::claude(0.10, 0.50, 0.01) }),
    ("claude-haiku-4-5", Price::claude(1.0, 5.0, 0.10)),
    ("claude-3-5-haiku", Price::claude(0.80, 4.0, 0.08)),
    ("gpt-5.3-codex", Price::openai(1.75, 14.0, 0.175)),
    ("gpt-5.2-codex", Price::openai(1.75, 14.0, 0.175)),
    ("gpt-5.2", Price::openai(1.75, 14.0, 0.175)),
    ("gpt-5.1-codex", Price::openai(1.25, 10.0, 0.125)),
    ("gpt-5.1", Price::openai(1.25, 10.0, 0.125)),
    ("gpt-5-codex", Price::openai(1.25, 10.0, 0.125)),
    ("gpt-5", Price::openai(1.25, 10.0, 0.125)),
];

/// The model id as the price table names it: Bedrock's and Vertex's
/// prefixes and the `[1m]` variant go (same price).
fn normalize(model: &str) -> String {
    let m = model.trim().to_ascii_lowercase().replace("[1m]", "");
    match m.find("claude-") {
        Some(i) => m[i..].to_string(),
        None => m,
    }
}

/// `name` is `model`, or `model` with a date or version after it
/// (`claude-sonnet-4-5-20250929`, `…-v1:0`, `…@20251101`), not a newer
/// model whose name starts the same (`claude-opus-5-5` isn't `claude-opus-5`).
fn names(name: &str, model: &str) -> bool {
    let Some(rest) = model.strip_prefix(name) else { return false };
    if rest.is_empty() || rest.starts_with('@') || rest.starts_with(':') || rest.starts_with("-v") {
        return true;
    }
    rest.strip_prefix('-').is_some_and(|r| r.bytes().take_while(u8::is_ascii_digit).count() >= 6)
}

/// The price of `model`: config.toml's first (by exact name, else the
/// longest name it starts with), then trix's own.
pub fn price(model: &str, own: &BTreeMap<String, Price>) -> Option<Price> {
    let m = normalize(model);
    if let Some(p) = own.get(model).or_else(|| own.get(&m)) {
        return Some(*p);
    }
    if let Some((_, p)) = own.iter().filter(|(k, _)| m.starts_with(&normalize(k))).max_by_key(|(k, _)| k.len()) {
        return Some(*p);
    }
    PRICES.iter().find(|(n, _)| names(n, &m)).map(|(_, p)| *p)
}

/// What some spend cost, in USD, and how many of its tokens had no price.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub usd: f64,
    pub unpriced: u64,
}

pub fn cost(spend: &Spend, own: &BTreeMap<String, Price>) -> Cost {
    let mut c = Cost::default();
    for (b, t) in spend {
        match price(&b.model, own) {
            Some(p) => {
                // Fast mode is twice the rate wherever it's offered.
                let speed = if b.fast { 2.0 } else { 1.0 };
                let size = if b.long { p.long } else { 1.0 };
                c.usd += p.of(t) * speed * size;
            }
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

    fn spend(model: &str, fast: bool, long: bool, t: Tokens) -> Spend {
        Spend::from([(Bucket { model: model.into(), fast, long }, t)])
    }
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn models_find_their_price() {
        let none = BTreeMap::new();
        assert_eq!(price("claude-opus-5-5", &none).map(|p| p.input), Some(4.0));
        assert_eq!(price("claude-opus-5", &none).map(|p| p.input), Some(5.0), "not Opus 5.5's");
        assert_eq!(price("claude-sonnet-4-5-20250929", &none).map(|p| p.input), Some(3.0), "a dated id");
        assert_eq!(price("us.anthropic.claude-sonnet-4-5-20250929-v1:0", &none).map(|p| p.output), Some(15.0), "Bedrock's");
        assert_eq!(price("claude-opus-4-5@20251101", &none).map(|p| p.input), Some(5.0), "Vertex's");
        assert_eq!(price("claude-opus-4-20250514", &none).map(|p| p.input), Some(15.0));
        assert_eq!(price("claude-sonnet-5-5[1m]", &none).map(|p| p.cache_read), Some(0.10));
        assert_eq!(price("claude-opus-5-6", &none), None, "a newer model isn't guessed at");
        assert_eq!(price("gpt-5.1-codex", &none).map(|p| p.cache_read), Some(0.125));
        assert_eq!(price("gpt-6.1-sol", &none), None);
        let own = BTreeMap::from([("gpt-6".to_string(), Price::openai(2.0, 10.0, 0.1)), ("gpt-6.1-sol".to_string(), Price::openai(3.0, 10.0, 0.1))]);
        assert_eq!(price("gpt-6.1-sol", &own).map(|p| p.input), Some(3.0), "the exact name");
        assert_eq!(price("gpt-6-luna", &own).map(|p| p.input), Some(2.0), "the name it starts with");
        let own = BTreeMap::from([("claude-opus-5-5".to_string(), Price::claude(1.0, 1.0, 1.0))]);
        assert_eq!(price("claude-opus-5-5", &own).map(|p| p.input), Some(1.0), "config.toml's price wins");
    }

    #[test]
    fn tokens_are_priced_by_kind() {
        let none = BTreeMap::new();
        let t = Tokens { input: 1_000_000, output: 1_000_000, cache_read: 1_000_000, cache_write_5m: 1_000_000, cache_write_1h: 1_000_000 };
        // Opus 5.5: 4 + 20 + 0.20 + 5 + 8.
        assert!(close(cost(&spend("claude-opus-5-5", false, false, t), &none).usd, 37.2));
        assert!(close(cost(&spend("claude-opus-5-5", true, false, t), &none).usd, 74.4), "fast mode: twice");
        let small = Tokens { input: 1_000_000, ..Default::default() };
        assert!(close(cost(&spend("claude-haiku-5-5", false, true, small), &none).usd, 0.5), "Haiku 5.5's long prompts");
        assert!(close(cost(&spend("claude-opus-5-5", false, true, small), &none).usd, 4.0), "others' aren't dearer");
        let codex = Tokens { input: 1_000_000, cache_read: 1_000_000, output: 1_000_000, ..Default::default() };
        assert!(close(cost(&spend("gpt-5-codex", false, false, codex), &none).usd, 11.375));
        let mut both = spend("claude-sonnet-5-5", false, false, small);
        both.extend(spend("mystery", false, false, Tokens { output: 42, ..Default::default() }));
        let c = cost(&both, &none);
        assert!(close(c.usd, 2.0));
        assert_eq!(c.unpriced, 42, "counted, not priced");
        assert_eq!(label(&c), "$2.00+");
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
