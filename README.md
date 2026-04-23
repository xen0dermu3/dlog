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
  hours per ticket and a running total.
  `[↑/↓]` select ticket · `[e]` edit time (type `30m`, `2h`, `2h 30m`)
  `[PgUp/PgDn]` scroll page

Global keys: `[Tab]` next pane · `[s]` scan · `[S]` rescan (bypass cache) ·
`[m]` morning standup · `[p]` push worklogs to Jira · `[J]` Jira settings ·
`[q]` / `[Esc]` quit.

The standup view (`d`) shows **yesterday's work** (grouped by ticket, with
hours) plus **today's in-flight** — unpushed commits by you from the last
7 days across all configured repos, grouped the same way. Use it first thing
in the morning for your standup call.

Ticket keys are extracted via the regex `[A-Z][A-Z0-9]+-\d+` applied to the
branch name at HEAD, each commit's subject, and each commit's body. Commits
with no matching key land in `(untagged)`. A commit whose message references
multiple keys appears under each.

Merge commits are skipped (they double-count work already attributed to the
feature branch).
