# trix

A terminal workspace for coding agents. Run Claude Code, Codex, Grok, OpenCode, Cursor, GitHub Copilot CLI, Muse Code, Gemini CLI, Antigravity CLI, Amp, shells or any CLI agent side by side, and see at a glance which one is working, which one needs you, and which one finished while you were looking elsewhere.

![trix demo: starting trix in a project and working with agents side by side](assets/trix.gif)

**Get started:** [Install](#install) · [Quick start](#quick-start) · [Keys](#keys) · [macOS](#macos) · [Supported agents](#supported-agents) · [Codex](#codex)<br>
**Features:** [Search](#search) · [Runnable worktrees](#runnable-worktrees) · [Review](#review) · [Clone](#clone) · [Agent control](#agent-control) · [Triage](#triage) · [Layouts](#layouts) · [Restore](#restore) · [Sessions](#sessions) · [Viewers](#viewers) · [Context gauge](#context-gauge) · [Cost and tokens](#cost-and-tokens)<br>
**Configure:** [Settings](#settings) · [Config](#config) · [Environment variables](#environment-variables) · [Themes](#themes) · [Your instructions](#your-instructions)<br>
**Reference:** [Security](#security) · [Troubleshooting](#troubleshooting) · [Development](#development) · [License](#license)

## Why

Generic multiplexers (tmux, zellij) know nothing about agents. herdr and Rex added agent awareness, but still treat the agent as just another pane. In trix, the agent comes first:

- **Hooks first, guessing second.** trix hands each agent hooks or a plugin for that one process: Claude Code a `--settings` overlay, Codex `-c` hook overrides, OpenCode a plugin in `OPENCODE_CONFIG_CONTENT`, Cursor and Copilot a `--plugin-dir`, Muse a hooks file named by `-c`. The agent then reports its own lifecycle: prompt submitted, each tool call, permission request, question, turn finished, interrupted. Your own agent configs are never touched. Grok, Gemini, Antigravity and Amp can't take hooks per process, so trix reads Grok's and Gemini's terminal titles and Antigravity's and Amp's screens, and you can add trix's hook (for Amp, its plugin) yourself. Agents without hooks fall back to output cadence and screen heuristics. See [Supported agents](#supported-agents), and [Codex](#codex) for its one-time trust step.
- **Feedback you can't miss.** Each agent's card shows a live state glyph, what it's doing now (`Edit app.rs`, `Bash cargo test`, `approve …`), time in state, a 48-second output sparkline, its git branch and the diff it has made so far. When an agent you're *not* looking at needs you, a red strip covers your pane's title (it takes no row, so your pane never moves): who, what it's asking and how long it has waited. Click it, or press `alt+a`, to go there. A desktop notification with trix's icon comes too (`notify-send` on Linux, so Omarchy's mako shows it), and the terminal bell sets the urgency hint while trix is unfocused. When an agent you're not looking at finishes, a short rising chime plays (`pw-play` from PipeWire on Linux, `afplay` on macOS); completions within two seconds share one chime. The terminal title tracks the counts. Which of these happen, and which sound plays, is up to you: see [Config](#config).
- **Context at a glance.** Each Claude and Codex pane shows how full its context window is (`context 31% · ~691k left`), yellow from 70% and red from 85%, read in the background from the agent's own conversation log. `trix ls --json` has it too, so a lead agent can see when a worker is running low. See [Context gauge](#context-gauge).
- **What it costs.** From the same logs, trix counts and prices each Claude and Codex agent's tokens: `$1.23 · 4.1M tok` on its pane, the workspace's total on the top bar, all of it in `trix ls --json`. Set a budget for the workspace, for each agent or for the month, and trix warns you as spend nears it and tells you when it goes over. `alt+$` shows what each model cost this month, today and last month, and where the month is heading. See [Cost and tokens](#cost-and-tokens).
- **One key to the next thing that matters.** `alt+a` goes to the agent that has been blocked longest, then to unseen finished work.
- **Many agents, one glance.** `alt+g` tiles your agents live, up to four at a time, and `alt+j`/`alt+k` page through the rest. Pin up to two agents to the top of the sidebar, and select several to act on all of them at once. See [Grid, pins and selection](#grid-pins-and-selection).
- **Isolation built in.** "Worktree" in the new-agent dialog creates `.trix/worktrees/<name>` on branch `trix/<name>`, excluded from git automatically, so parallel agents never step on each other. A name whose worktree is already there reuses it, unless an agent of another session is in it (or in a folder under it): then the form, and `trix spawn --worktree`, say whose it is (`trix/foo is foo's worktree in session sidebar: pick another name`). A repository can make those worktrees [runnable](#runnable-worktrees): a setup command, its `.env` files, and a block of ports for each. Closing an agent keeps its worktree and branch. When it's the last agent there and the worktree is clean, the close dialog offers `r` to remove it too; `alt+v` then `X` does the same from the review, once no session uses it ([Review](#review)). Until then git's own lock keeps the worktree from being removed by hand.
- **Review, then merge, without leaving trix.** `alt+v` opens an agent's diff. Leave notes on lines and send them back to the agent as one prompt; then commit, push, open a PR, merge and clean up the worktree from the same view. See [Review](#review).
- **Clone an agent.** `alt+l` starts a copy of the focused agent in a worktree of its own, with the original's uncommitted changes and, for Claude, Codex and Grok, a copy of its conversation. See [Clone](#clone).
- **Search every agent at once.** `alt+f` searches every agent's scrollback as you type, and Enter jumps to the match. See [Search](#search).
- **Broadcast.** `alt+b` sends one prompt to every agent, or only to the ones that are idle or done. Shells and agents sitting on a prompt are skipped, so a broadcast can never answer a permission dialog.
- **Agents outlive the terminal, and the server.** The workspace runs in a background server. Closing the window, losing SSH or pressing `alt+d` only detaches: agents keep working and still notify you on the desktop. Run `trix` again from any terminal to pick up where you were, scrollback included. Each agent's terminal is held by a small process of its own, so if the server crashes, every program keeps running and the next `trix` reattaches it. After `trix kill`, `alt+q` or a reboot, the next `trix` offers to bring the workspace back, each agent in the conversation it was in (`claude --resume <id>`, `codex resume <id>`, …) and each shell in its folder. See [Restore](#restore).
- **Agents can run agents.** `trix spawn`, `send`, `read`, `wait`, `close` and `events` let a script, or an agent in another pane, work the workspace the way you do. A lead Claude can start workers in their own worktrees, hand them tasks, wait for them and read what they did. It never answers a permission prompt for you, and the sidebar shows which agent it is driving. See [Agent control](#agent-control).
- **Layouts on demand.** `trix layout save work` remembers the tabs you have open, with their folders, and `trix layout open work` brings them back, with fresh conversations or, with `--resume`, the old ones. See [Layouts](#layouts).
- **Watch without taking over.** `trix view` attaches read-only from any number of other terminals: a second monitor, a laptop over SSH, a teammate on the same box. Nothing a viewer types reaches an agent. See [Viewers](#viewers).
- **Named sessions.** `trix -s work` and `trix -s home` are separate workspaces, each with its own agents, saved workspace and attached terminal. `trix ls` shows all of them. See [Sessions](#sessions).
- **Fast.** Rust, with `alacritty_terminal` as the VT engine (the same one Zed uses) and `ratatui`. Rendering is event-driven and capped at about 120 fps, and trix does nothing when nothing changes. Idle cost is about 0.4% of one core and about 10 MB of RAM. `seq 1 2000000` streams through in under a second.
- **Follows your theme.** trix follows Omarchy's theme live, and falls back to Tokyo Night without it. Or pick one of the 24 it ships (Omarchy's 23 and Cyber Aura, so a Mac has them too) in the settings, tried on live before you keep it. A theme you pick colors the agents' panes as well. See [Themes](#themes).
- **Settings in place.** `alt+,` changes the theme, the needs-you strip, the sidebar, how new agents start, restoring, notifications and sounds. Each change is written to your config files with your comments kept. See [Settings](#settings).

## Use

trix runs on Linux and macOS, on x86_64 and ARM.

### Requirements

- **Linux or macOS.** The Linux release binaries are built on Ubuntu 22.04 against glibc, so they need glibc 2.35 or newer. There is no Windows build.
- **A terminal that passes Alt through.** Every trix key is `alt+…`. On a Mac, set your terminal's Option key to send Alt ([how, per terminal](#macos)).
- **The agents you want to run.** One that isn't installed yet installs in its own pane the first time you start it ([Not installed yet](#not-installed-yet)). Log in to each agent yourself the first time it runs.
- **Optional:**
  - `git`: worktrees, branches and diffs on agent cards, and the review.
  - `gh`: opening and merging pull requests from the review.
  - `curl`: the live price list, and the vendors' install scripts for missing agents.
  - On Linux, `notify-send` (libnotify) for desktop notifications, `pw-play` (PipeWire) for sounds and `wl-copy` for copying in terminals that ignore OSC 52.
- **To build from source:** Rust stable, 1.85 or newer (the crate uses the 2024 edition).

### Install

With [Homebrew](https://brew.sh), on macOS or Linux:

```sh
brew install leofle/trix/trix
```

On Arch Linux or Omarchy, the latest release's pacman package (x86_64 or aarch64):

```sh
curl -fLO https://github.com/leofle/trix/releases/latest/download/trix-$(uname -m).pkg.tar.zst
sudo pacman -U ./trix-$(uname -m).pkg.tar.zst
```

Each release also carries its `PKGBUILD`, if you'd rather build the package with `makepkg`.

The release binary for Linux or macOS (x86_64 or ARM), into `~/.local/bin`:

```sh
os=$([ "$(uname)" = Darwin ] && echo apple-darwin || echo unknown-linux-gnu)
target=$(uname -m | sed 's/arm64/aarch64/')-$os
curl -fsSL https://github.com/leofle/trix/releases/latest/download/trix-$target.tar.gz | tar -xz
mkdir -p ~/.local/bin && mv trix-$target/trix ~/.local/bin/ && rm -r trix-$target
```

Each archive on the [releases page](https://github.com/leofle/trix/releases) has a `.sha256` next to it. On a Mac, a copy downloaded with a browser instead of `curl` is quarantined; clear it with `xattr -d com.apple.quarantine trix`.

From source, with [Rust](https://rustup.rs) (on a Mac, see [Building on a Mac](#building-on-a-mac) first):

```sh
git clone https://github.com/leofle/trix.git && cd trix
cargo install --path .
```

`trix --version` prints the version you have.

### Upgrade

- Homebrew: `brew upgrade trix`.
- pacman package or release binary: run the install lines again.
- From source: `git pull && cargo install --path .` in your clone.

A running workspace keeps the old version until its server stops. `trix kill` stops it and every agent, and the next `trix` starts the new version and offers to bring the workspace back ([Restore](#restore)).

### Uninstall

1. Stop every session: `trix kill --all`.
2. Remove the binary: `brew uninstall trix`, `sudo pacman -R trix`, `rm ~/.local/bin/trix`, or `cargo uninstall trix`, whichever way you installed it.
3. If you want, remove what trix keeps:
   - `~/.config/trix/`: `config.toml`, `colors.toml`, `instructions.md` and `layouts/` (`$XDG_CONFIG_HOME/trix` if set).
   - `~/.local/state/trix/`: the saved workspace, `prefs.json`, the ports ledger, trusted repositories and `removed.log` (`$XDG_STATE_HOME/trix` if set).
   - `~/.cache/trix/`: the price list (`$XDG_CACHE_HOME/trix` if set).
   - On macOS, `~/Library/Application Support/trix`, the small app that sends notifications.
   - Worktrees under a repository's `.trix/worktrees/` are ordinary git worktrees, locked by trix. Before uninstalling, `trix worktrees rm <name>` (or `trix worktrees prune`) removes them safely. Afterwards, by hand: `git worktree unlock <path>`, `git worktree remove <path>`, then `git branch -d trix/<name>`. The tips of deleted branches stay under `refs/trix/removed/` until `trix worktrees prune` drops them after 30 days, or you do (`git for-each-ref refs/trix/removed/`, then `git update-ref -d <ref>`).
   - Hooks or plugins you added to Grok, Gemini, Antigravity or Amp yourself ([Supported agents](#supported-agents)).

### Quick start

```sh
trix                      # open the workspace (or attach to the running one)
trix claude codex shell   # …and start these agents in the current folder
                          # (also grok, opencode, cursor, copilot, muse, gemini,
                          # antigravity, amp)
trix attach               # attach only; fails if nothing is running
trix view                 # watch read-only from another terminal (q leaves)
trix ls                   # agents, states and activity, without attaching
trix ls --json            # the same, for scripts and agents
trix kill                 # stop the workspace and every agent in it

trix -s work              # the same, for a named session (works with every command)
trix kill --all           # stop every running session
```

`alt+n` starts an agent from inside trix, `alt+?` lists every key, and `trix --help` every command.

`trix` starts a background server on first use (`$XDG_RUNTIME_DIR/trix/session.sock`, log in `server.log` next to it) and attaches to it. One terminal is in control at a time: attaching from a second one takes over from the first. To look without taking over, use `trix view`. The server keeps the environment of the terminal that started it, so agents launched later inherit that `PATH`.

### Grid, pins and selection

- **Grid.** `alt+g` tiles your agents live, up to four at a time: two to a row, and three as two on top and one below. The focused tile is accented, blocked tiles turn red, and the rest fade back a little. With more agents, the grid shows the page with the focused agent, and `alt+j`/`alt+k` move through them. The sidebar keeps full cards and scrolls, or shows one-line cards after `alt+c`.
- **Pins.** Every pane's title starts with a red pin (`⚲`). Click it and that agent moves to the top of the sidebar, with the pin on its card and *pinned* on its title. Up to two agents can be pinned, in the order you pinned them, and pinned cards stay put while the rest of the sidebar scrolls under them. Click the pin again to unpin it, and it goes right below the ones still pinned. The pin is the same red in every theme, and pins come back with a restored workspace. With a Nerd Font, `[ui] nerd_font = true` draws it as `` (see [Config](#config)).
- **Selection.** Alt+click an agent's card, or press `alt+x` on the focused one, to select it; do it again to unselect it. Selected cards are tinted and checked, and the sidebar's header counts them (click its `✕` to unselect them all). While any are selected, the keys act on all of them:
  - `alt+w` closes them, asking once if any still runs.
  - `alt+g` shows only them in the grid, which follows the selection as it changes, and `alt+j`/`alt+k` go round them there.
  - `alt+b` sends to them first (`tab` still picks every agent or the idle ones).
  - `alt+o`, or a right-click on a selected agent, opens their menu: restart the exited ones, copy all their folder paths, unselect all, close them.

  Rename, pin and jump stay one agent's. The selection isn't saved with the workspace. On macOS, Cmd+click a card in iTerm2 and Terminal.app, where Option-click doesn't reach trix; in other terminals Option-click works if the terminal passes it on (see [macOS](#macos)). `alt+x` works anywhere.

### Search

`alt+f` opens a search over every agent's scrollback, including exited ones. The last 10,000 lines of each are kept (`[ui] scrollback`, see [Config](#config)).

- Results update as you type, one row per matching line, newest first. They're grouped by agent, in sidebar order. Each row shows how far back the line is (`↑340`, `↑2.1k`, `on screen`). An agent shows at most 200 lines; a `+` after the count means some were left out.
- Smart case: a lowercase query ignores case, and any capital makes the match exact.
- Lines that wrapped on screen are searched as one line, so a match that wraps across rows is still found.
- `↑` `↓` (or `tab`, `ctrl+n` `ctrl+p`) select a result, `pgup` `pgdn` move by ten, `enter` jumps, `esc` closes.
- Jumping focuses the agent, scrolls the line into view and selects the match. New output since the search is accounted for. If the line has already fallen off the scrollback, you're told so. Typing into the agent returns it to the live view.
- Full-screen programs (vim, htop) only have their current screen to search.

### Runnable worktrees

A fresh worktree has only what git tracks: no `.env`, no dependencies, and every agent's dev server wants the same port. A `.trix.toml` at the root of the repository, committed, says what a new one needs:

```toml
[worktree]
setup = "npm ci"            # run in the new worktree, in the agent's pane, before the agent
copy = [".env", ".env.*"]   # gitignored files to bring from the main checkout
ports = 10                  # how many ports each worktree gets (1 to 100)
```

- **Setup** runs with `sh -ec`, with no stdin, in the agent's pane, so you watch it. The agent starts when it succeeds. If it fails the pane exits and no agent starts: Enter runs it again and keeps your first prompt. To go on without it, close the pane and start an agent with the same name and Worktree, since an existing worktree isn't set up again. Setup runs only for a worktree that was just created, never on a restart that follows a success, a restore or a layout. A server crash doesn't lose track of it: the next server knows whether it finished, and if not, Enter still runs it and the first prompt still follows. It gets `TRIX_ROOT`, the main checkout, and the port variables.
- **Copy** takes paths inside the repository, with `*` and `?` in each part. It brings only regular files that git ignores in both the main checkout and the new worktree, because anything else could land in the agent's commit. A tracked file a pattern also matches, like a committed `.env.example`, is already in the worktree, so it's passed over without a word and doesn't count. It never follows a symlink or overwrites a file, skips `.git` and `.trix`, keeps each file's mode, and stops after copying 1000 files or 64 MB. Anything else it leaves behind is named in a toast.
- **Ports.** Each worktree under `.trix/worktrees/` holds its own block, exported in its panes as `TRIX_PORT` (the first), `TRIX_PORT_END` and `TRIX_PORT_COUNT`, so `npm run dev -- --port $TRIX_PORT` never collides. This needs no `.trix.toml`. Blocks skip ports something is listening on, and are recorded in `$XDG_STATE_HOME/trix/ports.json`, shared by every session. Agents in the same worktree share its block. A restart, a reattach, and a quit followed by a restore keep it. Closing the last agent in the worktree, cleaning it up from the review, starting fresh instead of restoring, or quitting with nothing there to restore gives it back. If the range runs out, the agent starts without ports and a toast says so.

**Locked.** Every worktree trix makes is locked with `git worktree lock`, its reason starting with `trix:`. git then refuses to remove it, even with `--force` given once, keeps it when `git worktree prune` runs after its folder was deleted, and won't delete its branch. One from an earlier trix gets locked when an agent next starts in it. To remove one, use `trix worktrees rm <name>` or the review's `X`, which unlock it only once nobody uses it. A lock whose reason doesn't start with `trix:` is never touched. A lock that fails is logged in the server log and never stops the agent.

**`trix worktrees`** lists trix's worktrees in the repository you're in: each one's branch, when it was last used, what it is (in use by whom, uncommitted files, commits on no branch, merged and how, commits not in the default branch, folder missing, locked by someone else), and what `prune` would do with it. It also lists the `trix/*` branches with no worktree, and the deleted branches' tips under `refs/trix/removed/`. `--all` looks in every repository trix knows of: where sessions' agents are or would come back, the ports ledger, and the folders you've started agents in. `--json` is for scripts and agents.

- `trix worktrees prune` removes what holds no work, and nothing anyone uses:
  - worktrees that are clean and merged: the folder, then the branch;
  - entries whose folder is gone and that carry trix's lock: the entry, then the branch if it's merged;
  - `trix/*` branches with no worktree that are merged;
  - tips under `refs/trix/removed/` older than 30 days.

  `--folders` also removes clean worktrees whose branch isn't merged, keeping the branch. Anything used in the last day is left alone (the newest of the worktree's folder and its own git files; `--min-age 12h` changes it, `--min-age 0` turns it off).
- `trix worktrees rm <name>` removes one worktree, the same way the review's `X` does: the folder, and the branch if it's merged (`--keep-branch` keeps it). When it won't, it says why.
- **How they remove.**
  - Both show the plan first and ask, with no as the default. Without a terminal they do a dry run unless given `--yes`, and so does `--json` without `--yes`.
  - Each removal looks again right before it's done, at who uses the worktree and what it holds. Nothing is forced, and a lock that isn't trix's is never touched.
  - Ignored files go with the folder, as with `git worktree remove`, so the plan names them.
  - Deleted branches' tips are kept and logged as for `X`.
  - Exit codes: 0 done (or nothing to do), 1 something was refused or failed, 2 a usage mistake, 3 nothing done (a dry run, or no).

**Trust.** `.trix.toml` arrives with the repository, so it's someone else's commands until you've read them. Nothing in it is used until you accept it in the new-agent form (`alt+n`, with Worktree ticked), which shows exactly what would run: `y` trusts it and runs it, `n` or Enter starts this agent without it, `esc` goes back to the form. Any change to the file asks again. The file is read from the main checkout, so an agent's edits in its own worktree don't count. There's no command to grant trust: `trix spawn --worktree` on an untrusted repository makes the worktree without setup or copies, and says so on stderr and in a toast. Trust is recorded in `$XDG_STATE_HOME/trix/trusted-repos.json`. That's a guard against surprises and not a sandbox: an agent running as you, without a sandbox, could edit that file directly.

### Review

`alt+v` (or *Review diff* in the agent's menu, or a click on the diff count on its card) opens the focused agent's uncommitted changes against `HEAD`, untracked files included, file by file. Reading it never writes the index, so it can't get in the way of the agent's own `git add`.

- `↑` `↓` (`j` `k`), `pgup` `pgdn`, `g` `G` move; `]` `[` (`tab`) go file to file.
- `enter` or `c` leaves a note on the line under the cursor (a file's header for the whole file, a `@@` line for the hunk), or edits the note already there. `x` drops it, `n` `N` go note to note. Notes stay on their line when you reload (`r`) and the diff around them moves, and go with a line that's gone.
- `s` sends every note to the agent as one prompt: each one with its file, line number and the line itself, then what you wrote. It's guarded like `trix send`: an agent that needs you, a shell or one still starting isn't typed into, and the notes are kept.
- `C` stages everything and commits it with the message you type. If the commit fails (a hook says no, say), what was staged before is put back. `P` pushes the branch to `origin` (or the only remote) under its own name, so a branch that tracks `origin/main` doesn't push to main. `O` pushes and runs `gh pr create --fill`.
- `M` merges a worktree's branch into the branch the main checkout is on, there (`--no-ff`), or with `g` merges its pull request instead (`gh pr merge --merge`), offered once the branch has been pushed. It refuses while there are uncommitted changes, when the main checkout has uncommitted changes of its own, or when there's nothing to merge; a merge that conflicts is aborted, so nothing changes, and the files that conflicted are named.
- `X` cleans up a worktree: it closes the agent, runs `git worktree remove`, never forced, and then deletes the branch if its content is already in the repository's default branch. A worktree with uncommitted changes is refused before anything happens, and so is one whose detached HEAD has commits on no branch. "Already in" is judged by content, so a squash merge counts. It's measured against the default branch (origin's HEAD, else `main` or `master`) and its remote-tracking ref as of your last fetch, never the branch the main checkout happens to be on. A branch counts as merged if any of these holds: the default branch has its tip; merging it would add nothing (git 2.38 and later); or one commit there makes exactly its changes, which is what a squash merge or a cherry-pick leaves. A branch that's only partly merged, or was edited while it was squashed, is kept, and so is one that's pushed but not merged. The `trix/<name>` the worktree was made on goes too, if the agent switched to another branch and it's merged. Before a branch is deleted (`git branch -D`, since `-d` refuses squash merges), its tip is kept under `refs/trix/removed/<branch>/<time>` and logged in `$XDG_STATE_HOME/trix/removed.log`, and the result says how to bring it back: `git branch <name> <sha>`. Nothing is fetched, and remote branches are never touched. X also keeps a worktree anyone still uses, and the refusal names them: another agent of this session, by where it started or where it is now; another session's agents, or a shell of theirs that `cd`'d in; a restore offer nobody has answered; a stopped session's saved workspace that would bring an agent back into it; agents a stopped server left running; and a session that doesn't answer. Only then does X take off trix's lock on the worktree (see [Runnable worktrees](#runnable-worktrees)) and remove it; if git still refuses, the lock goes back on. A lock that isn't trix's stays, and X says to unlock it first.
- Closing the last agent in a worktree offers the same cleanup without opening the review. The close dialog (`alt+w`) checks the worktree in the background, by the same rules as X: "checking its worktree…" until it knows, and then one line. Clean and merged: `r` closes the agent and removes the worktree and its branch. Clean, with commits not in the default branch: `r` removes the folder and keeps the branch. Uncommitted work, commits on no branch, or anyone else using it: no `r`, and the line says why it stays. `y` and Enter still only close, and a `y` typed before the check answers is never an `r`. Closing several agents at once sums it up in one line ("2 of their worktrees are clean and merged"), and `r` removes only those. An agent that has already exited closes without asking, as before; if it leaves a clean worktree nobody uses, a toast says so: `kept worktree w (clean, merged): alt+v X, or trix worktrees rm w`. Quitting, `trix kill`, detaching and a crash never remove anything. `[worktree] on_close = "keep"` turns the offer and the toast off ([Config](#config)).
- Each git key looks again at the branch and remotes first, so a remote added while the review is open counts without a reload. Push, PR, merge and clean up ask first. Every step runs in the background and says how it went at the foot of the review, or as a toast once it's closed. git and gh are never left waiting on a password prompt.

### Clone

`alt+l`, or *Clone* in an agent's menu, starts another agent like the focused one, next to it: the same kind, flags and repo, in a worktree of its own (`.trix/worktrees/<name>-2` on `trix/<name>-2`). The original isn't touched.

- **Its name.** Clones are numbered after the first original: a clone of `trix` is `trix-2`, and a clone of `trix-2` is `trix-3`, never `trix-2-2`. The number is the first one from 2 that no agent here has, with no worktree folder or `trix/` branch left over, and that no other session is in. A name that only looks numbered, like `issue-1234`, is kept whole (`issue-1234-2`): what counts is which agent it was cloned from, not its digits. A renamed clone is still numbered with its family, and that holds across a restore and a server restart. When numbers were passed over, the toast says why: `cloned trix as trix-5 (trix-2…4 are leftover worktrees: trix worktrees)`.

- **Its code.** The new worktree starts at the original's commit, with its uncommitted changes copied over: what's staged stays staged, and files git doesn't track yet come too. Ignored ones (`.env`, build output) don't, unless the repo's `.trix.toml` copies them, as it does for any new worktree.
- **Its conversation.** Claude, Codex and Grok go on from a copy of the original's conversation (`--fork-session`, `codex fork`), so the clone knows what the original knew, and the two go their own ways from there. Other agents start a new conversation, and say so.
- **Its cost** counts only what the clone spends itself, not the conversation it was copied from. That holds across a restore and a saved [layout](#layouts).
- **Restoring it.** A clone is saved with its own conversation only once it has one. Restored before then, it starts a new conversation rather than going on in the original's.
- **Cleaning it up.** Closing the clone keeps its worktree and branch. `alt+v` then `X` on the clone closes it and removes both, the branch once it's merged ([Review](#review)).
- Outside git, the clone shares the original's folder.

### Agent control

Eight commands drive a running workspace from any shell. They're meant for scripts and for agents managing other agents:

```sh
trix spawn claude --name api --worktree --prompt "add rate limiting to /login"
trix send api "also cover it with a test"     # or pipe the prompt in on stdin
trix wait api                                 # blocks until api stops working
trix read api -n 80                           # the last 80 lines of its screen and scrollback
trix ls --json                                # every agent: state, activity, branch, diff, last prompt…
trix close api                                # --clean also removes its worktree, if that's safe
trix events --json                            # follow every change, live: one JSON object per line
trix args api --model claude-opus-5-5         # change the flags api starts with next time
```

- `<agent>` is an agent's name, or its number in `trix ls`. Names are safer, because numbers shift when an agent closes. `spawn` prints the new agent's name and folder. It refuses a name that's already taken, and it never steals focus from the agent you're looking at.
- `wait` takes one or more agents and returns when one of them stops working (`--all`: when every one has). It prints one `name<TAB>state<TAB>activity` line per agent, where the state is `done`, `idle`, `needs-you` or `exited`. A prompt just sent with `send` counts as work until the agent's turn starts, so `send` followed by `wait` waits for that turn. After `-t` seconds (default 300, `0` waits forever) it exits with 124, and you can run it again. A shell counts as done when it's back at its prompt. Builtins and shell functions run inside the shell itself, so `wait` can't see them.
- `events` follows the workspace live, the way `wait` would if it never returned. It starts with one `present` line per agent, then prints a line per change: `spawned`, `state` (with the state it left), `renamed`, `restarted` (a new process in the same place, with a new `id`; `prev_id` is the old one) and `closed`. Name agents to follow only those. Without `--json` it prints a line a person can read (`14:02:11  api  working → needs-you  approve Bash rm -rf dist`). With `--json`, each line is an object with `event`, `at` (Unix ms), `session`, `id`, `number`, `name`, `kind`, `state`, `prev`, `activity`, `unseen`, `cwd` and `managed_by`, the same states as `wait`. It exits 0 when the workspace stops. A reader that stops reading isn't waited for: once 1024 events are queued for it, trix drops it, and it exits 1 saying it fell behind.
- `args` shows an agent's extra arguments, replaces them (everything after the agent, flags included; one quoted word is taken as written) or removes them with `--clear`. The change is saved with the workspace and takes effect at the agent's next start, such as a restore or a saved layout. The process running now keeps its old flags. An agent may change its own, which is the usual way to fix a typo in your launch flags without closing the pane. Shells take none.
- `close` keeps the agent's worktree, and when it was the last agent in one, says so on stderr, with what it is: clean and merged, `kept worktree w (clean, merged): trix close --clean next time, or trix worktrees rm w now`; clean with commits not in the default branch, the same, and that removing it keeps the branch; otherwise why it stays (`kept worktree w: it has uncommitted work …`), with nothing offered to remove it. `close --clean` closes the agent and then removes its worktree the way the review's `X` does, with the same checks: the folder, and the branch if it's merged. When something keeps the worktree (uncommitted work, commits on no branch, anyone else using it), the agent is still closed, the worktree stays, and it exits 1 with the reason.
- Exit codes: 0 ok, 1 refused or failed (the reason goes to stderr), 2 bad usage, 124 `wait` timed out.

Guard rails, enforced by the server:

- `send` refuses an agent that **needs you**, because its Enter would answer that prompt, for example approve a `rm -rf`. It also refuses a **shell** (the prompt would run as a command) and an agent still **starting** (it may be on a trust dialog). `--force` overrides the first two. Answering prompts is left to you on purpose.
- An agent can't `send` to, `wait` for or `close` itself. trix knows which pane is asking from its `TRIX_AGENT_ID`.
- `close` won't stop an agent that is working or needs you without `--force`.
- A command running inside an agent's sandbox (such as Codex's) can't use trix to act outside it. It can `ls`, `read`, `wait`, follow `events`, `send` a prompt, `close` an idle agent and `spawn` agents as they normally start. It can't start a shell or a custom command, `spawn` with `--args`, `send --force`, `close --force` or `--clean`, change `args`, open a layout, attach, or `trix kill`: trix would do those outside the sandbox, so it refuses them and says why. trix asks the kernel which process is on the other end of its socket, and checks whether that process is in a sandbox the workspace isn't in (macOS: `sandbox_check`; Linux: no_new_privs, seccomp filters or a user namespace of its own). A sandbox stays with a process for good, so forking, or being orphaned to escape the pane, doesn't get around it. A prompt it sends is still only a prompt: the agent that gets it decides what to do, with its own permissions.
- Every `send` shows a toast (`lead → api: also cover it…`). Agents that another agent started or prompted show `‹lead` next to their name in the sidebar.

To make a manager, start a Claude pane and tell it what it's for. For example:

> You're the lead for this repo. Don't edit code yourself. Split the task into independent pieces. For each piece, start a worker with `trix spawn claude --worktree --name <piece> --prompt "<brief>"`. Then loop: `trix wait <all workers> -t 280`, `trix read <worker>` to check its work, and `trix send` follow-ups. If a worker shows `needs-you`, tell me which one and why, and leave it alone. When they're all done, summarise each worktree's branch and diff (`trix ls --json`).

Inside a pane, these commands act on that pane's session. Elsewhere, add `-s <name>` for a named session.

### Triage

`trix triage <PR>` (a number or a URL) starts a **case**: three agents, each with one job, take a pull request from findings to a merge, and trix itself runs the case. No lead agent and no instructions of your own are needed. `trix triage <issue>` works too: see [From an issue](#triage-issues) below.

```sh
trix triage 123                # opens the case's pane, triage-123, and starts the reviewer
trix case ls                   # every case and where it is
trix case show triage-123      # its findings, what was done to each, its log (--json too)
trix case approve triage-123   # merge it, once it's at the gate (or press y in its pane)
trix case abort triage-123     # stop it (or press a)
trix case answer triage-123 n  # answer any other question it asks, as in its pane (a merge is only ever approve)
```

1. **The reviewer** (Claude) gets a detached checkout of the PR's head and reports what's wrong: each finding with its file, line, severity and how it fails. It doesn't edit anything. Then it checks its own review: for each finding it re-reads the code and confirms it with evidence (a failing test or command and its output, or the lines that show it), or retracts it. Only confirmed findings reach the fixer; retracted ones stay in `trix case show`.
2. **The fixer** (Claude) gets a worktree on `trix/<case>-fix`, starting at the PR's branch. It keeps the PR's work and fixes or completes it in place: each finding fixed with a test, or not a bug, or won't fix, or `needs_you` (too risky or large to do safely, or a design choice), with why. It pushes to the PR's branch (`git push origin HEAD:<branch>`, never forced). trix checks its commits are on origin before it takes the report.
3. **The gatekeeper** (Claude) is a separate agent in a fresh checkout of what was pushed, and never sees the fixer's reasoning: only the findings and the fixer's report. It checks each fix, looks for anything the fixes broke, and reads CI: pass, or changes. Changes go back to the fixer with the reasons, for up to `max_rounds` rounds.
4. **The gate**: trix waits for CI on the commit that was checked (not whatever the branch holds now), and asks you. A PR that had checks when the case began is waited for until checks show up on the pushed commit, so "none yet" never reads as "no CI". Your `y` merges the PR, squashed, and only the checked commit (`gh pr merge --match-head-commit`), with CI checked again first. When the reviewer found nothing and you sent it to the gate, the checked commit is the one the reviewer read. Red CI at the gate becomes a finding for the fixer like any other, and anything the fixer left as `needs_you` is named in the question, so it isn't dropped. Then trix closes the case's agents and removes their worktrees, those that are safe to remove.

- **Hand-offs are reports, not chat.** Each agent hands in JSON with `trix case submit <case> <role>`, on stdin. The server checks it against the case before taking it (whose turn it is, every finding answered once, commit ids that are commit ids, a pass with nothing failing), and a report that doesn't fit goes back to its agent with what's wrong. This works from Codex's sandbox too. An agent whose permissions refuse a heredoc (Claude's allow rules don't match one) is told to write the JSON to `.trix-<role>.json` in its folder and hand it in with `--file`; trix deletes the file once it's taken.
- **The case's pane** is where it asks you: it shows as needing you, like an agent, with the reason (`triage-123 · ready to merge PR #123`), and you answer there with a key and Enter. It asks when the reviewer finds nothing (`m` goes to the gate anyway), when the rounds are used up (`r` for one more), when the case's budget is spent (`b` adds it again), when an agent stops twice without its report (`n` nudges it, or starts it again if it exited), and at the merge. `a` aborts at any of these, and leaves the PR and the worktrees as they are.
- **Who may do what.** trix tells who's asking by the process, not its environment: it walks the asking process's parents to the pane it runs in. A report is taken only from its role's own agent, so the fixer can't hand in the gatekeeper's verdict. `trix case approve` and `abort` are refused from inside any agent's pane (a shell pane is yours, and works), and so is any `trix send` from an agent to a case's pane, `--force` or not. An approve or abort is typed into the case's pane by the server, the one place the case takes answers from. This holds across sessions: `trix -s other case approve …` from an agent's pane in another session is refused too, naming the agent and its session. A command from none of the case session's panes makes its server ask every other running session whose pane it's in. If a session doesn't answer (busy, or an older trix), an approve, abort, answer or send into a case's pane is refused, and the error says why. A report is still taken, since its role's check doesn't depend on that answer. A process that leaves its pane (forked twice, so its parent is init) is still known by its session; one that also starts a session of its own isn't: this is best effort against a process that sets out to leave. trix's agents are never told to merge. An agent that isn't sandboxed still runs commands as you, though, `gh pr merge` included: what trix holds to is that its own merge is of the checked commit only.
- **Permissions.** The role agents start with your normal Claude permission settings, so an agent may stop and ask in its own pane, and the case waits until you answer there. To run a case unattended, give the roles a permission mode and an allowlist. Here it is for one role; the same `args` go in `[triage.builder]`, `[triage.reviewer]`, `[triage.fixer]` and `[triage.gatekeeper]`:

  ```toml
  [triage.fixer]
  agent = "claude"
  args = ["--permission-mode", "dontAsk", "--allowedTools",
          "Read,Edit,Write,Glob,Grep,Bash(git:*),Bash(trix case submit:*),Bash(trix case show:*),Bash(gh pr checks:*),Bash(gh pr view:*),Bash(gh pr diff:*),Bash(cargo test:*)"]
  ```

  `Bash(cargo test:*)` stands for your project's own test command (`Bash(npm test:*)`, `Bash(pytest:*)`, …): add yours, per project, or the agents can't run the tests. With `dontAsk`, anything not allowed is refused rather than asked, so a refused command can make an agent hand in less (a report that says it couldn't run the tests, say). `Bash(git:*)` lets an agent run any git command; narrow it (`Bash(git diff:*)`, `Bash(git commit:*)`, `Bash(git push origin HEAD:*)`, …) if you want.
- **What it costs** is the case's agents' spend, against `[triage] budget` ($10 by default); the reviewer and the gatekeeper close as soon as their report is in.
- Each case is a folder in `~/.local/state/trix/cases/` (`$XDG_STATE_HOME` if set). Its pane is restored like any agent, and picks the case up where it was.
- For now, a case takes a PR whose branch is in the same repository (not a fork), on GitHub, with `gh` signed in.

<a id="triage-issues"></a>**From an issue.** `trix triage 12` looks up #12 (GitHub numbers issues and PRs together), and a closed issue is refused. For an open one, trix finds the PRs that would solve it, the same way every time: those linked to close it, open ones whose title or body mentions `#12` or the issue's URL, and those its timeline says refer to it. A single formal link is triaged as above, with the issue on the case (`trix case show` names both). Any other PR that may solve it, even just one, is yours to pick: the pane lists each with how it was found (linked, mentions #12, referenced from the timeline), and `1` to `9` picks one, `n` means none of these (a builder makes one). With none, a fourth role starts first, **the builder** (Claude, `[triage.builder]`): it implements the issue with tests on a new branch, `trix/issue-12-<title>`, from the default branch, and pushes. trix, not the builder, opens the PR (`Fixes #12`, plus the builder's summary), and the case goes on from the review. Until then the case is `triage-issue-12`.

With an issue, the review also asks whether the PR solves it (yes, partly or no, with what's missing and what was run), and what's missing becomes an important finding. The gatekeeper passes only when it's solved on the checked commit. If the reviewer says no, the pane asks: `f` fixes it in place, `b` hands the issue to a new case where a builder starts a fresh PR (the old one stays open), `a` aborts. The issue's text is anyone's on a public repository, so the agents get it fenced and marked as data, not instructions. If any of it is from outside the project (not an owner, member or collaborator), the pane asks before any agent gets it, whether a builder or the reviewer of a linked PR comes first (`s` goes on; asked once per case). The case's log keeps to findings and file:line, never the issue's text, and the budget covers the builder too.

### Layouts

A layout is a named set of tabs you can open again whenever you like: the three repos you always work in, a shell per service, a dev server.

```sh
trix layout save work-layout          # the tabs open now: kind, name, folder, extra args
trix layout open work-layout          # open them again, with fresh conversations
trix layout open work-layout --resume # …or pick up the conversations they had when saved
trix layout ls                        # saved layouts and their tabs
trix layout rm work-layout
```

- Layouts live in `~/.config/trix/layouts/<name>.toml` (`$XDG_CONFIG_HOME` if set), one plain TOML file each, meant to be edited by hand or by an agent:
  ```toml
  [[agent]]
  kind = "claude"            # claude, codex, grok, opencode, cursor, copilot, muse, gemini, antigravity, amp, shell or custom
  name = "api"
  cwd = "~/code/api"         # a relative folder starts from your home folder
  args = "--model opus"      # optional: extra arguments (for custom, the command)
  session = "0a955a0a-…"     # optional: what --resume picks up
  ```
- `save` records where each shell is now, even if you `cd`'d since. It won't overwrite an existing layout without `--force`.
- `open` adds to the workspace you're in and skips tabs that are already open under the same name, so opening twice is harmless. New tabs never steal focus. A tab whose folder is gone is reported and skipped.
- From a terminal outside trix, `open` starts the workspace if needed and attaches to it, unless a tab failed to start: then it reports why and leaves attaching to you. From inside a pane, it just adds the tabs, so an agent can run it too: "open my work layout". `trix -s <name> layout open …` from a pane starts that session if needed and opens the tabs there.
- `--resume` only applies to Claude, Codex, Grok, OpenCode, Cursor, Copilot, Muse, Gemini, Antigravity and Amp tabs that have a saved `session`. Everything else starts fresh either way.
- A clone's entry also gets `[[agent.inherited]]` tables: what its conversation had already cost when it was cloned. With `--resume`, its pane counts only what it spent itself, as it did before, since the original already counted the rest.
- How this differs from [Restore](#restore): restore brings back whatever was open after a crash or restart, and it's per session and one-shot. A layout stays until you remove it, and you can open it into any session, any number of times.

### Settings

`alt+,`, or a click on *settings* in the bottom bar, opens the settings:

- **Theme** opens a list of themes. At the top is no theme of trix's own: Omarchy's, followed as it changes, or without Omarchy your terminal's colors. Then come the 24 themes trix ships, all of Omarchy's plus Cyber Aura, and with Omarchy any other theme it has. Moving through the list tries each one on, panes included. `enter` keeps it, and `esc` goes back to the theme you had. A theme you keep is written to `~/.config/trix/colors.toml` (see [Themes](#themes)).
- **Theme in agent panes** is on by default: a theme of trix's own paints the panes, and Claude, Grok and OpenCode take it too. Off, trix's bars and sidebar keep the theme, but the panes keep your terminal's colors (and its window transparency), and the agents keep their own themes. It does nothing while trix follows Omarchy's theme, which themes the terminal itself.
- **Needs-you strip** sets the color of the bar that says another agent needs you: the theme's red, orange, yellow, green, cyan, accent or magenta, so it follows a theme switch, or the `#rrggbb` your config names.
- **Sidebar** (wide, narrow or icons) and **sidebar cards** (full or compact) do the same as `alt+m` and `alt+c`, and are remembered the same way.
- **Nerd Font icons** is off by default. On, the pane pin comes from a Nerd Font. The row shows the pin as it will look, so if it shows as a box, your font isn't a Nerd Font: leave it off.
- **New ones start as** picks the agent `alt+n`'s form starts on: claude (the default), codex, grok, opencode, cursor, copilot, muse, gemini, antigravity, amp, shell, or *last*, the one you started last. **New ones in a worktree** ticks the form's worktree box to begin with.
- **Bring back the last workspace** is what a fresh trix does with the workspace saved last time: *ask* (the default), *always* bring it back, or *never*, starting fresh (see [Restore](#restore)).
- **Notify me** sets whether you hear about an agent you're not looking at when it needs you and when it's done, and whether that comes as a desktop notification, with the terminal bell, or both.
- **Sounds** sets the sound for the same two events: one of trix's own (chime, bell, drop, knock or marimba), off, or the sound file your config names. Each one plays as you pick it.

Use `↑↓` to choose, `enter` or `←→` to change and `esc` or `alt+,` to close. Clicking a setting changes it too. Each change is written where you'd make it by hand, so the files and the settings always agree: the theme goes to `colors.toml`, the sidebar to `prefs.json`, the rest to `config.toml`. Only the line that changed is touched. Your comments and layout stay, and a setting you never moved from its default stays out of the file. If `config.toml` has a mistake, the settings say so and leave the file alone until you fix it. A `config.toml` that is a symlink, into a dotfiles repo for example, is written through the link. Every running trix picks up the change within a second.

Keys, muted agents, a notify command, the strip's text color, the new-agent form's kind row, budgets and prices aren't in the settings; set them in `config.toml`.

### Config

`~/.config/trix/config.toml` (`$XDG_CONFIG_HOME` if set) changes keys, notifications, sounds, the needs-you strip, how new agents start, the theme in the panes and restoring the workspace. Every setting is optional, and without the file trix works as described above. trix reloads the file within a second of each save and says so in a toast. If the file has a mistake, the toast names the line and trix keeps the previous settings, then tells you when it's fixed. `trix config` checks the file from the command line. The [settings](#settings) (`alt+,`) write most of these here for you. A `colors.toml` next to it sets the theme (see [Themes](#themes)).

```toml
[keys]
new = "alt+t"                     # one key…
next = ["alt+j", "ctrl+down"]     # …or several
jump = "ctrl+1..9"                # jump is the only one that takes a range
grid = []                         # none: alt+g then reaches the agent

[notify]
on = ["needs-you"]                # which events notify: needs-you, done (both by default)
desktop = true                    # the desktop notification
bell = true                       # the terminal bell: the urgency hint while trix is unfocused
command = "~/bin/ping-phone"      # also run this, for each notification

[sound]
done = "marimba"                  # trix's own: chime (default), bell, drop, knock, marimba; or "off", or a sound file
needs_you = "~/sounds/ping.ogg"   # off by default
mute = ["codex", "scratch"]       # agent kinds or names that never make a sound

[strip]
color = "#e0af68"                 # the needs-you strip: "#rrggbb", or a theme color like "yellow"
text = "bg"                       # optional: its text (by default whichever reads best)

[new]
agent = "codex"                   # what alt+n starts on: claude (default), codex, grok, opencode, cursor, copilot, muse, gemini, antigravity, amp, shell, custom, or "last"
worktree = true                   # the worktree box ticked to begin with (off by default)
kinds = ["claude", "codex", "opencode", "shell"]  # alt+n's kind row; the rest go under "more ▸"

[theme]
panes = false                     # keep the panes in the terminal's colors (on by default)

[ui]
nerd_font = true                  # draw icons from a Nerd Font (off by default: plain Unicode)
scrollback = 5000                 # lines each pane keeps to scroll back through and search (10000 by default, 0 to 100000)

[workspace]
restore = "always"                # a fresh trix and the saved workspace: "ask" (default), "always", "never"

[cost]
budget = 25                       # USD for the whole workspace (none by default)
agent_budget = 5                  # USD for each agent (none by default)
monthly_budget = 300              # USD for a calendar month, every session's agents together (none by default)
warn_at = 80                      # warn this far into a budget, in percent (80 by default)
live_prices = false               # don't fetch the live price list: config.toml's prices and trix's own only (on by default)
prices_url = "https://example.com/prices.json"  # fetch the list from here instead, in LiteLLM's format

[cost.prices."gpt-6.1-sol"]       # USD per million tokens, for a model trix has no price for, or to correct one
input = 2
output = 10
cache_read = 0.1                  # optional: a tenth of input by default
cache_write = 2                   # optional: input by default (cache_write_1h too, for Claude's hour-long writes)

[triage]
budget = 10                       # USD a case may spend before it asks you (10 by default)
max_rounds = 2                    # fix rounds before it asks you (1 to 10, 2 by default)
[triage.gatekeeper]               # also [triage.builder], [triage.reviewer] and [triage.fixer]
agent = "claude"                  # which agent takes the role (all four: claude by default). agent = "codex" works too, but Codex
                                  # asks to trust each new folder first, so the case waits for you there
args = ["--model", "claude-sonnet-5-5"]  # its extra arguments

[worktree]
ports = true                      # give each trix worktree a block of ports (on by default)
port_base = 20000                 # where blocks start (1024 to 65535)
port_count = 10                   # ports per block, unless the repo's .trix.toml says (1 to 100)
on_close = "ask"                  # closing a worktree's last agent offers to remove it if safe; "keep" never offers

[session]
idle_after = "8h"                 # when `trix ls` flags a session left alone: like 30m, 8h or 1d (default), or "off"
```

- **Keys.** The actions are `new`, `jump`, `next`, `prev`, `attention`, `grid`, `sidebar`, `mini_sidebar`, `compact`, `search`, `broadcast`, `review`, `clone`, `rename`, `close`, `menu`, `select`, `detach`, `help`, `settings` and `quit`. Naming an action replaces its keys; the actions you leave out keep theirs.
  - A key is `alt`, `ctrl` and/or `shift` plus a letter, a symbol, a named key (`up`, `pgdn`, `enter`, `space`, `plus`…) or `f1`–`f12`. It needs `alt` or `ctrl`, so it can't swallow what you type into agents; only an F-key may stand alone. `super`/`cmd` is refused, because terminals don't pass it on.
  - A key you bind is yours: an action that has it only by default goes without it (with `new = "alt+l"`, clone has no key), so a default trix adds later never breaks your file. Two of your own bindings on one key are an error, and so is a key in jump's range.
  - The footer, the help (`alt+?`) and the other hints show the keys you chose.
- **Notifications** cover agents you're not looking at, as before. `on` picks the events; `desktop`, `bell` and `command` pick where they go. `command` runs without a shell (as an argv, or a string split like one; `~` works in the program), from your home folder, with the event's JSON on stdin (the same object as `trix events --json`). It also gets `TRIX_NOTIFY_EVENT` (`needs-you` or `done`), `TRIX_NOTIFY_AGENT`, `TRIX_NOTIFY_TITLE`, `TRIX_NOTIFY_BODY` and `TRIX_NOTIFY_SESSION`. For example, `command = ['sh', '-c', 'curl -s -d "$TRIX_NOTIFY_TITLE" ntfy.sh/my-agents']` puts every notification on your phone through [ntfy](https://ntfy.sh).
- **Sounds** follow their own settings, separate from `on`. trix has five of its own: `chime`, short and rising, the default for done; `bell`, one strike of a small bell; `drop`, a water droplet, barely there; `knock`, two soft knocks on a door, made for needs-you; and `marimba`, three rising notes. Their names always mean them, so to play a file of yours named `bell`, write its path: `./bell`. A sound file can be anything `pw-play` (wav, flac, ogg) or `afplay` (also mp3, m4a) plays. A relative path is looked up next to `config.toml`. Sounds within two seconds of each other share one sound, done and needs-you separately.
- **The strip** is the theme's red by default. `color` and `text` take `#rrggbb` (or `#rgb`), or the name of one of the theme's colors (`red`, `yellow`, `green`, `cyan`, `magenta`, `orange`, `accent`, `fg`, `fg_bright`, `bg`), which follows a theme switch. Without `text`, trix picks the theme's dark or light, whichever contrasts more with the strip.
- **New agents** take `agent` and `worktree` as where `alt+n`'s form starts; you can still change both in it. `"last"` is the kind you started last from the form, remembered in `prefs.json`, or claude before you've started one. `kinds` is the form's kind row, in your order: claude, codex, shell and custom by default. Every other kind, including any trix adds later, waits under *more ▸*, and its letter picks it all the same. A name that isn't a kind, or one named twice, is a mistake.
- **`[theme] panes = false`** keeps a theme of trix's own to its bars, sidebar and cards (see [Themes](#themes)).
- **`[ui] nerd_font = true`** draws the pane pin from a Nerd Font (``) instead of plain Unicode (`⚲`). Turn it on only if your terminal's font is a Nerd Font: trix can't see the font, and without one the icon shows as an empty box.
- **`[ui] scrollback`** is how many lines each pane keeps above its screen: what you scroll back through, what search (`alt+f`) looks in, and what `trix read` reads. It is most of what a pane costs in memory, about 24 bytes a character cell. 10,000 lines of a 170-column pane is some 40 MB once it fills up, so a dozen long-running agents can take half a gigabyte between them. Fewer lines use less. A change reaches the panes open now: fewer drops their oldest lines at once.
- **`[workspace] restore`** answers the question a fresh trix asks about the saved workspace (see [Restore](#restore)).
- **`[cost]`** sets the budgets and prices in [Cost and tokens](#cost-and-tokens). A price under `[cost.prices]` applies to the model of that name, or to models whose name starts with it (`"gpt-6"` covers `gpt-6-luna`), ahead of the live list and trix's own. `live_prices` and `prices_url` are for the [live list](#cost-and-tokens).
- **`[worktree]`** sets the ports of [runnable worktrees](#runnable-worktrees). The default base sits below the ports Linux and macOS hand out on their own. What a repository's worktrees run and copy lives in its `.trix.toml`, not here. `on_close` is `"ask"` (the default: the close dialog's `r` and the kept-worktree toast, see [Review](#review)) or `"keep"` (neither). There's no `"remove"`: a worktree goes only when you say so.
- **`[session] idle_after`** is how long a session can go with nobody attached and nothing happening before `trix ls` flags it, and how long an agent can need you before it's flagged more strongly (see [Sessions](#sessions)). A trix from before this key was added can't reload a config that has it.
- **What you tell every agent** (who does what, who reports to whom) goes in `~/.config/trix/instructions.md`, not here: see [Your instructions](#your-instructions).

### Environment variables

Set these before the workspace server starts; a running server keeps the environment it started with.

- `TRIX_NO_NOTIFY=1` turns off desktop notifications, and `TRIX_NO_SOUND=1` every sound, whatever `config.toml` says.
- `TRIX_THEME=path/to/colors.toml` sets the theme, ahead of `~/.config/trix/colors.toml` (see [Themes](#themes)).
- `TRIX_CLAUDE_CONTEXT=<tokens>` sets Claude's context window for the [gauge](#context-gauge).
- `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` and `XDG_RUNTIME_DIR` move trix's folders as they do for other programs.

Inside a pane, trix sets `TRIX`, `TRIX_SOCKET`, `TRIX_AGENT_ID`, `TRIX_AGENT_NAME` and `TRIX_SESSION`, plus `TRIX_PORT`, `TRIX_PORT_END` and `TRIX_PORT_COUNT` in a worktree with [ports](#runnable-worktrees). So any tool can report its state with `trix hook <source>` and a hook-style JSON payload on stdin (e.g. `{"hook_event_name":"Stop"}`). Once a tool reports, trix trusts it and stops guessing from output.

### Themes

trix uses the first theme it finds: the file `TRIX_THEME` points at, then `~/.config/trix/colors.toml`, then Omarchy's current theme, then Tokyo Night. The start screen, with no agents open, and the settings' **Theme** row say which one is in use.

trix ships all 23 of Omarchy's themes plus Cyber Aura, built into the binary and in `themes/`, from Catppuccin to White, so a machine without Omarchy has them too. Catppuccin Latte, Flexoki Light, Lupine, Rose Pine and White are light. To pick one, open the [settings](#settings) (`alt+,`) and choose **Theme**: moving through the list tries each theme on, and `enter` keeps it. With Omarchy, the list also has any theme you added to Omarchy, and the file you keep sets trix's theme apart from the desktop's; to follow Omarchy again, choose **Follow Omarchy** at the top of the list, or remove the file. While `TRIX_THEME` is set, the settings can't change the theme.

From a shell, `install-theme.sh` does the same with a theme's name, run from your trix clone:

```sh
./install-theme.sh ristretto
```

It copies the theme to `~/.config/trix/colors.toml` (`$XDG_CONFIG_HOME` if set), next to `config.toml`, and every running trix restyles within a second, with no restart. Run it again with another name to switch, or with `tokyo-night` to go back to the default. Run it with no name to list the themes.

It also installs an Omarchy theme folder or any `colors.toml` by path, such as `./install-theme.sh ~/Downloads/some-theme`. If the `colors.toml` it replaces isn't one of the shipped themes, because you made or edited it, it saves that file next to it as `colors.toml.<date>.bak` first. Only the colors count, not comments, so Omarchy's own copy of a theme counts as that theme. The settings do the same with a file you made: choosing another theme keeps yours as a `.bak` first. Installing by hand is the same thing: `mkdir -p ~/.config/trix && cp themes/ristretto/colors.toml ~/.config/trix/colors.toml`. Removing the file, or choosing the top of the list in the settings, goes back to Omarchy's theme or, without Omarchy, to the default.

**In the panes.** A theme picked or installed this way colors the panes too, not only trix's bars, sidebar and cards:

- trix draws each pane's background, plain text and 16 terminal colors from it, mapped the way Omarchy builds a terminal's palette, and answers a program that asks the terminal for its colors (OSC 10, 11 and 4) with the theme's.
- Agents that draw their own colors are asked to use the terminal's: Claude Code's ANSI-only theme (`dark-ansi`, or `light-ansi` for a light theme like Lupine, in the settings file trix hands Claude), Grok's `terminal` theme (`GROK_THEME=terminal`) and OpenCode's `system` theme (`OPENCODE_CLI_CONFIG_CONTENT`). Your own settings for them don't change, and a `GROK_THEME` or `OPENCODE_CLI_CONFIG_CONTENT` you set yourself wins.
- Agents take this when they start, so one that was already open keeps its colors until you restart it (`alt+o` › restart). After a theme switch, whatever comes from the 16 colors changes right away. Restart an agent if the rest still looks off, as Claude needs after a switch between a dark and a light theme. A color a program sets exactly (24-bit) stays its own.
- Because trix paints the background, your terminal's window transparency doesn't show through it. To keep the transparency, turn off **Theme in agent panes** in the settings (`[theme] panes = false`): the theme then stays on trix's bars and sidebar, and the panes and agents keep their own colors.
- When trix follows Omarchy's theme, it leaves the panes to your terminal, which Omarchy themes for you, and answers color questions with Omarchy's colors. With no theme at all, the panes are your terminal's.

To make your own theme, edit your copy. It is Omarchy's `colors.toml` format, one `key = "#rrggbb"` per line, and any key you leave out keeps its Tokyo Night color.

### Agent workspace discovery

Trix supplies workspace discovery instructions to Claude through
`--append-system-prompt` and Codex through `-c developer_instructions=…`.
New sessions are instructed to run `trix ls --json`, identify their own pane,
and use Trix commands for authorized coordination. Claude's custom append
prompt is combined with these instructions. So are the `developer_instructions`
in your Codex `config.toml` (the active profile's, else the top level's), which
`-c` would otherwise replace. A `-c developer_instructions=…` in your own Codex
args comes last and replaces both.

OpenCode receives a runtime instruction file through its session configuration;
Copilot receives the same file through `COPILOT_CUSTOM_INSTRUCTIONS_DIRS`,
appended to existing directories. Muse receives context from its own
`SessionStart` hook (when Trix’s managed hooks are enabled), and Gemini from
its own once you've added trix's hooks ([Gemini](#supported-agents)), as does
Antigravity from its. Amp receives it with a thread's first prompt, from
trix's plugin once you've added that ([Amp](#supported-agents)). Grok and
Cursor do not yet receive this context.
Existing conversations may retain their original instructions; start a new
conversation after updating Trix. No user configuration files are modified.

### Your instructions

`~/.config/trix/instructions.md` is what you'd otherwise tell every agent yourself: who does what, who reports to whom, what an agent may do without asking. Every agent gets it after trix's own instructions above, the same way, marked as yours. Where it asks agents to message, report to or hand work to each other, that counts as the go-ahead trix's instructions ask for. It never lifts the rule against `--force`.

```markdown
## Roles
- **alpha dog** reviews every PR the others open, merges the ones that are
  ready (only with CI green), and is the only one who reports to the user.
- Everyone else builds what they're asked, then reports to alpha dog with
  `trix send "alpha dog" "<what you did, the PR link, what's left>"`.
  Ask alpha dog, not the user, when you're stuck. Don't merge your own PRs.
```

- Each agent knows its own name (`trix ls --json` marks it `you: true`), so one file can give every role.
- It's read as each agent starts, so an edit reaches the agents started after it: new ones, restores, layouts, restarts. Those already running keep what they started with.
- It lives with your config, outside every repository, and nothing of it lands in a project.
- Every agent carries it in its prompt on every turn, so keep it short. A file over 16 KB, or one that isn't UTF-8 text, isn't used: agents get trix's instructions alone, a toast says why as they start, and `trix config` checks it. An empty file is no file.
- Starting agents is trix's job, not the file's: nothing reads it until an agent starts. To always have an agent there, keep it in a [layout](#layouts).
- Rules for one project's code (its tests, its style) belong in that repository's `CLAUDE.md` or `AGENTS.md`, which agents read themselves.

### Context gauge

trix follows each Claude and Codex agent's conversation log (Claude's `~/.claude/projects/…/<session>.jsonl`, Codex's `~/.codex/sessions/…/rollout-…-<thread>.jsonl`). It reads only what was added since its last look, at most every two seconds, in a background thread. The figure is the context as of the agent's last turn.

- **Codex** logs its window size, so its gauge is exact.
- **Claude** doesn't log it. trix starts from 200K, and once any conversation of a model goes past 200K without being compacted, it knows that model has 1M. It remembers that per model in `prefs.json`. Until then a 1M model reads fuller than it is, which is why its pane says `~` before the number left. To set it outright, start trix with `TRIX_CLAUDE_CONTEXT=1000000`. The `[1m]` model variants are recognised as 1M.
- In `trix ls --json`, each agent gets `"context": {"used", "window", "left", "used_percent", "window_exact"}`, or `null` before its first turn.


### Cost and tokens

The same logs the [context gauge](#context-gauge) reads have every turn's tokens, so trix adds them up for each Claude and Codex agent and prices them per model.

- **Claude**: every model call in the conversation, its subagents' too (`<session>/subagents/*.jsonl`). Claude writes a line per part of a reply, each with the reply's usage, so each reply counts once. Input, cache writes (5-minute and hour-long apart), cache reads and output each have their price; fast mode is twice the price, and so is a long prompt where the model charges more for one (Haiku 5.5, over 100K). After `/clear`, what the earlier conversation spent stays the agent's.
- **Codex**: the running total in its log (`token_count`), each turn's share at the price of the model the turn ran on (`turn_context`).
- **Prices** are list prices, from three places, first one first: `[cost.prices]` in `config.toml`, then the live list, then trix's own table. For an agent on a subscription, the figure is what the same work would cost on the API, not what you're billed.
- **The live list** is [LiteLLM's](https://github.com/BerriAI/litellm) public list of model prices (`model_prices_and_context_window.json`). trix fetches it about once a day, and at startup when its copy is a day old, with `curl`, in the background with a 30-second timeout, so nothing waits for it. It keeps Anthropic's and OpenAI's own entries, not Bedrock's, Azure's or a reseller's, with their input, output, cache read, cache write and hour-long cache write prices, and the higher prices some models charge past a prompt size (`_above_200k_tokens` and the like). Names meet whatever their prefix or date: `anthropic/claude-sonnet-4-5-20250929` is the `claude-sonnet-4-5` an agent logs, but `claude-opus-5-5` is never taken for `claude-opus-5`. The copy is kept in `~/.cache/trix/prices.json` (`$XDG_CACHE_HOME` if set). A download that fails, or isn't a price list, leaves the last good copy in use, and trix tries again an hour later. `[cost] live_prices = false` stops the fetching and leaves the list unused; `prices_url` fetches it from somewhere else (see [Config](#config)).
- **trix's own table** has Anthropic's models and the OpenAI models Codex used before GPT-6 (`gpt-5`, `gpt-5-codex` to `gpt-5.3-codex`), for when trix has never fetched the list. A model with no price anywhere still has its tokens counted. Its cost is left out, the figure gets a `+` (`$4.20+`), or shows tokens alone, and `cost_complete` is false in `trix ls --json`. Give it a price in `[cost.prices]` (see [Config](#config)).
- **On screen**: each pane's bottom border has the agent's `$1.23 · 4.1M tok` next to its context, and the top bar the workspace's total (`$12.40 of $25.00 · 18M tok` with a budget). An agent closed since the server started still counts in the total. `trix ls` adds the total to its first line.
- **Budgets**: with `budget` or `agent_budget` set in `[cost]`, the figure turns yellow at `warn_at` percent of its budget (80 by default) and red over it. trix says so in a toast as spend passes the warning, and again when it goes over, along with a desktop notification and the bell if `[notify]` has them on. Each is said once; raise the budget, and it's said again the next time spend passes it. trix doesn't stop an agent over budget.
- **This month**: trix keeps a record of what its agents spend, day by day and model by model, in `~/.local/state/trix/spend/` (`$XDG_STATE_HOME` if set), one file a month. Every session adds to it, so it's the whole machine's trix spend, not one workspace's. Each turn counts on the day its log line says, in local time, and once only: an agent restored, a clone (whose log opens with its original's conversation) or a conversation resumed from before trix counts only what it spends under trix. Tokens are kept, not dollars, and priced when shown, so a corrected price reaches past months too. The record starts when you first run a trix that has it: earlier spend isn't in it.
  - `alt+$` opens the month: each model's tokens, cost and share, the total, today, last month, and what the month will cost at its pace so far (from its first day with spend, once there's a day of it). `←` and `→` step through earlier months.
  - `monthly_budget` holds the month to a budget, warned and told like the others. With it set, the top bar shows the month (`$256 of $300 this month`) in place of the workspace's total.
  - `trix spend` prints the month, `trix spend 2026-10` an earlier one, and `--json` gives `{"month", "total_tokens", "cost_usd", "cost_complete", "today_usd", "pace_usd", "budget_usd", "over_budget", "models": [{"model", "total_tokens", "cost_usd", "cost_complete"}]}`. It needs no running trix. `trix ls --json` has the same for this month as `"month"`.
- In `trix ls --json`, each agent gets `"spend": {"input_tokens", "cache_write_tokens", "cache_read_tokens", "output_tokens", "total_tokens", "cost_usd", "cost_complete", "budget_usd", "over_budget"}`, or `null` before it has used any, and the listing gets `"totals"`, the same for the whole workspace against `budget`. Its `"prices"` says where prices come from: `"source"` (`"live"` or `"built-in"`), `"fetching"`, `"url"`, `"fetched_at"` (Unix seconds), `"age_seconds"`, `"models"` and `"last_error"`, the last fetch's failure while the previous list stays in use.

### Viewers

`trix view` (or `trix -s work view`) watches a running session read-only. Any number of viewers can watch at once, and the controlling terminal's top bar shows how many (`◉ 2 watching`), as does `trix ls`.

- Viewers see the controller's screen. A smaller viewer sees it cropped, a bigger one sees it with blank space around it. A "view only" badge covers the clock. The layout, and the agents' terminal size, always follow the controller, so watching never changes anything.
- Keys go nowhere, except `q`, `esc`, `ctrl+c` or `alt+d`, which stop watching. Mouse selection uses the viewer's own terminal.
- Viewers stay when the controller detaches, and keep seeing live output. When the workspace quits or is killed, they're told and let go.
- Viewers don't mark anything as seen, and they don't receive clipboard copies made in the controller.

### Sessions

Without `-s` you get the default session. `-s <name>` (or `--session <name>`) picks another one: letters, digits, `-` and `_`, up to 32 characters. Each session is its own server, with `session-<name>.sock`, `hooks-<name>.sock` and `server-<name>.log` in the runtime folder and `workspace-<name>.json` for restore, so killing or restoring one never touches another. A named session shows its name in the top bar, the terminal title and its desktop notifications.

`trix ls` without `-s` lists every running session. Inside a pane, `TRIX_SESSION` holds the pane's session, so `trix ls` and `trix kill` there act on that session; `trix ls --all` lists every session from there too (with `--json`, as an array of each session's listing).

**Left alone.** Each session's line in `trix ls` says how long it has been since its controlling terminal left and how long nobody and nothing has used it: `trix · sidebar · 4 agents · … · detached 27h · idle 27h`. Something happening means an agent working, typing into it, or a `trix send` or `trix spawn`; an agent's output alone doesn't count, because programs redraw on their own. A session with a terminal attached is never idle (watching with `trix view` doesn't count). Times are wall-clock, so a night with the laptop closed counts. A session idle longer than `[session] idle_after` (a day by default) gets a line saying so, with `trix -s <name>` to look and `trix kill -s <name>` to stop it; stopping loses nothing, since [restore](#restore) brings it back. An agent that has needed you for longer than that is flagged first, with a `!`. `trix ls --json` has the same as `attached`, `detached_seconds`, `idle_seconds` and `started_seconds`.

**Stopped, but saved.** A stopped session's saved workspace keeps its worktrees in use: the review's `X` won't remove them, since a restore would bring agents back into them. `trix ls` ends with the stopped sessions that still have one, each with its number of agents, how old the file is, and "all N folders gone" when none of them is left. `trix forget -s <name>` drops one: it shows the agents it would drop and asks, with no as the default. Without a terminal to ask it does nothing unless given `--yes`. It refuses while the session's server runs (stop it first: `trix kill` saves the workspace) or while agents a server that died left running are still about. Folders and branches stay; only the offer to bring the agents back goes.

### Restore

**After a crash, nothing needs restoring.** The server owns no terminals: each agent's PTY, and the program in it, belong to a holder process of its own (`trix holder`, in its own process session, like dtach or abduco), which also keeps the last 4 MB of that agent's output. If the server dies, from a panic or a `kill -9`, the holders and their programs carry on: a long build keeps building, a shell keeps its running command, Claude keeps its turn. The next server (the next `trix`) attaches to them and rebuilds each pane from the output its holder kept: same agents, same ids, same order, focus and pins, their scrollback back, the counter still counting. Anything that came back this way is left out of the restore offer below.

- **Finding them.** Holders are found by their sockets (`holder.<hash of the session>.<agent id>.<pid>.sock`), and each confirms the session and id when greeted. Agents' hooks keep reporting across the change, since `TRIX_SOCKET` is the session's (`hooks.sock`, `hooks-<name>.sock`), not one server's. The emulator's answers to what the program asked long ago aren't sent again while its output replays.
- **Socket paths.** A Unix socket path has to fit in 104 bytes on macOS and 108 on Linux, which a long runtime folder can overflow. Holders' socket names are short whatever the session's name (12 hex digits stand for it), and any socket whose full path wouldn't fit goes in `/tmp/trix-<uid>` instead, a folder only you can open (trix makes it `0700`, and won't use one that isn't yours or that others can open). trix looks for sockets in both places, so no agent is left without a holder because of where your runtime folder is or what your session is called.
- **Full-screen programs come back full-screen.** A holder keeps only the last 4 MB, so a program that switched to the alternate screen, hid the cursor or turned on mouse reporting long ago has had that setup scroll out of the buffer. So the holder follows the terminal modes as the output passes through it: alternate screen, cursor visibility and style, application cursor keys and keypad, bracketed paste, mouse reporting and its encoding, focus events, the kitty keyboard flags, the scroll region, wraparound, origin and insert mode. It's a byte-level parser that doesn't mind a sequence split across reads, and doesn't take what's inside a title or other string for a command.
  - A new server gets the modes in force where the kept output begins, as a short preamble, then the output itself. When trimming, the buffer starts at a line, never inside a sequence, and on the alternate screen at its next full clear when there's one, so the replay draws from a clean screen.
  - Then the holder tells a full-screen program the terminal changed size (one row less for a moment, then back), so it repaints all of it at once.
  - A preamble doesn't carry the cursor position and colors where the kept output begins, which a full clear or repaint sets anyway. A program that ignores a size change shows what its kept output draws until it next repaints.
- **Stopping.** `trix kill`, `alt+q` and closing an agent stop holders and their programs, as they always stopped the agents; detaching (`alt+d`, closing the terminal) never does.
  - A holder hears that even from a program that stopped reading its input: typing goes through a queue of its own, and what's typed past 1 MB of backlog is dropped. A holder that still hasn't stopped a few seconds later is stopped by the server, program and all, so a closed agent never comes back with the next server.
  - A holder made to stop signals its program only while that is still its child, so a pid that went to another process since is never touched.
  - `trix kill` with no server running stops the agents a crashed one left behind, and `trix kill --all` does that for every session, except those of a session whose server is only just starting (it holds the session's lock already, and is about to reattach them). Until then they aren't forgotten: `trix ls`, and any command that finds no server, says how many agents are still running, and that `trix` picks them up and `trix kill` stops them.
  - A program that ended while no server was attached is kept, with its last screen and exit code, for an hour; then its holder goes.
  - If a holder can't start, or doesn't say it's up within 10 seconds (then it's killed, so it can't start the program a second time), that agent's terminal lives in the server, and only the restore below can bring it back.
- **A server that falls behind.** A server that stops reading for 5 seconds is let go and hung up on, so the program never stalls behind it. The server hears that, attaches again and redraws the pane from the replay, and what you typed meanwhile is sent once it's back; a program that ended meanwhile comes back with its last output and its exit code. A server taken over by another is told so, and doesn't attach again.
- **Where reattached agents are.** One that needed you still does (and so does one whose screen shows a question that came up while no server watched), news nobody looked at is still news, and one that was working still is if it's still writing. One whose turn ended while no server heard its hook is done, *finished while trix was away*, and that is news too (not "interrupted": nobody was there to interrupt it). This goes for agents that report their turns, OpenCode aside, which can work without a word.
- **Upgrades.** Holders greet with a protocol version, since they outlive upgrades: a trix attaches to holders of its version or older, and leaves a newer one running for the trix that started it, saying so.

**After `trix kill`, `alt+q` or a reboot**, the saved workspace brings the agents back.

The layout (kind, name, folder, extra args, and the conversation id each agent reports, or that trix gave Grok or Copilot) is saved to `$XDG_STATE_HOME/trix/workspace.json` (default `~/.local/state/trix/`; `workspace-<name>.json` for a named session) whenever it changes. When a fresh server finds it, you're asked once:

- `y` restores. Claude, Codex, Grok, OpenCode, Cursor, Copilot, Muse, Gemini, Antigravity and Amp resume their conversation, or start fresh if they never reported one. Shells reopen where they were. Custom commands come back **stopped** (they could do anything when re-run); press enter on one to start it. Agents whose folder is gone, such as a deleted worktree, are skipped.
- `n` starts fresh and forgets the saved workspace.
- `esc` decides later. Nothing is lost, and the question comes back on the next attach.

To stop being asked, set **Bring back the last workspace** in the [settings](#settings) to *always* or *never*, or `restore` in `config.toml`'s `[workspace]`.

Scrollback isn't restored this way (only by reattaching), and closed or exited agents aren't saved.

The sidebar size you pick with `alt+m`, and what `alt+c` chose, are kept separately, in `prefs.json` in the same folder. It applies to every session and survives restarts whether or not you restore.

### Keys

| key | action |
| --- | --- |
| `alt+n` | new agent (kind, folder, first prompt, worktree). The kind row has claude · codex · shell · custom command, then *more ▸*, which opens in place as `→` reaches it: grok · opencode · cursor · copilot · muse · gemini · antigravity · amp (the row scrolls to the ones that don't fit). A kind picked from *more* shows in its place. `c` `x` `g` `o` `u` `p` `m` `i` `a` `e` `s` pick claude, codex, grok, opencode, cursor, copilot, muse, gemini, antigravity, amp, shell, under *more* or not. `[new] kinds` sets the row (see [Config](#config)). The Folder field lists folders under it: the ones that complete the path you type (`tab` completes, as a shell does), and ones proposed before you type, which a word narrows: where agents are open, where you started some before, and the git repositories beside this one. `↑↓` pick, `enter` or a click takes one |
| `alt+1…9` / `alt+j` `alt+k` | jump / next / previous |
| `alt+a` | next agent that needs you |
| `alt+g` | grid ⇄ focus |
| `alt+b` | broadcast a prompt |
| `alt+f` | search every agent's scrollback |
| `alt+l` | clone the focused agent: same kind and flags, a new worktree from its commit and changes, a copy of its conversation |
| `alt+r` / `alt+w` | rename (or double-click its name) / close (with a selection, all of it) |
| `alt+o`, right-click | an agent's menu: rename, review diff, clone, restart, copy folder path, select, close. A selected agent's menu is the selection's |
| `alt+x`, alt+click a card | select / unselect an agent: close, grid, broadcast and the menu then act on all selected |
| `alt+d` | detach (agents keep running) |
| `alt+s` | toggle sidebar |
| `alt+m` | sidebar size, in turn: narrow (one line per agent, half the width), icons (only each agent's number and state), wide again (remembered). In a window too narrow for your choice, the sidebar steps down a size or two on its own and hides only where that would squeeze the agent; it comes back as you chose it when the window widens. `alt+m` goes on from the size you see, to the next one that looks different there |
| `alt+c` | compact sidebar: one-line cards instead of full ones that scroll (remembered) |
| `shift+pgup/pgdn`, wheel | scrollback |
| drag | select & copy (OSC 52, plus wl-copy or pbcopy) |
| `alt+,` | settings: theme, sidebar, notifications, sounds |
| `alt+$` | spend: this month (or earlier ones) by model |
| `alt+?` | all keys |
| `alt+q` | quit (stops every agent) |

The keys in the bottom bar are buttons too: click one to do what it says.

Every other key goes straight to the focused agent, including `shift+enter` for a newline in the Claude and Codex composers.

## macOS

trix runs on Linux and macOS. macOS support is newer: if something doesn't work, please [open an issue](https://github.com/leofle/trix/issues) and include `$TMPDIR/trix-$(id -u)/server.log` (`server-<name>.log` for a named session).

### Building on a Mac

`brew install leofle/trix/trix` is the quickest way, and `brew upgrade trix` keeps it current. The [release binary](#install) works on a Mac too. To build it yourself:

```sh
xcode-select --install          # Apple's compiler and linker (skip it if git already works)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env             # or open a new terminal
git clone https://github.com/leofle/trix.git && cd trix
cargo install --path .          # installs ~/.cargo/bin/trix
```

Then set your terminal's Option key to send Alt (below), and run `trix claude` in a project folder.

### Differences

- **Make Option send Alt.** Every trix key is `alt+…`, and macOS terminals type `˜` for Option-N unless told otherwise. In Ghostty, set `macos-option-as-alt = true`. In iTerm2, go to Settings › Profiles › Keys and set Left Option to `Esc+`. In Terminal.app, go to Settings › Profiles › Keyboard and turn on "Use Option as Meta key". In kitty, set `macos_option_as_alt yes`.
- **Selecting agents with the mouse.** trix selects an agent on alt+click. In iTerm2 and Terminal.app, Option-click doesn't reach trix, but Cmd-click arrives as alt+click, so Cmd+click a card there to select it. In other terminals, Option-click works if the terminal passes it on. `alt+x` selects the focused agent in any terminal.
- **Notifications** come from trix, with its icon. macOS shows the icon of the app that sends a notification, so trix keeps a small app of its own: `trix.app` in `~/Library/Application Support/trix`, a copy of the `trix` binary with the icon, made the first time trix notifies you and again after you update trix. The first notification asks whether trix may notify you. If you missed that question or said no, turn on System Settings › Notifications › trix. Clicking a notification only dismisses it. If the app can't be made, notifications go through `osascript` as before and come from Script Editor, and the server log says why.
- **Sounds** play through `afplay`. **Copying** goes through OSC 52, plus `pbcopy` for terminals that ignore it (Terminal.app).
- **Files:** sockets and logs live under `$TMPDIR/trix-<uid>`, where `<uid>` is your user ID (`id -u`), because there's no `XDG_RUNTIME_DIR`. The saved workspace lives in `~/.local/state/trix`, and `trix.app`, which sends the notifications, in `~/Library/Application Support/trix`. Without Omarchy, the theme is Tokyo Night until you pick one (see [Themes](#themes)).

## Supported agents

trix starts each of these agents by name, with an argv (never through a shell), and installs nothing into your agent configs: everything it adds is passed to that one process and lives in `$XDG_RUNTIME_DIR/trix` until the server stops. An agent that isn't installed yet installs first, in its own pane ([below](#not-installed-yet)). Log in to each agent yourself the first time it runs.

| kind | runs | live state from | needs-you from | resume |
| --- | --- | --- | --- | --- |
| `claude` | `claude` | hooks (`--settings` overlay) | `PermissionRequest`, `AskUserQuestion` hooks | yes, `--resume <id>` |
| `codex` | `codex` | hooks (`-c hooks=…`, trusted once) + `notify` | `PermissionRequest` hook | yes, `codex resume <id>` |
| `grok` | `grok` | terminal title, screen fallback; hooks if you add them (below) | title (`⚠ Action Required`) + Grok's own prompt wording on screen | yes: trix starts it with `--session-id <uuid>`, restores with `--resume <uuid>` |
| `opencode` | `opencode` (2.x: `opencode --standalone`) | plugin events (`OPENCODE_CONFIG_CONTENT`) | `permission.asked`, and `question.asked` (1.x) or a question form (2.x) | yes, `--session <id>` |
| `cursor` | `cursor-agent` | output cadence and screen; plugin hooks (`--plugin-dir`) add each tool | screen only: Cursor's approval prompts | yes, `--resume <chatId>` from `sessionStart` |
| `copilot` | `copilot` | plugin hooks (`--plugin-dir`) | `Notification` hook (`permission_prompt`, `elicitation_dialog`), `ask_user` | yes: trix starts and restores it with `--session-id=<uuid>` |
| `muse` | `muse` | managed hooks (`-c managed_hooks_path=…`) | `Notification` hook (`permission_prompt`, `question_prompt`), `request_user_input` | yes, `muse resume <id>`, the id from its hooks |
| `antigravity` | `agy` | output cadence and screen; hooks if you add them (below) | screen only: Antigravity's own trust and approval wording; with hooks, `ask_question` | once hooks report its id: `--conversation <id>` |
| `gemini` | `gemini` | terminal title, screen fallback; hooks if you add them (below) | title (`✋ Action Required`) + Gemini's own prompt wording on screen; with hooks, `Notification` (`ToolPermission`) and `ask_user` | yes: trix starts it with `--session-id <uuid>`, restores with `--resume <uuid>` |
| `amp` | `amp` | output cadence and screen; a plugin if you add it (below) | screen: the wording any agent's prompts share; with the plugin, its thread's `awaiting-approval` state | once the plugin reports its thread: `amp threads continue <id>` |

- **Grok** ([Grok Build](https://docs.x.ai/build)) can't be handed hooks per process: it reads them only from `~/.grok/hooks`, its config and the other files in its [hook locations](https://docs.x.ai/build/features/hooks). So trix reads Grok's terminal title instead: a braille spinner and what it's doing while it works, `⚠ Action Required` while a permission waits, `grok` when idle. Prompts and dialogs the title doesn't cover (folder trust, questions) come from wording only Grok's prompts use (`Ctrl+o:always-approve`, `No, reject (type to add feedback)`, `x:dismiss`). trix names each new conversation with `--session-id`, so restore resumes it, unless your extra args pick one (`-c`, `-r`, `-s`, `--fork-session`). For tool-by-tool detail, you can add trix's hook yourself, as `~/.grok/hooks/trix.json` (`trix hook grok` does nothing outside a trix pane, and Grok runs it only in folders you've trusted):
  ```json
  { "hooks": {
      "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "trix hook grok" }] }],
      "PreToolUse":       [{ "hooks": [{ "type": "command", "command": "trix hook grok" }] }],
      "PostToolUse":      [{ "hooks": [{ "type": "command", "command": "trix hook grok" }] }],
      "Stop":             [{ "hooks": [{ "type": "command", "command": "trix hook grok" }] }],
      "StopCancelled":    [{ "hooks": [{ "type": "command", "command": "trix hook grok" }] }],
      "Notification":     [{ "matcher": "permission_prompt", "hooks": [{ "type": "command", "command": "trix hook grok" }] }]
  } }
  ```
  With it, the hooks report the turn and each tool, and approvals come from both the title and the hook. Grok has no `PermissionRequest` hook; a waiting permission prompt raises `Notification` with type `permission_prompt` ([Grok hook events](https://docs.x.ai/build/features/hooks)), which trix shows as needs-you, with Grok's own message as the activity. Grok's installer also links the name `agent`, which Cursor's installer uses too, so trix never runs `agent`.
- **OpenCode** gets a small plugin, appended to your own `OPENCODE_CONFIG_CONTENT` if you set one (left alone if that isn't a JSON object). It hands OpenCode's events (`session.status`, `permission.asked`, `question.asked`, `tool.execute.before`, …) to `trix hook opencode`, and tells a subagent's sessions from the pane's own so a subagent finishing isn't the turn finishing. It reports only inside a trix pane. OpenCode 2 changed three things, and trix follows each when the installed `opencode --help` lists `--standalone`:
  - **`--standalone`.** OpenCode 2 otherwise joins a background server (`opencode serve --service`) that the first OpenCode started. That server keeps the first one's environment, plugin included, even after that pane or trix itself has gone. With `--standalone`, each pane runs its own server with its own environment. Pass `--standalone` or `--server` in the pane's args and trix adds nothing.
  - **A plugin folder.** OpenCode 2 loads a plugin from a folder only, so trix writes its plugin as `opencode-plugin-<pid>/index.js` too.
  - **A new plugin API.** The folder's plugin is `export default { id, setup }`. It follows `ctx.event.subscribe()` (`session.execution.*`, `session.inbox.enqueued`, `permission.asked`, `form.created`, …) and `ctx.tool.hook`, and reports each the way the OpenCode 1 plugin does.
- **Cursor** gets a plugin directory, `--plugin-dir`, holding only observing hooks: `sessionStart`, `beforeSubmitPrompt`, `postToolUse(Failure)`, `stop`, `sessionEnd`. None of Cursor's permission hooks (`preToolUse`, `beforeShellExecution`, …): their answer decides whether a tool runs, and trix never approves anything. `trix hook cursor` answers `{}` (and `{"continue": true}` before a prompt). cursor-agent 2026.11.11 runs only `sessionStart` and the tool hooks from a plugin, not `beforeSubmitPrompt` or `stop`. So Cursor's screen says when a turn starts and ends: it redraws while it thinks, streams and runs a command, and is silent at its prompt and while an approval waits. The tool hooks add what it ran (`Shell wc -l fruit.txt ✓`), and `sessionStart` names the conversation that restore resumes. Cursor has no hook for its approval prompts either, so trix reads those from the screen: `Run (once)`, `Skip (esc or n)`, `Waiting for approval...`, workspace trust. Only that wording counts, so a question in Cursor's reply doesn't hold the pane on needs-you. If your team's Cursor settings disallow local plugins, the hooks don't run: the state still comes from the screen, but there's no tool detail and restore has no conversation to resume.
- **Copilot** ([GitHub Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli)) gets a plugin directory too, `--plugin-dir`, with a `plugin.json` and a `hooks.json`. Its hooks use Claude's event names (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse(Failure)`, `Notification`, `Stop`, `SessionEnd`), so Copilot sends them as Claude would: snake_case fields and Claude's tool names (`Bash`, `Read`, `Edit`, `AskUserQuestion` for its `ask_user`). See its [hooks reference](https://docs.github.com/en/copilot/reference/hooks-reference).
  - **Needs you.** A waiting approval raises `Notification` with type `permission_prompt`, shown as `approve <the tool it was about to run>`; a dialog (`elicitation_dialog`) and `ask_user` show their question. trix doesn't hook `PermissionRequest`: Copilot runs it before its own rules decide, so for tools that never ask you too. Copilot's approval prompt (`No, and tell Copilot what to do differently`) and its start-up folder trust (`Confirm folder trust`) count on screen as well.
  - **Watching only.** Copilot denies a tool when its `PreToolUse` command fails, so trix's hook is `trix hook copilot || true` and prints nothing: it never decides anything.
  - **Conversations.** trix names each new one with `--session-id=<uuid>`, unless your extra args pick one (`--continue`, `-r`/`--resume`, `--session-id`, `--connect`). Restore uses the same flag, which resumes that conversation, or starts one under that id if it never had a turn. A first prompt goes in as `--interactive=<prompt>`.
  - **Folder trust.** Copilot asks whether you trust the folder as it starts, before anything else. The plugin's hooks ran after answering *Yes* in a folder it hadn't seen (Copilot 1.0.110). Tested with Copilot CLI 1.0.110; older ones may lack the notification hook or `--session-id`.
  - An interrupted turn raises no hook ([none documented](https://docs.github.com/en/copilot/reference/hooks-reference)): like Claude's, it reads as interrupted once the screen goes quiet.
- **Muse** ([Muse Code](https://dev.meta.ai/docs/muse-code), Meta's) gets a hooks file of trix's, named per process with `-c managed_hooks_path=…`. Managed hooks are a hook source of their own, so the hooks in your Muse settings still run beside trix's. Muse runs every hook with a cleared environment, so trix also passes `-c managed_hooks_env_vars=["TRIX_SOCKET","TRIX_AGENT_ID"]`, the two variables `trix hook muse` needs. The hooks use Claude's event and field names (`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse(Failure)`, `Notification`, `Stop(Failure)`, `SessionEnd`; see its [hooks](https://dev.meta.ai/docs/muse-code/extending)), with Muse's own tool names: `bash cargo test`, `read_file main.rs`.
  - **Needs you.** A waiting approval raises `Notification` with type `permission_prompt` ("Muse Code needs approval"), shown as `approve <the tool it was about to run>`. `request_user_input` and a `question_prompt` notification show the question. trix doesn't hook `PermissionRequest`: Muse's approval judge, on by default, settles many calls without asking you. On screen, Muse's workspace trust (`Do you trust this workspace?`), its review of new hooks, its approval choices (`Allow once`, `Always allow in this workspace`, `Reject`, …) and its question dialog count as well.
  - **Subagents.** After each turn, Muse runs reminder subagents ("Double checking"), and they run hooks too. Their hooks carry their own `session_id`, while `MUSE_SESSION_ID` in every hook's environment names the pane's. So trix drops their tools: the finished turn stays done, and restore resumes the pane's conversation. A subagent's approval still needs you.
  - **Start-up.** Muse runs `SessionStart`'s hooks only as its first turn begins, at the same time as that turn's own, so trix takes from it only the conversation id, never the state. Until the first prompt, the screen says: Muse's workspace trust needs you, and once it's answered the pane is idle, not done. Muse's title is its folder, with a braille spinner in front while it works, so its redraws as it starts or resumes don't read as work.
  - **Conversations.** Muse takes no session id for an interactive session, so trix learns it from the hooks and restores with `muse resume <id>`, which takes no first prompt. A first prompt goes in as `muse <prompt>`.
  - **Your managed hooks win.** Muse refuses a run that sets one setting twice, and trix's `-c` would replace a `managed_hooks_path` in your Muse settings for that run. So when your extra args or Muse's `settings.json` set its managed hooks, trix passes none, and that Muse's state comes from its screen.
  - **Watching only.** trix's hook is `trix hook muse || true`. It adds workspace discovery context at session startup; it never decides approvals.
  - An interrupted turn raises no hook: it reads as interrupted once the screen goes quiet. Tested with Muse Code 1.4.18 and its offline `echo` provider (`muse --provider echo`): start-up, workspace trust, first prompts, turn ends, interrupts, reminder subagents, resume, and your own managed hooks. The echo provider calls no tools, so the tool and approval hooks follow Muse's documented events and the wording in its binary.
- **Gemini** ([Gemini CLI](https://github.com/google-gemini/gemini-cli), Google's) can't be handed hooks per process either. It reads hooks from its `settings.json` files, and the only per-process ones, `GEMINI_CLI_SYSTEM_SETTINGS_PATH` and `GEMINI_CLI_SYSTEM_DEFAULTS_PATH`, must sit in a folder owned by root, or Gemini skips them with a security warning (0.68.0). `GEMINI_CLI_HOME` would move your whole `~/.gemini`, logins included. So, as with Grok, trix reads Gemini's terminal title (`ui.dynamicWindowTitle`, on by default): `◇ Ready`, `✦ Working…` (or its thought) and `⏲ Working…`, and `✋ Action Required` while an approval or a question waits. Its folder trust (`Do you trust the files in this folder?`) and approval prompt (`No, suggest changes (esc)`) count on screen as well. trix names each new conversation with `--session-id`, so restore resumes it, unless your extra args pick one (`-r`/`--resume`, `--session-id`, `--session-file`). A first prompt goes in as `--prompt-interactive=<prompt>`. For tool-by-tool detail and [workspace discovery](#agent-workspace-discovery), add trix's hook yourself, in `~/.gemini/settings.json` (`trix hook gemini` does nothing outside a trix pane; Gemini runs these hooks only in folders you've trusted):
  ```json
  { "hooks": {
      "SessionStart": [{ "hooks": [{ "type": "command", "command": "trix hook gemini" }] }],
      "BeforeAgent":  [{ "hooks": [{ "type": "command", "command": "trix hook gemini" }] }],
      "BeforeTool":   [{ "hooks": [{ "type": "command", "command": "trix hook gemini" }] }],
      "AfterTool":    [{ "hooks": [{ "type": "command", "command": "trix hook gemini" }] }],
      "Notification": [{ "hooks": [{ "type": "command", "command": "trix hook gemini" }] }],
      "AfterAgent":   [{ "hooks": [{ "type": "command", "command": "trix hook gemini" }] }],
      "SessionEnd":   [{ "hooks": [{ "type": "command", "command": "trix hook gemini" }] }]
  } }
  ```
  With it, the hooks report the prompt, each tool (`run_shell_command cargo test`, `replace app.rs`), the turn's end and the conversation id. Gemini runs `BeforeTool` before it asks you about a tool, then `Notification` with type `ToolPermission` as it asks: trix shows `approve <the tool>`, and `ask_user` shows its question. A turn you stop with esc raises no hook: the title going back to ready says so. The hook also gives Gemini the workspace discovery instructions as its session starts, inside a trix pane only (Gemini adds them to the conversation as a `<hook_context>` turn). Otherwise it prints nothing, and it never exits 2, so it never blocks or decides anything. Tested with Gemini CLI 0.68.0 in a trix pane: folder trust, its login dialog, the hooks reporting a prompt and its turn's end, the discovery instructions reaching the model's request, and `--session-id`.
- **Antigravity** ([Antigravity CLI](https://antigravity.google/docs/cli/reference), Google's `agy`) is where Gemini CLI's personal Google sign-in moved: since June 2026, Gemini CLI answers a personal account with "This client is no longer supported for Gemini Code Assist for individuals". It can't be handed hooks per process either: it reads them from `~/.gemini/config/hooks.json`, a project's `.agents/hooks.json` and its plugins, and has no flag or variable for more.
  - **Without hooks.** It sets no terminal title of its own, so its state comes from output cadence and the screen: its sign-in (`Select login method:`), its project trust (`Do you trust the contents of this project?`) and its approval prompts (`Requesting permission for:`, `Yes, run command`, and the ways out of the others: `No, deny…`, `No, reject this change`). A first prompt goes in as `--prompt-interactive=<prompt>`. `agy` has no flag to name a new conversation, so restore resumes one (`--conversation <id>`) only once its hooks have reported it.
  - **The hook.** For that, tool-by-tool detail and [workspace discovery](#agent-workspace-discovery), add trix's hook yourself, in `~/.gemini/config/hooks.json` (`trix` must be on the `PATH` Antigravity runs hooks with; in a project's `.agents/hooks.json` it runs only from the session after the one that trusted the folder):
    ```json
    { "trix": {
        "SessionStart":  [{ "type": "command", "command": "trix hook antigravity", "timeout": 5 }],
        "PreInvocation": [{ "type": "command", "command": "trix hook antigravity", "timeout": 5 }],
        "PreToolUse":    [{ "matcher": "*", "hooks": [{ "type": "command", "command": "trix hook antigravity", "timeout": 5 }] }],
        "PostToolUse":   [{ "matcher": "*", "hooks": [{ "type": "command", "command": "trix hook antigravity", "timeout": 5 }] }],
        "Stop":          [{ "type": "command", "command": "trix hook antigravity", "timeout": 5 }]
    } }
    ```
    With it, the hooks report the prompt, each tool (`run_command cargo test`, `replace_file_content app.rs`), its questions (`ask_question`), the turn's end, a turn you stopped, and the conversation id.
  - **Approvals.** No hook reports one: the prompt on screen does, and trix shows `approve <the tool>` from the `PreToolUse` before it. None reports a tool you decline or a turn you stop with esc either: the prompt gone from a still screen says `declined`, and a screen gone quiet `interrupted`.
  - **Watching only.** Antigravity reads a hook's stdout as its answer, and the same file serves its app and IDE, so `trix hook antigravity` always answers, and never with a decision of its own: before a tool, `allow`, which hands the call on to Antigravity's own permissions (it still asks you wherever it would have; an answer with no decision makes Antigravity deny the tool), `{}` otherwise, and inside a trix pane the workspace discovery instructions as the session starts.
  - **Its sandbox.** In auto mode (and with `--sandbox`) Antigravity runs commands in a sandbox that refuses trix's socket, so a `trix ls` or `trix send` it runs fails there first. trix says so (`a sandbox blocks its socket here … run this trix command again outside the sandbox`) rather than `no workspace running`, and Antigravity then runs it outside the sandbox, asking you first where its mode does.
  - **Tested** with Antigravity CLI 1.5.9 in a trix pane: sign-in, project trust, turns with and without the hooks, a command's approval allowed and declined, a turn stopped with esc, the conversation id and `--conversation`. Its file-edit and file-access approvals and its `ask_question` dialog follow the wording in its binary.
- **Amp** ([Amp](https://ampcode.com/docs/cli), Sourcegraph's `amp`) can't be handed hooks per process either: it reads plugins only from a project's `.amp/plugins/` and from `~/.config/amp/plugins/`, and has no flag, variable or setting for more. Without the plugin its state comes from output cadence and the screen, with the prompt wording any agent's share: Amp's own approval wording and its terminal title aren't documented, so trix reads neither.
  - **First prompt.** Amp takes no first prompt as an argument. Its docs: "If you pipe input to the CLI, it uses the input as the first user message in interactive mode". So trix starts it with the prompt on its stdin: a pipe that holds the prompt and then ends, with Amp's screen and keys still the pane's. No shell is involved: the pane runs `trix install amp --stdin <prompt> -- amp …`, which becomes `amp`.
  - **Conversations.** Amp names its own threads (`T-…`) and can't be told a new one's id, so restore continues one (`amp threads continue <id>`, your extra args after it) only once the plugin has reported it. A thread it continues takes no first prompt.
  - **The plugin.** For live state, each tool, approvals, restore and [workspace discovery](#agent-workspace-discovery), add trix's plugin yourself (`trix` must be on the `PATH` Amp runs commands with; outside a trix pane the plugin registers nothing):
    ```sh
    mkdir -p ~/.config/amp/plugins && trix hook amp --plugin > ~/.config/amp/plugins/trix.ts
    ```
    With it, Amp reports each prompt (`agent.start`), each tool once it has run (`tool.result`: `Bash cargo test ✓`, `edit_file app.rs ✓`), the turn's end and a turn you stopped (`agent.end`, with `cancelled`), and the thread id. The thread's own state says when it waits on you: `awaiting-approval` shows as `needs your approval`, until the thread runs again. Only the thread on screen counts, so a subagent's turn ending isn't taken for the pane's. With a thread's first prompt, the plugin hands Amp the workspace discovery instructions as text for the model that Amp doesn't show (`agent.start`'s `message`).
  - **Watching only.** The plugin handles no `tool.call`. A handler of that event must answer whether the tool runs, and Amp's docs don't say what one plugin's `allow` means for another's answer, or for Amp's own asking. So a tool shows once it has run, not as it starts, and an approval names no tool. Each event goes to `trix hook amp` through Amp's `$`, the payload in hex: one plain word, however `$` quotes it.
  - **Not run against Amp yet.** Written from Amp's CLI docs and its plugin API's types (`@ampcode/plugin`, November 2026), with no Amp at hand. trix's side was tested live with a stand-in `amp` in a trix pane: the first prompt on stdin, the plugin's events through `trix hook amp`, working / needs you / done, the thread id saved, and `amp threads continue`. The plugin itself runs in the tests against a fake of Amp's API, under Node. Amp's sign-in, its approval prompts on screen and the plugin inside a real Amp are untested.
- **Context gauge** and **cost** stay Claude and Codex only.

### Not installed yet

When an agent's command isn't on trix's `PATH`, its pane installs the agent first, then starts it with the arguments it would have had (its first prompt, `--resume`, trix's hooks). You watch the install as it runs. Until it's done, the pane shows `installing grok (npm)` and counts as starting: `trix send` waits for it and `trix wait` keeps waiting, and the install output never reads as the agent working. If the install fails, its output stays on screen and Enter tries again. Two panes starting the same agent don't install it twice: the second one waits, then starts it.

trix uses the first of these you have:

| kind | mise (as on Omarchy) | Homebrew | npm | install script |
| --- | --- | --- | --- | --- |
| `claude` | `claude` | cask `claude-code` | `@anthropic-ai/claude-code` | `claude.ai/install.sh` |
| `codex` | `codex` | cask `codex` | `@openai/codex` | |
| `grok` | `npm:@xai-official/grok` | none (Homebrew's `grok` is another program) | `@xai-official/grok` | |
| `opencode` | `opencode` | `opencode` | `opencode-ai` | |
| `cursor` | | cask `cursor-cli` | | `cursor.com/install` |
| `copilot` | `npm:@github/copilot` | cask `copilot-cli` | `@github/copilot` | `gh.io/copilot-install` |
| `muse` | | | none (npm's `@muse-code/sdk` is its SDK) | `dev.meta.ai/install.sh` |
| `gemini` | `npm:@google/gemini-cli` | `gemini-cli` | `@google/gemini-cli` | |
| `antigravity` | | | | `antigravity.google/cli/install.sh` |
| `amp` | `npm:@ampcode/cli` | | `@ampcode/cli` | `ampcode.com/install.sh` |

- **mise** runs `mise use -g <package>`. If mise's shims aren't on trix's `PATH`, the agent runs through `mise x`.
- **The install scripts** are the vendors' own, fetched with curl and run by bash. They put the agent in `~/.local/bin` (Amp's puts it in `~/.amp/bin` and links it from there), which trix adds to that agent's `PATH` if it's missing.
- trix only reads its own `PATH` to decide, so an agent installed some other way, somewhere on your `PATH`, is never installed again. The server keeps the `PATH` of the terminal that started it: after installing a tool like mise or Homebrew, restart trix from a new terminal (`trix kill`, then `trix`).

## Codex

Codex runs a hook only after you've trusted it in its `/hooks` screen. trix passes its hooks the same way on every launch (`-c hooks=…`, one hook per event, all running `trix hook codex`), so trusting them once covers every Codex pane trix ever opens:

1. Open a Codex pane in trix (`alt+n`, codex).
2. Type `/hooks`, and trust the eight trix hooks.

From then on, Codex panes show live detail like Claude's: `exec_command cargo test`, `apply_patch app.rs, new.md`, `approve exec_command rm -rf dist`, and `interrupted` when you press esc.

Until the hooks are trusted, Codex skips them and trix falls back to Codex's `notify`, which only reports finished turns. Screen heuristics cover the rest, and the first finished turn shows a one-time reminder. Codex records trust against the hook's exact definition, which includes the path to trix. If you run trix from another path, such as `target/release/trix` while developing, Codex asks you to trust its hooks again.

Codex panes can drive the workspace like any other agent: `trix ls`, `trix send`, `trix spawn` and the rest.

- **Network access.** Codex's sandbox blocks every socket while its network is off, trix's own included, so trix also passes `-c sandbox_workspace_write.network_access=true`. Without it, those commands say `no workspace running`. That setting also lets the commands Codex runs reach the network, as they would in a plain shell. To keep it off, start Codex with `-c sandbox_workspace_write.network_access=false` in its command (your own args come after trix's, so they win).
- **Read-only mode** (`--sandbox read-only`, or a folder you haven't trusted): Codex can't reach trix either way.
- **No way out of the sandbox.** The commands Codex runs in its sandbox can't use trix to get out of it: trix refuses them anything it would do outside the sandbox, such as starting a shell or answering another agent's prompt (see the guard rails under [Agent control](#agent-control)). A command you approve to run outside the sandbox isn't held back.

Codex shows a startup warning that `-c` overrides run it "without the shared background server". That's been true of every Codex pane in trix, because trix has always passed `-c notify=…`.

## States

| glyph | state | source |
| --- | --- | --- |
| `⠹` | working | `UserPromptSubmit` / `PreToolUse` hooks, or sustained output |
| `▲` | needs you | `PermissionRequest`, `AskUserQuestion`, OpenCode `permission.asked` / `question.asked`, Copilot's `permission_prompt` / `elicitation_dialog` and Muse's `permission_prompt` / `question_prompt` notifications, Amp's `awaiting-approval` thread state, Grok's title, or an approval prompt seen on screen |
| `✓` | done | `Stop` hook / Codex `agent-turn-complete`, or work that went quiet |
| `○` | idle | at the prompt, or interrupted (Codex `Interrupt`, OpenCode abort) |
| `•` | unseen | it finished or asked something while you were elsewhere |

## Security

trix runs agents as you, with your permissions. It doesn't sandbox them; it keeps itself from widening what they can do:

- **It never answers a prompt for you.** trix's hooks and plugins only watch: none decides whether a tool runs. Cursor gets none of its permission hooks, Copilot's and Muse's hooks run as `trix hook … || true`, Gemini's never blocks, and Antigravity's answers `allow`, which hands the call on to Antigravity's own permissions. `trix send` (without `--force`), a broadcast and the review's notes are never typed into an agent that needs you, so none of them can answer its prompt ([guard rails](#agent-control)).
- **Sandboxed agents stay sandboxed.** A command running inside an agent's sandbox (such as Codex's) can't use trix to do anything outside it: no shells, custom commands, `--force`, layouts or `trix kill`. trix asks the kernel which process is on the other end of its socket and checks its sandbox ([guard rails](#agent-control)).
- **A repository's setup runs only once you trust it.** Nothing in a `.trix.toml` runs or is copied until you accept it in the new-agent form, which shows exactly what would run, and any change to it asks again ([Trust](#runnable-worktrees)). That guards against surprises; it isn't a sandbox.
- **Your agent configs stay yours.** Everything trix hands an agent is passed to that one process and lives in trix's runtime folder until the server stops. trix makes that folder `0700` and refuses to start if it isn't private to you; sockets whose path would be too long go in `/tmp/trix-<uid>`, which trix makes `0700` and won't use if it isn't yours or others can open it.
- **No shell in between.** Supported agents are started by name with an argv, never through a shell, and so is a notification `command`.
- **What reaches the network:** trix fetches [LiteLLM's](https://github.com/BerriAI/litellm) public price list about once a day with `curl` (`[cost] live_prices = false` stops it). A missing agent is installed with mise, Homebrew, npm or its vendor's install script the first time you start it, in its pane where you can watch ([Not installed yet](#not-installed-yet)). Codex panes get `-c sandbox_workspace_write.network_access=true` so they can reach trix's socket, which also lets Codex's commands reach the network ([Codex](#codex) says how to turn it off). git and gh reach a remote only when you push, open or merge a pull request from the [review](#review).
- **Reporting a vulnerability:** report it privately at [github.com/leofle/trix/security/advisories/new](https://github.com/leofle/trix/security/advisories/new), not in a public issue.

## Troubleshooting

Start with the server log: `$XDG_RUNTIME_DIR/trix/server.log` on Linux, `$TMPDIR/trix-$(id -u)/server.log` on macOS (`server-<name>.log` for a named session). Without `XDG_RUNTIME_DIR`, Linux uses the temp folder's `trix-<uid>` too.

- **`alt` keys type characters like `˜` or `å` (macOS).** Set your terminal's Option key to send Alt: see [macOS](#macos).
- **A config change doesn't take.** A mistake in `config.toml` keeps the previous settings, and a toast names the line. `trix config` checks `config.toml` and `instructions.md` from the command line.
- **An agent you just installed isn't found, or a new trix isn't running after an upgrade.** The server keeps the `PATH` and binary it started with. Run `trix kill`, then `trix` from a new terminal; the workspace is offered back.
- **Codex panes only show finished turns.** Trust trix's hooks once in Codex's `/hooks` screen ([Codex](#codex)). Codex asks again if trix runs from another path.
- **`trix` commands inside Codex say `no workspace running`.** Codex's sandbox has its network off, or the folder is read-only to it ([Codex](#codex)).
- **Antigravity says `a sandbox blocks its socket here`.** Its auto mode runs commands in a sandbox that refuses trix's socket; Antigravity then runs the command outside it, asking you first where its mode does.
- **`trix: refusing to use …`.** The runtime folder isn't private to you, for example another user made it first in a shared `/tmp`. Fix its owner or permissions (`0700`), or set `XDG_RUNTIME_DIR`.
- **No notifications.** On Linux, trix needs `notify-send` and a notification daemon. On macOS, allow trix in System Settings › Notifications › trix. Check `[notify]` in `config.toml` and that `TRIX_NO_NOTIFY` isn't set.
- **No sound.** On Linux, trix plays through `pw-play` (PipeWire). Check `[sound]` and `mute` in `config.toml`, and `TRIX_NO_SOUND`.
- **Copying doesn't reach the clipboard.** trix copies with OSC 52, plus `wl-copy` on Linux and `pbcopy` on macOS for terminals that ignore it.
- **The pane pin shows as a box.** Your font isn't a Nerd Font: turn **Nerd Font icons** off in the settings.
- **An agent's colors look wrong after a theme switch.** Restart it (`alt+o` › restart) ([Themes](#themes)).
- **Claude's context gauge reads too full on a 1M model.** trix learns the window from the first conversation that goes past 200K; to set it outright, start trix with `TRIX_CLAUDE_CONTEXT=1000000` ([Context gauge](#context-gauge)).
- **`trix ls` says agents are still running, with no workspace.** A server stopped and left their holders running. `trix` picks them up, and `trix kill` stops them ([Restore](#restore)).

Still stuck? [Open an issue](https://github.com/leofle/trix/issues) with the log and `trix --version`.

## Development

```sh
cargo build
cargo test      # the unit tests, in well under a second
cargo clippy --all-targets -- -D warnings
```

CI runs all three on Linux and macOS for every pull request. There's no rustfmt config: don't run `cargo fmt` over the tree, which would reformat most of it.

```
src/main.rs    CLI, attach client, frame-paced server loop
src/session.rs detach/attach and viewers: control socket, framing, remote ratatui backend
src/search.rs  search across agent scrollbacks
src/review.rs  alt+v's diff review: diff parsing, notes and the prompt they make, commit / push / PR / merge / clean up
src/layout.rs  saved layouts: save / open / ls / rm, TOML files
src/usage.rs   context gauge and token counts: follows Claude / Codex conversation logs
src/case.rs    trix triage: a case's roles, reports, the runner in its pane, the merge gate, issues
src/cost.rs    prices per model, what agents' tokens cost, budget levels
src/ledger.rs  each month's spend on disk, per day and model; trix spend
src/prices.rs  the live price list: fetched daily, cached, read into prices per model
src/control.rs agent control: spawn / send / read / wait / close, ls --json, guard rails
src/events.rs  `trix events`: the live event stream, its subscribers and the client
src/worktree.rs runnable worktrees: .trix.toml, its trust, copies, the ports ledger, the setup in the pane
src/config.rs  config.toml: keys, notifications, sounds
src/instructions.rs instructions.md: what you tell every agent, after trix's own instructions
src/settings.rs the settings (alt+,): their rows, and config.toml edits that keep your comments
src/state.rs   saved workspace for restore
src/app.rs     workspace state, keys, mouse, modals, worktrees, notifications
src/agent.rs   emulator per agent (its PTY through a holder), state machine, heuristics
src/holder.rs  `trix holder`: one agent's PTY and program, kept alive across servers; its socket protocol and output buffer
src/modes.rs   terminal modes followed through a program's output, and the preamble that puts them back
src/hooks.rs   unix socket + `trix hook` reporter + Claude settings overlay, OpenCode / Cursor / Copilot plugins, Muse hooks file, Amp plugin + payloads
src/ui.rs      all rendering
src/input.rs   key → xterm byte encoding
src/theme.rs   theme loading (TRIX_THEME, ~/.config/trix/colors.toml, Omarchy) / live reload / the themes the settings offer, built in
src/platform.rs Linux / macOS differences: process liveness and folders, foreground job, notifications, sound, clipboard
src/notifier.rs macOS notifications with trix's icon: makes trix.app, and posts from it through UserNotifications
assets/        trix's icon (trix.svg, and trix.png / trix.icns made from it), the README demo (trix.gif) and its sounds (chime, bell, drop, knock, marimba)
themes/        Omarchy's 23 themes and Cyber Aura, to use without Omarchy (macOS)
install-theme.sh copies one of them, or any colors.toml, to ~/.config/trix/colors.toml
```

The tests cover agent control and its guard rails, the event stream, the config file and the settings that write it, the new-agent form, scrollback search on a real emulator, the diff review on real repositories with worktrees, the agent state machine on a real PTY, hook parsing, key and paste encoding, the session flag and wire framing, holders and reattaching, terminal mode tracking, the context gauge, token counts, prices and budgets, the saved workspace, runnable worktrees, and themes, including `themes/` and `install-theme.sh`.

## Contributing

Issues and pull requests are welcome on [GitHub](https://github.com/leofle/trix). For a bug, include the server log ([Troubleshooting](#troubleshooting)) and `trix --version`.

## License

MIT. See [LICENSE](LICENSE).
