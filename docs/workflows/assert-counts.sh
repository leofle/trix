#!/bin/sh
# Counts the assertions in each #[test] of a Rust file's test module:
#   docs/workflows/assert-counts.sh src/case.rs
# Used to show the triage tests keep every assertion across the runner port.
awk '
/^    #\[test\]/ { pending = 1; next }
/^    fn / {
  name = "";
  if (pending) { name = $2; sub(/\(.*/, "", name); order[++n] = name }
  pending = 0
}
name != "" { line = $0; c = gsub(/assert(_eq|_ne)?!\(/, "", line); count[name] += c }
END { total = 0; for (i = 1; i <= n; i++) { printf "%s %d\n", order[i], count[order[i]]; total += count[order[i]] } printf "TOTAL %d tests %d assertions\n", n, total }
' "$1"
