use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use crate::tickets;

const PR_WINDOW_DAYS: i64 = 14;
const PR_LIMIT: &str = "50";

#[derive(Deserialize)]
struct Pr {
    number: u64,
    title: String,
    body: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
}

#[derive(Deserialize)]
struct PrCommit {
    oid: String,
}

#[derive(Deserialize)]
struct PrCommitList {
    commits: Vec<PrCommit>,
}

pub struct PrEnrichment {
    map: HashMap<String, Vec<String>>,
}

impl PrEnrichment {
    pub fn keys_for(&self, oid: &str) -> &[String] {
        self.map.get(oid).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn merge(&mut self, other: PrEnrichment) {
        for (k, v) in other.map {
            self.map.entry(k).or_default().extend(v);
        }
        for v in self.map.values_mut() {
            v.sort();
            v.dedup();
        }
    }
}

pub fn fetch(repo_path: &Path) -> Result<PrEnrichment> {
    let prs: Vec<Pr> = run_gh_json(
        repo_path,
        &[
            "pr", "list",
            "--author", "@me",
            "--state", "all",
            "--limit", PR_LIMIT,
            "--json", "number,title,body,headRefName,updatedAt",
        ],
    )
    .context("listing PRs via `gh`")?;

    let cutoff = Utc::now() - Duration::days(PR_WINDOW_DAYS);

    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for pr in prs {
        let updated: DateTime<Utc> = pr
            .updated_at
            .parse()
            .with_context(|| format!("parsing updatedAt for PR #{}", pr.number))?;
        if updated < cutoff {
            continue;
        }

        let mut keys = Vec::new();
        keys.extend(tickets::extract(&pr.title));
        keys.extend(tickets::extract(&pr.body));
        keys.extend(tickets::extract(&pr.head_ref_name));
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            continue;
        }

        let num_str = pr.number.to_string();
        let list: PrCommitList = run_gh_json(
            repo_path,
            &["pr", "view", num_str.as_str(), "--json", "commits"],
        )
        .with_context(|| format!("fetching commits for PR #{}", pr.number))?;

        for c in list.commits {
            map.entry(c.oid).or_default().extend(keys.iter().cloned());
        }
    }

    for v in map.values_mut() {
        v.sort();
        v.dedup();
    }

    Ok(PrEnrichment { map })
}

fn run_gh_json<T: for<'de> Deserialize<'de>>(cwd: &Path, args: &[&str]) -> Result<T> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to spawn `gh` -- is the GitHub CLI installed and on PATH?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "gh {} exited with {}: {}",
            args.join(" "),
            output.status,
            stderr.trim()
        );
    }

    serde_json::from_slice(&output.stdout).context("parsing gh JSON output")
}
