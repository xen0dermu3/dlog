# DLog

Personal git → Jira daily logger. Scans your local git repos, groups commits by
Jira ticket, and (eventually) pushes worklogs to Jira Cloud and writes a morning
standup summary.

## Prerequisites

- Rust toolchain (stable, 2024 edition)
- A git repo with some commits authored by the email set in `user.email`

## Build

```sh
cargo build --release
```

The binary lands at `target/release/dlog`. For development, `cargo run -- …`
works too.

## Usage

```
dlog scan [PATH] [--date YYYY-MM-DD]
```

- `PATH` — path to a git repo. Defaults to `.` (current directory).
- `--date` — which day to scan, in local timezone. Defaults to today.

It walks commits reachable from all local branches, keeps the ones authored by
you (matched via `git config user.email`) in the given day's local-midnight
window, and prints them grouped by Jira ticket key.

Ticket keys are extracted via the regex `[A-Z][A-Z0-9]+-\d+` applied to:

- the branch name at HEAD
- each commit's subject
- each commit's body

Commits with no matching key land in `(untagged)`. A commit whose message
references multiple keys appears under each.

### Examples

```sh
# today, current repo
cargo run -- scan

# today, a specific repo
cargo run -- scan ~/work/some-repo

# yesterday
cargo run -- scan --date 2026-04-19

# a specific day, specific repo
cargo run -- scan ~/work/some-repo --date 2026-04-15
```
