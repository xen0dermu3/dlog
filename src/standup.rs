use anyhow::Result;
use chrono::{Duration, NaiveDate};

use crate::config::Config;
use crate::scanner::{self, CommitRecord};

pub const IN_FLIGHT_CUTOFF_DAYS: i64 = 7;

pub struct StandupReport {
    pub yesterday: NaiveDate,
    pub yesterday_records: Vec<CommitRecord>,
    pub in_flight_records: Vec<CommitRecord>,
}

/// Compose a standup report: "what did I do yesterday" + "what's still in
/// flight (unpushed) across my repos". Per-repo errors are suppressed
/// individually so one bad repo doesn't kill the whole view.
pub fn build(config: &Config, today: NaiveDate) -> Result<StandupReport> {
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

    Ok(StandupReport {
        yesterday,
        yesterday_records,
        in_flight_records,
    })
}
