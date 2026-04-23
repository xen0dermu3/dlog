use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::pr::{PrInfo, PrState};

/// Cheap check: does this repo's `origin` remote look like GitHub?
/// Returns false for any error (missing repo, no origin, non-github host).
pub fn is_github_repo(repo_path: &Path) -> bool {
    let Ok(repo) = git2::Repository::open(repo_path) else {
        return false;
    };
    let Ok(remote) = repo.find_remote("origin") else {
        return false;
    };
    remote.url().is_some_and(|url| url.contains("github.com"))
}

/// Fetch up to 50 recent PRs authored by `@me` for this repo via the `gh`
/// CLI. On any failure (gh missing, not authed, not a GitHub repo), returns
/// an empty vec — never an error — so callers degrade silently.
pub fn fetch_prs(repo_path: &Path) -> Vec<PrInfo> {
    try_fetch(repo_path).unwrap_or_default()
}

fn try_fetch(repo_path: &Path) -> Result<Vec<PrInfo>> {
    let raws: Vec<RawPr> = run_gh_json(
        repo_path,
        &[
            "pr",
            "list",
            "--author",
            "@me",
            "--state",
            "all",
            "--limit",
            "50",
            "--json",
            "number,title,body,headRefName,url,mergedAt,closedAt,commits",
        ],
    )?;

    Ok(raws
        .into_iter()
        .map(|r| {
            let state = if r.merged_at.is_some() {
                PrState::Merged
            } else if r.closed_at.is_some() {
                PrState::Closed
            } else {
                PrState::Open
            };
            PrInfo {
                number: r.number,
                title: r.title,
                body: r.body,
                head_branch: r.head_ref_name,
                url: r.url,
                state,
                commit_oids: r.commits.into_iter().map(|c| c.oid).collect(),
            }
        })
        .collect())
}

#[derive(Deserialize)]
struct RawPr {
    number: u64,
    title: String,
    body: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    url: String,
    #[serde(rename = "mergedAt")]
    merged_at: Option<String>,
    #[serde(rename = "closedAt")]
    closed_at: Option<String>,
    commits: Vec<RawPrCommit>,
}

#[derive(Deserialize)]
struct RawPrCommit {
    oid: String,
}

fn run_gh_json<T: for<'de> Deserialize<'de>>(cwd: &Path, args: &[&str]) -> Result<T> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to spawn `gh` — is the GitHub CLI installed and on PATH?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh {} failed: {}", args.join(" "), stderr.trim());
    }
    serde_json::from_slice(&output.stdout).context("parsing gh JSON output")
}
