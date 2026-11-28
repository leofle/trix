//! The run log: one shared `runs/<id>/log.jsonl`, in order. Assignments
//! (the prompts as rendered, with their briefs), questions, answers and
//! queued mentions, a diff summary per hand-in, check results, landings,
//! approvals and quota events.
//!
//! Each line carries the hash of the line before it, and
//! `trix workflow log --verify` reports any line that was edited, deleted
//! or cut off. That detects edits; it can't prevent them on a file you own.
//! The log stays on the machine: what leaves it is the outcome summary.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One line of the log.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub n: u64,
    pub at: u64,
    pub kind: String,
    pub data: serde_json::Value,
    /// The hash of the line before: sixty-four zeros for the first.
    pub prev: String,
    pub hash: String,
}

/// What's waiting to be written, as the run made it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Pending {
    pub at: u64,
    pub kind: String,
    pub data: serde_json::Value,
}

pub fn file(dir: &Path) -> PathBuf {
    dir.join("log.jsonl")
}

fn head_file(dir: &Path) -> PathBuf {
    dir.join("log.head")
}

/// SHA-256, as FIPS 180-4 has it. Small enough to carry, so the chain is
/// a real one without a dependency.
pub fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

fn hash_of(n: u64, at: u64, kind: &str, data: &serde_json::Value, prev: &str) -> String {
    sha256(format!("{prev}\n{n}\n{at}\n{kind}\n{data}").as_bytes())
}

/// The log's lines, as they parse. A line that doesn't is `Err` with its number.
pub fn read(dir: &Path) -> Vec<Result<Entry, usize>> {
    let text = std::fs::read_to_string(file(dir)).unwrap_or_default();
    text.lines()
        .enumerate()
        .map(|(i, l)| serde_json::from_str::<Entry>(l).map_err(|_| i + 1))
        .collect()
}

/// Append what the run made since its last look. The head (how many lines,
/// and the last one's hash) is kept beside the log, so lines cut off its
/// end are noticed too.
pub fn append(dir: &Path, pending: &[Pending]) -> Result<(), String> {
    if pending.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let last = read(dir).into_iter().rev().find_map(Result::ok);
    let (mut n, mut prev) = last.map_or((0, "0".repeat(64)), |e| (e.n, e.hash));
    let mut out = String::new();
    for p in pending {
        n += 1;
        let hash = hash_of(n, p.at, &p.kind, &p.data, &prev);
        let entry = Entry {
            n,
            at: p.at,
            kind: p.kind.clone(),
            data: p.data.clone(),
            prev: prev.clone(),
            hash: hash.clone(),
        };
        out.push_str(&serde_json::to_string(&entry).map_err(|e| e.to_string())?);
        out.push('\n');
        prev = hash;
    }
    let path = file(dir);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, out.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    super::store::write_atomic(&head_file(dir), format!("{n} {prev}\n").as_bytes())
}

/// Hold the log against its chain. `Ok`: how many lines, all as written.
/// `Err`: what's wrong, line by line: one edited, one missing, the end cut off.
pub fn verify(dir: &Path) -> Result<usize, Vec<String>> {
    let lines = read(dir);
    let mut wrong = Vec::new();
    let (mut n, mut prev) = (0u64, "0".repeat(64));
    for (i, line) in lines.iter().enumerate() {
        let at = i + 1;
        let Ok(e) = line else {
            wrong.push(format!(
                "line {at} isn't a log line: it was edited, or cut off"
            ));
            continue;
        };
        if e.n != n + 1 {
            wrong.push(format!(
                "line {at} is entry {}, and entry {} should be here: {} missing",
                e.n,
                n + 1,
                if e.n > n + 1 {
                    "a line is"
                } else {
                    "the order is wrong, or a line is"
                }
            ));
        } else if e.prev != prev {
            wrong.push(format!(
                "line {at} doesn't follow the line before it: one of them was edited"
            ));
        }
        if hash_of(e.n, e.at, &e.kind, &e.data, &e.prev) != e.hash {
            wrong.push(format!(
                "line {at} was edited: its content isn't what its hash was made from"
            ));
        }
        n = e.n;
        prev = e.hash.clone();
    }
    // The head: how long the log was when it was last written.
    let head = std::fs::read_to_string(head_file(dir)).unwrap_or_default();
    let mut parts = head.split_whitespace();
    if let (Some(count), Some(hash)) = (
        parts.next().and_then(|c| c.parse::<u64>().ok()),
        parts.next(),
    ) {
        if count > n {
            wrong.push(format!(
                "the log ends at entry {n}, and it had {count}: {} cut off its end",
                if count - n == 1 {
                    "a line was".to_string()
                } else {
                    format!("{} lines were", count - n)
                }
            ));
        } else if count == n && hash != prev && wrong.is_empty() {
            wrong.push(format!(
                "line {} isn't the one the log ended with: it was replaced",
                lines.len()
            ));
        }
    }
    if wrong.is_empty() {
        Ok(lines.len())
    } else {
        Err(wrong)
    }
}

/// The log, for you to read: every prompt, report, edge and nudge.
pub fn render(dir: &Path) -> String {
    let mut s = String::new();
    for e in read(dir).into_iter().flatten() {
        let (_, day, secs) = crate::ledger::local(e.at);
        let text = match e.kind.as_str() {
            "note" => e.data.as_str().unwrap_or_default().to_string(),
            "assign" => format!(
                "assigned {} (visit {}):\n{}",
                e.data["node"].as_str().unwrap_or_default(),
                e.data["visit"],
                indent(e.data["prompt"].as_str().unwrap_or_default())
            ),
            _ => format!("{}: {}", e.kind, e.data),
        };
        s.push_str(&format!(
            "{day:>2}d {:02}:{:02}  {text}\n",
            secs / 3600,
            secs / 60 % 60
        ));
    }
    s
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("    | {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("trix-runlog-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn p(kind: &str, text: &str) -> Pending {
        Pending {
            at: 1_795_000_000,
            kind: kind.into(),
            data: text.into(),
        }
    }

    #[test]
    fn sha256_is_sha256() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let long = "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(
            sha256(long.as_bytes()),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn verify_reports_an_edited_a_deleted_and_a_truncated_line() {
        let d = dir("verify");
        append(
            &d,
            &[
                p("note", "started implementer"),
                p("note", "implementer handed in ready: 2 files"),
            ],
        )
        .unwrap();
        append(
            &d,
            &[
                p("approval", "merge approved by judge"),
                p("note", "merged PR #20"),
            ],
        )
        .unwrap();
        assert_eq!(verify(&d), Ok(4));
        // Each line carries the hash of the line before it.
        let lines: Vec<Entry> = read(&d).into_iter().map(Result::unwrap).collect();
        assert_eq!(lines[0].prev, "0".repeat(64));
        assert!(
            lines
                .windows(2)
                .all(|w| w[1].prev == w[0].hash && w[1].n == w[0].n + 1)
        );
        let original = std::fs::read_to_string(file(&d)).unwrap();
        // Edited: an approval rewritten.
        std::fs::write(
            file(&d),
            original.replace("approved by judge", "approved by you"),
        )
        .unwrap();
        let wrong = verify(&d).unwrap_err();
        assert_eq!(
            wrong,
            ["line 3 was edited: its content isn't what its hash was made from"]
        );
        // Deleted: a line taken out of the middle.
        let kept: Vec<&str> = original
            .lines()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, l)| l)
            .collect();
        std::fs::write(file(&d), kept.join("\n") + "\n").unwrap();
        let wrong = verify(&d).unwrap_err();
        assert!(
            wrong[0]
                .starts_with("line 2 is entry 3, and entry 2 should be here: a line is missing"),
            "{wrong:?}"
        );
        // Truncated: the end cut off, whole lines or half of one.
        let first_two: String = original.lines().take(2).map(|l| format!("{l}\n")).collect();
        std::fs::write(file(&d), &first_two).unwrap();
        assert_eq!(
            verify(&d).unwrap_err(),
            ["the log ends at entry 2, and it had 4: 2 lines were cut off its end"]
        );
        std::fs::write(file(&d), &original[..original.len() - 40]).unwrap();
        let wrong = verify(&d).unwrap_err();
        assert!(
            wrong
                .iter()
                .any(|w| w.starts_with("line 4 isn't a log line")),
            "{wrong:?}"
        );
        // Put back as it was, it verifies again; and a new line chains on.
        std::fs::write(file(&d), &original).unwrap();
        assert_eq!(verify(&d), Ok(4));
        append(&d, &[p("note", "closed")]).unwrap();
        assert_eq!(verify(&d), Ok(5));
        assert!(render(&d).contains("merged PR #20"));
        let _ = std::fs::remove_dir_all(d);
    }
}
