use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use git2::{BranchType, Oid, Repository, Sort};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct CommitRecord {
    pub oid: String,
    pub author_time: i64,
    pub subject: String,
    pub body: String,
    /// Space-joined list of local branch names that contain this commit.
    /// Used as one of the inputs to ticket-key extraction.
    pub branches: String,
    pub repo: String,
}

pub fn scan(
    repo_path: &Path,
    start_date: NaiveDate,
    end_date_inclusive: NaiveDate,
) -> Result<Vec<CommitRecord>> {
    let repo = Repository::open(repo_path)
        .with_context(|| format!("opening git repo at {}", repo_path.display()))?;

    let repo_name = repo_path
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| repo_path.display().to_string());

    let me = repo
        .config()?
        .get_string("user.email")
        .context("reading user.email from git config")?;

    let start = start_date
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .single()
        .context("resolving start-of-day")?
        .timestamp();
    let end = end_date_inclusive
        .succ_opt()
        .context("computing day after end")?
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .single()
        .context("resolving end-of-day")?
        .timestamp();

    // Phase 1: for every local branch, walk its history and record which
    // branches contain each OID. We only need to go back to the scan's start
    // (with a day of slack for out-of-order merge timestamps).
    let cutoff = start - 24 * 3600;
    let mut branches_by_oid: HashMap<Oid, Vec<String>> = HashMap::new();
    for branch in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let name = match branch.name()? {
            Some(n) => n.to_string(),
            None => continue,
        };
        let Some(tip) = branch.get().target() else {
            continue;
        };
        let mut walk = repo.revwalk()?;
        walk.set_sorting(Sort::TIME)?;
        walk.push(tip)?;
        for oid in walk {
            let oid = oid?;
            let commit = repo.find_commit(oid)?;
            let time = commit.time().seconds();
            if time < cutoff {
                // Past the window with margin — older commits on this branch
                // can't contribute to in-window records.
                break;
            }
            branches_by_oid.entry(oid).or_default().push(name.clone());
        }
    }

    // Phase 2: filter OIDs to author-me, in-window, non-merge; emit records
    // with the full branch list joined.
    let mut records = Vec::new();
    for (oid, branches) in &branches_by_oid {
        let commit = match repo.find_commit(*oid) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if commit.parent_count() > 1 {
            continue;
        }
        let time = commit.time().seconds();
        if time < start || time >= end {
            continue;
        }
        if commit.author().email() != Some(me.as_str()) {
            continue;
        }
        let msg = commit.message().unwrap_or("");
        let (subject, body) = match msg.split_once('\n') {
            Some((s, b)) => (s.trim_end().to_owned(), b.trim().to_owned()),
            None => (msg.trim().to_owned(), String::new()),
        };
        records.push(CommitRecord {
            oid: oid.to_string(),
            author_time: time,
            subject,
            body,
            branches: branches.join(" "),
            repo: repo_name.clone(),
        });
    }

    records.sort_by_key(|r| r.author_time);
    Ok(records)
}
