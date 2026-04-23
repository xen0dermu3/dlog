use std::collections::BTreeMap;

use crate::config::EstimationConfig;
use crate::hours::{format_hours, FirstToLast, Hours, HoursEstimator, SessionSpan};
use crate::scanner::CommitRecord;
use crate::tickets;

pub const UNTAGGED: &str = "(untagged)";

pub type Group<'a> = (String, Vec<&'a CommitRecord>);

pub struct GroupSummary<'a> {
    pub ticket: String,
    pub commits: Vec<&'a CommitRecord>,
    /// Primary hours used for display. Equals `explicit` if any commit
    /// message carried time markers; otherwise the SessionSpan estimate.
    pub gap: Hours,
    /// Wall-clock: first commit → last commit.
    pub span: Hours,
    /// Sum of explicit `[2h]`/`[30m]` markers found in commit subjects and
    /// bodies. `None` if no commit has one.
    pub explicit: Option<f32>,
    /// Pure SessionSpan estimate (independent of markers). Used as the
    /// weight for the `f` fill algorithm.
    pub session_weight: f32,
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
    group_with_hours_cfg(records, &EstimationConfig::default())
}

pub fn group_with_hours_cfg<'a>(
    records: &'a [CommitRecord],
    cfg: &EstimationConfig,
) -> Vec<GroupSummary<'a>> {
    let primary = SessionSpan::from_config(cfg);
    let span = FirstToLast;
    group_commits(records)
        .into_iter()
        .map(|(ticket, commits)| {
            let session_h = primary.estimate(&commits);
            let span_h = span.estimate(&commits);
            let explicit = sum_time_markers(&commits);
            let gap_h = match explicit {
                Some(h) => Hours {
                    value: h,
                    detail: "from commit messages".into(),
                },
                None => session_h.clone(),
            };
            GroupSummary {
                ticket,
                commits,
                gap: gap_h,
                span: span_h,
                explicit,
                session_weight: session_h.value,
            }
        })
        .collect()
}

/// Sum all `[2h]` / `[30m]` markers across a ticket's commits. Returns
/// `None` if no commit has one — caller uses that to decide whether the
/// number is authoritative or just estimated.
fn sum_time_markers(commits: &[&CommitRecord]) -> Option<f32> {
    let mut total = 0.0f32;
    let mut found = false;
    for c in commits {
        for t in tickets::extract_time_markers(&c.subject) {
            total += t;
            found = true;
        }
        for t in tickets::extract_time_markers(&c.body) {
            total += t;
            found = true;
        }
    }
    if found {
        Some(total)
    } else {
        None
    }
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
