use std::collections::BTreeMap;

use chrono::{Local, TimeZone};

use crate::scanner::CommitRecord;
use crate::tickets;

const UNTAGGED: &str = "(untagged)";

pub fn print_grouped(records: &[CommitRecord]) {
    if records.is_empty() {
        println!("(no matching commits)");
        return;
    }

    let mut groups: BTreeMap<String, Vec<&CommitRecord>> = BTreeMap::new();

    for r in records {
        let mut keys: Vec<String> = tickets::extract(&r.branch_at_head);
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

    let mut names: Vec<String> = groups
        .keys()
        .filter(|k| k.as_str() != UNTAGGED)
        .cloned()
        .collect();
    if groups.contains_key(UNTAGGED) {
        names.push(UNTAGGED.to_string());
    }

    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let commits = &groups[name];
        let n = commits.len();
        println!("{} ({} commit{})", name, n, if n == 1 { "" } else { "s" });
        let mut sorted: Vec<&&CommitRecord> = commits.iter().collect();
        sorted.sort_by_key(|c| c.author_time);
        for c in sorted {
            let hm = Local
                .timestamp_opt(c.author_time, 0)
                .single()
                .map(|dt| dt.format("%H:%M").to_string())
                .unwrap_or_else(|| "--:--".to_string());
            let short = &c.oid[..7.min(c.oid.len())];
            println!("  {}  {}  {}", hm, short, c.subject);
        }
    }
}
