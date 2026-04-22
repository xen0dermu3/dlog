use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use git2::{BranchType, Repository, Sort};

#[derive(Clone)]
pub struct CommitRecord {
    pub oid: String,
    pub author_time: i64,
    pub subject: String,
    pub body: String,
    pub branch_at_head: String,
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

    let branch_at_head = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(str::to_owned))
        .unwrap_or_default();

    let mut seen: HashSet<git2::Oid> = HashSet::new();
    let mut records = Vec::new();

    for branch in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let Some(tip) = branch.get().target() else { continue };

        let mut walk = repo.revwalk()?;
        walk.set_sorting(Sort::TIME)?;
        walk.push(tip)?;

        for oid in walk {
            let oid = oid?;
            if !seen.insert(oid) {
                continue;
            }
            let commit = repo.find_commit(oid)?;
            // Skip merge commits: they distort hour estimates and clutter the
            // display; a merge's real work already appears in the branch it
            // came from.
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
                branch_at_head: branch_at_head.clone(),
                repo: repo_name.clone(),
            });
        }
    }

    records.sort_by_key(|r| r.author_time);
    Ok(records)
}
