#!/usr/bin/env bash
# Generate mock git repos for exercising dlog's TUI (fuzzy finder, date picker,
# multi-repo grouping, ticket-key extraction paths).
#
# Usage: ./scripts/gen-mock.sh [OUT_DIR]
#   Default OUT_DIR: ~/dev/dlog-mock
#
# macOS-specific: uses BSD `date -v-Nd` for backdating.

set -euo pipefail

OUT="${1:-$HOME/dev/dlog-mock}"
EMAIL="$(git config --global user.email 2>/dev/null || echo you@example.com)"
NAME="$(git config --global user.name 2>/dev/null || echo tester)"

if [[ -d "$OUT" ]]; then
    read -r -p "$OUT exists. Wipe and regenerate? [y/N] " ans
    case "$ans" in
        y|Y) rm -rf "$OUT" ;;
        *) echo "aborted"; exit 1 ;;
    esac
fi

mkdir -p "$OUT"
echo "Using email: $EMAIL"
echo "Writing to:  $OUT"
echo

# Commit at a given ISO datetime on the current branch.
backdated_commit() {
    local when="$1" msg="$2"
    GIT_AUTHOR_DATE="$when" GIT_COMMITTER_DATE="$when" \
        git commit -q --allow-empty -m "$msg"
}

# Initialise a fresh repo with local user config matching the global.
new_repo() {
    local p="$1"
    mkdir -p "$p"
    cd "$p"
    git init -q -b main
    git config user.email "$EMAIL"
    git config user.name "$NAME"
}

# ---------- repo-a: active feature + hotfix on same day ----------
# Exercises: ticket key in branch name (feature/TP-NNNN-slug), ticket key in
# subject, work spanning multiple days.
new_repo "$OUT/repo-a"
backdated_commit "$(date -v-10d +%Y-%m-%dT09:00:00)" "chore: initial scaffold"

git checkout -q -b feature/TP-1042-login
backdated_commit "$(date -v-3d +%Y-%m-%dT10:15:00)" "TP-1042: scaffold login form"
backdated_commit "$(date -v-3d +%Y-%m-%dT14:22:00)" "add password field"
backdated_commit "$(date -v-2d +%Y-%m-%dT09:30:00)" "TP-1042: wire up submit handler"
backdated_commit "$(date -v-1d +%Y-%m-%dT11:05:00)" "fix flaky test"

git checkout -q main
git checkout -q -b hotfix-metrics  # branch has no ticket key; commit does
backdated_commit "$(date -v-1d +%Y-%m-%dT16:40:00)" "TP-1999: rotate metrics API key"

# ---------- repo-b: a migration across two days ----------
# Exercises: branch-only ticket key (most commits untagged in subject).
new_repo "$OUT/repo-b"
backdated_commit "$(date -v-5d +%Y-%m-%dT09:00:00)" "chore: initial"

git checkout -q -b feature/TP-1105-migration
backdated_commit "$(date -v-2d +%Y-%m-%dT13:00:00)" "run migration dry-run in staging"
backdated_commit "$(date -v-2d +%Y-%m-%dT15:30:00)" "clean up redundant indexes"
backdated_commit "$(date -v-1d +%Y-%m-%dT10:00:00)" "TP-1105: add rollback step"

# ---------- repo-c: today's work + cross-ticket commit ----------
# Exercises: today in date picker, untagged bucket, one commit belonging to
# two tickets.
new_repo "$OUT/repo-c"
backdated_commit "$(date -v-7d +%Y-%m-%dT09:00:00)" "chore: initial"

git checkout -q -b feature/TP-1200-cleanup
backdated_commit "$(date     +%Y-%m-%dT10:00:00)" "TP-1200: delete dead code path"
backdated_commit "$(date     +%Y-%m-%dT11:30:00)" "drop unused import"
backdated_commit "$(date     +%Y-%m-%dT14:00:00)" "TP-1200: also touches TP-1042 edge case"
backdated_commit "$(date     +%Y-%m-%dT15:30:00)" "wip cleanup"

# ---------- today's cross-repo work: TP-1042 spans repo-a + repo-b ----------
# Exercises: a single ticket grouped across multiple repos in one daily scan.
# After running this, `dlog scan repo-a repo-b repo-c` today shows TP-1042
# with commits from both repo-a and repo-b in the same group.
cd "$OUT/repo-a"
git checkout -q feature/TP-1042-login
backdated_commit "$(date     +%Y-%m-%dT09:15:00)" "TP-1042: address review feedback"
backdated_commit "$(date     +%Y-%m-%dT13:45:00)" "TP-1042: tighten input validation"

cd "$OUT/repo-b"
git checkout -q main
git checkout -q -b feature/TP-1042-api-shim
backdated_commit "$(date     +%Y-%m-%dT11:20:00)" "TP-1042: expose login endpoint from gateway"

echo "Generated:"
ls -1 "$OUT" | sed 's/^/  /'
echo
echo "Try:"
echo "  cargo run -- scan $OUT/repo-a $OUT/repo-b $OUT/repo-c"
echo "    (today: TP-1042 spans repo-a + repo-b; TP-1200 in repo-c)"
echo "  cargo run -- scan $OUT/repo-a $OUT/repo-b $OUT/repo-c --date $(date -v-1d +%Y-%m-%d)"
echo "    (yesterday: TP-1042 in repo-a, TP-1105 in repo-b, TP-1999 in repo-a)"
echo "  cargo run   # TUI — press 'r', 'a', type 'dlog-mock'"
