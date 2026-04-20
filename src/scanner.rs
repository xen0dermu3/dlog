use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use git2::{BranchType, Repository, Sort};

pub struct CommitRecord {
    pub oid: String,
    pub author_time: i64,
    pub subject: String,
    pub body: String,
    pub branch_at_head: String,
}

pub fn scan(repo_path: &Path, date: Option<NaiveDate>) -> Result<Vec<CommitRecord>> {
    let repo = Repository::open(repo_path)
        .with_context(|| format!("opening git repo at {}", repo_path.display()))?;

    let me = repo
        .config()?
        .get_string("user.email")
        .context("reading user.email from git config")?;

    let target = date.unwrap_or_else(|| Local::now().date_naive());
    let start = target
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(Local)
        .single()
        .context("resolving start-of-day")?
        .timestamp();
    let end = target
        .succ_opt()
        .context("computing next day")?
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
            });
        }
    }

    records.sort_by_key(|r| r.author_time);
    Ok(records)
}
