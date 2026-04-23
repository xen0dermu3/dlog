use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{Duration, NaiveDate};

use crate::config::Config;
use crate::github::{PrInfo, PrState};
use crate::jira::{IssueInfo, JiraClient};
use crate::scanner::{self, CommitRecord};

pub const IN_FLIGHT_CUTOFF_DAYS: i64 = 7;

/// One line in the "Today — plan" section. Either a Jira issue (with
/// optional matching PR), or a standalone open PR with no Jira match.
pub struct TodayItem {
    pub ticket: Option<String>, // Some(key) if known
    pub title: String,          // Jira summary or PR title
    pub status: Option<String>, // Jira status name, if sourced from Jira
    pub pr_number: Option<u64>, // open PR number if this item has one
}

pub struct StandupReport {
    pub yesterday: NaiveDate,
    pub yesterday_records: Vec<CommitRecord>,
    pub in_flight_records: Vec<CommitRecord>,
    pub today_plan: Vec<TodayItem>,
}

/// Compose a standup report: yesterday's work + in-flight unpushed +
/// today's plan (Jira issues in configured statuses ∪ my open PRs).
/// All external calls degrade silently per-source.
pub fn build(
    config: &Config,
    today: NaiveDate,
    pr_index: &HashMap<String, Vec<PrInfo>>,
    jira: Option<&JiraClient>,
) -> Result<StandupReport> {
    let yesterday = today.pred_opt().unwrap_or(today);
    let cutoff = today
        .checked_sub_signed(Duration::days(IN_FLIGHT_CUTOFF_DAYS))
        .unwrap_or(today);

    let mut yesterday_records = Vec::new();
    let mut in_flight_records = Vec::new();

    for repo in &config.repos {
        if let Ok(mut r) = scanner::scan(repo, yesterday, yesterday) {
            yesterday_records.append(&mut r);
        }
        if let Ok(mut r) = scanner::scan_in_flight(repo, cutoff, today) {
            in_flight_records.append(&mut r);
        }
    }

    yesterday_records.sort_by_key(|r| r.author_time);
    in_flight_records.sort_by_key(|r| r.author_time);

    let today_plan = build_today_plan(config, pr_index, jira);

    Ok(StandupReport {
        yesterday,
        yesterday_records,
        in_flight_records,
        today_plan,
    })
}

/// Merge Jira issues in "today's" statuses with the user's open PRs.
/// When a PR's title/body mentions a Jira key that already appears in
/// the Jira list, attach the PR number to that item instead of listing
/// separately.
fn build_today_plan(
    config: &Config,
    pr_index: &HashMap<String, Vec<PrInfo>>,
    jira: Option<&JiraClient>,
) -> Vec<TodayItem> {
    let mut items: Vec<TodayItem> = Vec::new();
    let mut seen_keys: HashSet<String> = HashSet::new();

    // 1. Jira issues in the configured statuses.
    if let (Some(cfg), Some(client)) = (&config.jira, jira) {
        if let Ok(issues) = client.search_issues(&cfg.status_filter) {
            for IssueInfo { key, summary, status } in issues {
                seen_keys.insert(key.clone());
                items.push(TodayItem {
                    ticket: Some(key),
                    title: summary,
                    status: Some(status),
                    pr_number: None,
                });
            }
        }
    }

    // 2. Open PRs authored by me. Dedup against Jira items by key match.
    let open_prs: Vec<&PrInfo> = pr_index
        .values()
        .flatten()
        .filter(|pr| pr.state == PrState::Open)
        .collect();

    // Unique PRs by number (same PR can appear under multiple OIDs).
    let mut seen_pr_numbers: HashSet<u64> = HashSet::new();
    for pr in open_prs {
        if !seen_pr_numbers.insert(pr.number) {
            continue;
        }
        // Pull ticket keys from the PR's own text.
        let mut keys: Vec<String> = crate::tickets::extract(&pr.title);
        keys.extend(crate::tickets::extract(&pr.body));
        keys.extend(crate::tickets::extract(&pr.head_branch));

        // If any of its keys already appears as a Jira item, attach the PR
        // number to the first matching item rather than adding a new row.
        let matched = keys
            .iter()
            .find(|k| seen_keys.contains(k.as_str()))
            .cloned();
        if let Some(k) = matched {
            if let Some(slot) = items.iter_mut().find(|it| it.ticket.as_deref() == Some(&k)) {
                slot.pr_number = Some(pr.number);
                continue;
            }
        }
        // Otherwise list the PR standalone (use the first ticket key if any).
        items.push(TodayItem {
            ticket: keys.into_iter().next(),
            title: pr.title.clone(),
            status: None,
            pr_number: Some(pr.number),
        });
    }

    items
}
