use std::collections::BTreeMap;

use crate::hours::{format_hours, CommitGap, FirstToLast, Hours, HoursEstimator};
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

pub fn group_commits<'a>(records: &'a [CommitRecord]) -> Vec<Group<'a>> {
    let mut groups: BTreeMap<String, Vec<&'a CommitRecord>> = BTreeMap::new();

    for r in records {
        let mut keys: Vec<String> = tickets::extract(&r.branches);
        keys.extend(tickets::extract(&r.subject));
        keys.extend(tickets::extract(&r.body));
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

pub fn group_with_hours<'a>(records: &'a [CommitRecord]) -> Vec<GroupSummary<'a>> {
    let gap = CommitGap::default();
    let span = FirstToLast;
    group_commits(records)
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
/// wall-clock span.
pub fn subtitle(n: usize, elapsed_hours: f32) -> String {
    if n == 1 {
        "1 commit".to_string()
    } else if elapsed_hours > 0.0 {
        format!("{n} commits across {}", format_hours(elapsed_hours))
    } else {
        format!("{n} commits")
    }
}
