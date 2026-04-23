use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::config::BitbucketConfig;
use crate::pr::{PrInfo, PrState};

const KEYRING_SERVICE: &str = "dlog-bitbucket";
const API_ROOT: &str = "https://api.bitbucket.org/2.0";

/// Check whether the repo's `origin` remote looks like Bitbucket Cloud.
pub fn is_bitbucket_repo(repo_path: &Path) -> bool {
    let Ok(repo) = git2::Repository::open(repo_path) else {
        return false;
    };
    let Ok(remote) = repo.find_remote("origin") else {
        return false;
    };
    remote
        .url()
        .is_some_and(|url| url.contains("bitbucket.org"))
}

/// Fetch PRs authored by the configured user for this repo. Silently
/// returns `vec![]` on any failure so the caller can degrade gracefully.
pub fn fetch_prs(repo_path: &Path, cfg: &BitbucketConfig) -> Vec<PrInfo> {
    try_fetch(repo_path, cfg).unwrap_or_default()
}

/// Save the Atlassian app password for Bitbucket to the OS keyring.
pub fn save_token(email: &str, app_password: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, email).context("opening keyring entry")?;
    entry
        .set_password(app_password)
        .context("saving Bitbucket app password to keyring")?;
    Ok(())
}

// ---------- internals ---------------------------------------------------

fn try_fetch(repo_path: &Path, cfg: &BitbucketConfig) -> Result<Vec<PrInfo>> {
    let (workspace, slug) = parse_remote(repo_path)?;
    let entry = keyring::Entry::new(KEYRING_SERVICE, &cfg.email)
        .context("opening Bitbucket keyring entry")?;
    let password = entry
        .get_password()
        .context("reading Bitbucket app password from keyring")?;
    let auth = {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine;
        B64.encode(format!("{}:{}", cfg.email, password))
    };

    // Identify ourselves so we can filter PRs we authored.
    let me: UserResp = get_json(&format!("{API_ROOT}/user"), &auth)?;
    let my_account_id = me.account_id;

    // List PRs — all states, filter by author client-side.
    let list_url = format!(
        "{API_ROOT}/repositories/{workspace}/{slug}/pullrequests?state=OPEN&state=MERGED&state=DECLINED&pagelen=50"
    );
    let prs: PrListResp = get_json(&list_url, &auth)?;

    let mut result = Vec::new();
    for raw in prs.values {
        if raw.author.account_id != my_account_id {
            continue;
        }
        let state = match raw.state.as_str() {
            "OPEN" => PrState::Open,
            "MERGED" => PrState::Merged,
            _ => PrState::Closed,
        };
        let head_branch = raw
            .source
            .as_ref()
            .and_then(|s| s.branch.as_ref())
            .map(|b| b.name.clone())
            .unwrap_or_default();
        let url = raw
            .links
            .as_ref()
            .and_then(|l| l.html.as_ref())
            .map(|h| h.href.clone())
            .unwrap_or_default();

        // Fetch commits for this PR.
        let commits_url = format!(
            "{API_ROOT}/repositories/{workspace}/{slug}/pullrequests/{}/commits?pagelen=50",
            raw.id
        );
        let commits: CommitsResp = match get_json(&commits_url, &auth) {
            Ok(c) => c,
            Err(_) => CommitsResp { values: vec![] },
        };
        let commit_oids: Vec<String> = commits.values.into_iter().map(|c| c.hash).collect();

        result.push(PrInfo {
            number: raw.id,
            title: raw.title,
            body: raw.description.unwrap_or_default(),
            head_branch,
            url,
            state,
            commit_oids,
        });
    }
    Ok(result)
}

/// Extract `(workspace, repo_slug)` from the repo's `origin` remote URL.
fn parse_remote(repo_path: &Path) -> Result<(String, String)> {
    let repo = git2::Repository::open(repo_path).context("opening git repo")?;
    let remote = repo.find_remote("origin").context("finding origin remote")?;
    let url = remote.url().context("origin remote has no URL")?;

    // Strip common prefixes and `.git` suffix. Handles:
    //   git@bitbucket.org:workspace/repo.git
    //   https://bitbucket.org/workspace/repo.git
    //   ssh://git@bitbucket.org/workspace/repo
    let tail = url
        .split("bitbucket.org")
        .nth(1)
        .context("not a bitbucket.org remote")?
        .trim_start_matches([':', '/']);
    let tail = tail.trim_end_matches(".git");
    let mut parts = tail.splitn(2, '/');
    let workspace = parts.next().context("missing workspace")?.to_string();
    let slug = parts.next().context("missing repo slug")?.to_string();
    if workspace.is_empty() || slug.is_empty() {
        bail!("parsed empty workspace/slug from {url}");
    }
    Ok((workspace, slug))
}

fn get_json<T: for<'de> Deserialize<'de>>(url: &str, basic_auth: &str) -> Result<T> {
    let mut response = ureq::get(url)
        .config()
        .http_status_as_error(false)
        .build()
        .header("Authorization", &format!("Basic {basic_auth}"))
        .header("Accept", "application/json")
        .call()
        .context("Bitbucket GET")?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let text = response
            .body_mut()
            .read_to_string()
            .unwrap_or_else(|_| "<no body>".to_string());
        bail!("Bitbucket HTTP {status}: {}", text.trim());
    }
    response
        .body_mut()
        .read_json()
        .context("parsing Bitbucket JSON")
}

// ---------- API response types -----------------------------------------

#[derive(Deserialize)]
struct UserResp {
    account_id: String,
}

#[derive(Deserialize)]
struct PrListResp {
    values: Vec<PrRaw>,
}

#[derive(Deserialize)]
struct PrRaw {
    id: u64,
    title: String,
    #[serde(default)]
    description: Option<String>,
    state: String,
    #[serde(default)]
    source: Option<PrSource>,
    author: PrAuthor,
    #[serde(default)]
    links: Option<PrLinks>,
}

#[derive(Deserialize)]
struct PrSource {
    #[serde(default)]
    branch: Option<PrBranch>,
}

#[derive(Deserialize)]
struct PrBranch {
    name: String,
}

#[derive(Deserialize)]
struct PrAuthor {
    account_id: String,
}

#[derive(Deserialize)]
struct PrLinks {
    #[serde(default)]
    html: Option<PrLink>,
}

#[derive(Deserialize)]
struct PrLink {
    href: String,
}

#[derive(Deserialize)]
struct CommitsResp {
    values: Vec<CommitRaw>,
}

#[derive(Deserialize)]
struct CommitRaw {
    hash: String,
}
