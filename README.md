# DLog

Personal git → Jira daily logger. Scans your local git repos, groups commits by
Jira ticket, and (eventually) pushes worklogs to Jira Cloud and writes a morning
standup summary.

## Install

### From a tagged release (no Rust required)

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/xen0dermu3/dlog/releases/latest/download/dlog-installer.sh | sh
```

Prebuilt binaries are published by
[cargo-dist](https://opensource.axo.dev/cargo-dist/) on every git tag `v*`.
Targets: macOS (Apple Silicon + Intel) and Linux x86_64.

### From source (requires Rust)

```sh
# latest main
cargo install --git https://github.com/xen0dermu3/dlog

# or a local checkout
cargo install --path .
```

Either form installs `dlog` to `~/.cargo/bin/`.

## Prerequisites

- A git repo with some commits authored by the email set in `user.email`.

## Usage

```sh
dlog
```

Launches the terminal UI — three columns:

- **Repos** (left) — the list of git repos to scan. Config persists to
  `~/.dlog/config.toml`; scan cache and pushed-worklog records persist to
  `~/.dlog/dlog.sqlite`.
  `[a]` add (with fuzzy finder) · `[x]` remove · `[↑/↓]` select
- **Date** (middle) — always-visible calendar.
  `[←/→/↑/↓]` move cursor · `[[ / ]]` change month · `[t]` today · `[y]` yesterday
  `[space]` or `[r]` toggle range anchor (for multi-day scans)
- **Results** (right) — commits grouped by Jira ticket key, with estimated
  hours per ticket and a running total. Hours are a **session-based**
  estimate: commits within `session_break_min` minutes (default 120) count
  as one continuous work session, so long stretches of focused coding
  between commits aren't under-counted.
  `[↑/↓]` select ticket · `[e]` edit time (type `30m`, `2h`, `2h 30m`) ·
  `[f]` fill — distribute a day's budget across tickets by session weight ·
  `[PgUp/PgDn]` scroll page

**Explicit time markers in commit messages.** Put a bracketed duration
anywhere in the commit subject or body — `[30m]`, `[2h]`, `[1h 15m]` —
and dlog treats it as authoritative time for that commit. Totals per
ticket sum across all its marked commits, and `[f] fill` **pins** that
value while splitting the remaining budget across the free-floating
tickets. Non-duration brackets like `[WIP]` or `[TODO]` are ignored.

Estimation thresholds and the fill-budget default are configurable in
`~/.dlog/config.toml`:

```toml
[estimation]
session_break_min = 120     # min idle gap that breaks a session
lead_min = 15               # buffer before each session's first commit
trail_min = 15              # buffer after each session's last commit
budget_default_hours = 8.0  # pre-fill for the [f] button
```

Global keys: `[Tab]` next pane · `[s]` scan · `[S]` rescan (bypass cache) ·
`[m]` morning standup · `[p]` push worklogs to Jira · `[J]` Jira settings ·
`[q]` / `[Esc]` quit.

The standup view (`m`) shows three sections:
1. **Yesterday's work** — tickets you committed to, with session-hour
   estimates (and any `[Xh]` markers from commit messages).
2. **Today — in-flight** — unpushed commits by you from the last 7 days.
3. **Today — plan** — Jira issues assigned to you in your configured
   "in-progress-ish" statuses, merged with any open PRs you authored. A
   PR that mentions a Jira issue already listed is attached to that item;
   PRs with no Jira match are listed standalone. Configure the statuses
   in Jira settings (press `J`, step 4/4 — default is `In Progress`).

Use this view first thing in the morning for your standup call.

Ticket keys are extracted via the regex `[A-Z][A-Z0-9]+-\d+` applied to the
branch name at HEAD, each commit's subject, and each commit's body. Commits
with no matching key land in `(untagged)`. A commit whose message references
multiple keys appears under each.

Merge commits are skipped (they double-count work already attributed to the
feature branch).
