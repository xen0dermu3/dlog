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

## Prerequisites (runtime)

- A git repo with some commits authored by the email set in `user.email`.
- (Optional, for `--with-prs`) [GitHub CLI](https://cli.github.com) installed
  and authenticated (`gh auth login`).

## Usage

### Interactive (TUI)

```sh
dlog
```

Launches a terminal UI with four screens:

- **Home** — summary of configured repos and the selected date.
  `[r]` repos  `[d]` date  `[s]` scan  `[q]` quit
- **Repos** — edit the list of repos to scan. Config persists to
  `~/Library/Application Support/dlog/config.toml`.
  `[a]` add  `[x]` delete  `[↑/↓]` move  `[Esc]` back
- **Date picker** — calendar. Cursor is cyan, today is underlined, previously
  selected date is green.
  `[←/→/↑/↓]` move  `[ [ ]` prev month  `[ ] ]` next month  `[t]` today
  `[Enter]` select  `[Esc]` cancel
- **Results** — groups-by-ticket table for the selected date across every
  configured repo. `[Esc]` back.

### Non-interactive (CLI — unchanged from step 2)

```
dlog scan [PATHS...] [--date YYYY-MM-DD] [--with-prs]
```

- `PATHS` — one or more paths to git repos. Defaults to `.` (current directory).
  When multiple repos are given, commits are merged into a single table and each
  line is prefixed with the repo name.
- `--date` — which day to scan, in local timezone. Defaults to today.
- `--with-prs` — enrich ticket extraction by also reading the repo's GitHub PR
  titles, bodies, and head-branch names (via `gh`). Useful when the ticket key
  only appears in the PR, not in the commit or branch.

It walks commits reachable from all local branches per repo, keeps the ones
authored by you (matched via `git config user.email`) in the given day's
local-midnight window, and prints them grouped by Jira ticket key.

Ticket keys are extracted via the regex `[A-Z][A-Z0-9]+-\d+` applied to:

- the branch name at HEAD
- each commit's subject
- each commit's body
- (with `--with-prs`) the title, body, and head-branch of every PR authored by
  you that was updated in the last 14 days

Commits with no matching key land in `(untagged)`. A commit whose message
references multiple keys appears under each.

### Examples

```sh
# today, current repo
cargo run -- scan

# today, a specific repo
cargo run -- scan ~/work/some-repo

# today, across multiple repos (grouped together)
cargo run -- scan ~/work/backend ~/work/frontend ~/work/infra

# yesterday
cargo run -- scan --date 2026-04-19

# a specific day, specific repo
cargo run -- scan ~/work/some-repo --date 2026-04-15

# include PR metadata (requires `gh auth login`)
cargo run -- scan ~/work/backend ~/work/frontend --with-prs
```
