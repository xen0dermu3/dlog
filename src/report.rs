use std::collections::BTreeMap;

use chrono::{Local, TimeZone};

use crate::hours::{format_hours, CommitGap, FirstToLast, Hours, HoursEstimator};
use crate::pr::PrEnrichment;
use crate::scanner::CommitRecord;
use crate::tickets;

pub const UNTAGGED: &str = "(untagged)";

pub type Group<'a> = (String, Vec<&'a CommitRecord>);

pub struct GroupSummary<'a> {
    pub ticket: String,
    pub commits: Vec<&'a CommitRecord>,
    pub gap: Hours,
    pub span: Hours,
}

pub fn group_commits<'a>(
    records: &'a [CommitRecord],
    pr: Option<&PrEnrichment>,
) -> Vec<Group<'a>> {
    let mut groups: BTreeMap<String, Vec<&'a CommitRecord>> = BTreeMap::new();

    for r in records {
        let mut keys: Vec<String> = tickets::extract(&r.branch_at_head);
        keys.extend(tickets::extract(&r.subject));
        keys.extend(tickets::extract(&r.body));
        if let Some(pr) = pr {
            keys.extend(pr.keys_for(&r.oid).iter().cloned());
        }
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            groups.entry(UNTAGGED.to_string()).or_default().push(r);
        } else {
            for k in keys {
                groups.entry(k).or_default().push(r);
            }
        }
    }

    let (mut tagged, untagged): (Vec<_>, Vec<_>) = groups
        .into_iter()
        .partition(|(k, _)| k.as_str() != UNTAGGED);
    tagged.sort_by(|a, b| a.0.cmp(&b.0));
    tagged.extend(untagged);

    for (_, commits) in tagged.iter_mut() {
        commits.sort_by_key(|c| c.author_time);
    }

    tagged
}

pub fn group_with_hours<'a>(
    records: &'a [CommitRecord],
    pr: Option<&PrEnrichment>,
) -> Vec<GroupSummary<'a>> {
    let gap = CommitGap::default();
    let span = FirstToLast;
    group_commits(records, pr)
        .into_iter()
        .map(|(ticket, commits)| {
            let gap_h = gap.estimate(&commits);
            let span_h = span.estimate(&commits);
            GroupSummary {
                ticket,
                commits,
                gap: gap_h,
                span: span_h,
            }
        })
        .collect()
}

/// One-line natural-language description of a group's commit count and
/// wall-clock span. Shared by CLI and TUI renderers.
pub fn subtitle(n: usize, elapsed_hours: f32) -> String {
    if n == 1 {
        "1 commit".to_string()
    } else if elapsed_hours > 0.0 {
        format!("{n} commits across {}", format_hours(elapsed_hours))
    } else {
        format!("{n} commits")
    }
}

pub fn print_grouped(records: &[CommitRecord], pr: Option<&PrEnrichment>) {
    if records.is_empty() {
        println!("(no matching commits)");
        return;
    }

    let repo_width = records.iter().map(|r| r.repo.len()).max().unwrap_or(0);
    let groups = group_with_hours(records, pr);

    let mut total: f32 = 0.0;

    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let n = group.commits.len();
        println!("{} — {}", group.ticket, group.gap.display());
        println!("  {}", subtitle(n, group.span.value));
        total += group.gap.value;
        for c in &group.commits {
            let hm = Local
                .timestamp_opt(c.author_time, 0)
                .single()
                .map(|dt| dt.format("%H:%M").to_string())
                .unwrap_or_else(|| "--:--".to_string());
            let short = &c.oid[..7.min(c.oid.len())];
            println!(
                "  [{:<width$}]  {}  {}  {}",
                c.repo,
                hm,
                short,
                c.subject,
                width = repo_width
            );
        }
    }

    println!();
    println!("Total: {}", format_hours(total));
}
