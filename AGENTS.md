# Trix workspace

At the start of a session, run `trix ls --json` to discover peer agents.
The entry with `you: true` identifies your pane. Trix peers are separate from
your built-in subagent registry. Use `trix read <agent> -n 60` to inspect output
and `trix wait <agent> -t 30` to wait. When coordination is requested, use
`trix send <agent> <prompt>`. Do not assign work or change peer lifecycle unless
the user task authorizes it. Never bypass permission prompts with `--force`.
If Trix is unreachable, report that instead of assuming no peers exist.
