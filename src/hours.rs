use crate::scanner::CommitRecord;

#[derive(Clone, Debug)]
pub struct Hours {
    pub value: f32,
    #[allow(dead_code)] // surfaced by UI in a later step (tooltip / expanded view)
    pub detail: String,
}

impl Hours {
    pub fn display(&self) -> String {
        format_hours(self.value)
    }

    pub fn zero(detail: impl Into<String>) -> Self {
        Self {
            value: 0.0,
            detail: detail.into(),
        }
    }
}

/// Render a duration in minutes / hours+minutes — e.g. `7m`, `30m`, `1h`,
/// `1h 15m`, `2h 20m`. Rounded to the nearest minute.
pub fn format_hours(hours: f32) -> String {
    let minutes = (hours * 60.0).round() as i64;
    if minutes < 60 {
        format!("{minutes}m")
    } else {
        let h = minutes / 60;
        let m = minutes % 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h {m}m")
        }
    }
}

/// Parse a user-friendly duration string into hours. Accepts:
///   `30m`, `2h`, `2h 30m`, `2h30m`, `1.5h`, `2.5` (bare number = hours).
/// Returns `None` on empty / negative / unparseable input.
pub fn parse_duration(input: &str) -> Option<f32> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Bare number — interpret as hours (back-compat).
    if let Ok(v) = trimmed.parse::<f32>() {
        return if v >= 0.0 { Some(v) } else { None };
    }

    // Normalise: lowercase, strip whitespace.
    let normalised: String = trimmed
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();

    let mut remaining = normalised.as_str();
    let mut total_min: f32 = 0.0;
    let mut found = false;

    if let Some(idx) = remaining.find('h') {
        let (hours_str, rest) = remaining.split_at(idx);
        let hours: f32 = hours_str.parse().ok()?;
        if hours < 0.0 {
            return None;
        }
        total_min += hours * 60.0;
        remaining = &rest[1..];
        found = true;
    }
    if let Some(idx) = remaining.find('m') {
        let (mins_str, rest) = remaining.split_at(idx);
        let mins: f32 = mins_str.parse().ok()?;
        if mins < 0.0 {
            return None;
        }
        total_min += mins;
        remaining = &rest[1..];
        found = true;
    }

    if !remaining.is_empty() || !found {
        return None;
    }
    Some(total_min / 60.0)
}

pub trait HoursEstimator {
    #[allow(dead_code)] // used once multiple estimators are disambiguated in UI
    fn name(&self) -> &'static str;
    fn estimate(&self, commits: &[&CommitRecord]) -> Hours;
}

pub struct CommitGap {
    pub idle_gap_min: u32,
    pub lead_min: u32,
    pub trail_min: u32,
}

impl Default for CommitGap {
    fn default() -> Self {
        Self {
            idle_gap_min: 30,
            lead_min: 15,
            trail_min: 15,
        }
    }
}

impl HoursEstimator for CommitGap {
    fn name(&self) -> &'static str {
        "gap"
    }

    fn estimate(&self, commits: &[&CommitRecord]) -> Hours {
        if commits.is_empty() {
            return Hours::zero("no commits");
        }
        if commits.len() == 1 {
            let mins = self.lead_min + self.trail_min;
            return Hours {
                value: mins as f32 / 60.0,
                detail: format!("single commit ({} min default)", mins),
            };
        }
        let mut times: Vec<i64> = commits.iter().map(|c| c.author_time).collect();
        times.sort_unstable();
        let idle_seconds = (self.idle_gap_min as i64) * 60;
        let mut total: i64 = 0;
        let mut clamps: usize = 0;
        for w in times.windows(2) {
            let gap = w[1] - w[0];
            if gap <= idle_seconds {
                total += gap;
            } else {
                total += idle_seconds;
                clamps += 1;
            }
        }
        total += (self.lead_min as i64) * 60;
        total += (self.trail_min as i64) * 60;
        let value = total as f32 / 3600.0;
        let n = commits.len();
        let detail = if clamps == 0 {
            format!("{n} commits")
        } else {
            format!(
                "{n} commits, {clamps} gap{} clamped",
                if clamps == 1 { "" } else { "s" }
            )
        };
        Hours { value, detail }
    }
}

pub struct FirstToLast;

impl HoursEstimator for FirstToLast {
    fn name(&self) -> &'static str {
        "span"
    }

    fn estimate(&self, commits: &[&CommitRecord]) -> Hours {
        if commits.is_empty() {
            return Hours::zero("no commits");
        }
        if commits.len() == 1 {
            return Hours::zero("single commit");
        }
        let min = commits.iter().map(|c| c.author_time).min().unwrap();
        let max = commits.iter().map(|c| c.author_time).max().unwrap();
        Hours {
            value: (max - min) as f32 / 3600.0,
            detail: "first to last commit".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(ts: i64) -> CommitRecord {
        CommitRecord {
            oid: format!("{ts:07x}"),
            author_time: ts,
            subject: "x".into(),
            body: String::new(),
            branches: String::new(),
            repo: "r".into(),
        }
    }

    // Approximate-equal for floating point.
    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn format_hours_under_one_hour() {
        assert_eq!(format_hours(0.0), "0m");
        assert_eq!(format_hours(0.1), "6m");
        assert_eq!(format_hours(0.5), "30m");
        assert_eq!(format_hours(0.983), "59m");
    }

    #[test]
    fn format_hours_at_or_above_one_hour() {
        assert_eq!(format_hours(1.0), "1h");
        assert_eq!(format_hours(1.25), "1h 15m");
        assert_eq!(format_hours(2.333), "2h 20m");
    }

    #[test]
    fn parse_duration_minutes_hours_and_combo() {
        assert_eq!(parse_duration("30m"), Some(0.5));
        assert_eq!(parse_duration("2h"), Some(2.0));
        assert_eq!(parse_duration("2h 30m"), Some(2.5));
        assert_eq!(parse_duration("2h30m"), Some(2.5));
        assert_eq!(parse_duration("1.5h"), Some(1.5));
        assert_eq!(parse_duration("45M"), Some(0.75)); // case-insensitive
    }

    #[test]
    fn parse_duration_bare_number_is_hours() {
        assert_eq!(parse_duration("2.5"), Some(2.5));
        assert_eq!(parse_duration("0"), Some(0.0));
    }

    #[test]
    fn parse_duration_rejects_invalid() {
        assert!(parse_duration("").is_none());
        assert!(parse_duration("   ").is_none());
        assert!(parse_duration("foo").is_none());
        assert!(parse_duration("-1h").is_none());
        assert!(parse_duration("2h30").is_none(), "trailing digits without unit");
        assert!(parse_duration("2h m").is_none(), "empty minutes component");
    }

    #[test]
    fn gap_single_commit_returns_lead_plus_trail() {
        let commits = vec![c(0)];
        let refs: Vec<&CommitRecord> = commits.iter().collect();
        let h = CommitGap::default().estimate(&refs);
        assert!(approx(h.value, 0.5), "expected 0.5h, got {}", h.value);
    }

    #[test]
    fn gap_three_close_commits_sums_gaps_plus_buffers() {
        // 10-minute gaps → 20 minutes inside + 15+15 buffer = 50 minutes.
        let commits = vec![c(0), c(600), c(1200)];
        let refs: Vec<&CommitRecord> = commits.iter().collect();
        let h = CommitGap::default().estimate(&refs);
        let expected = (20.0 + 30.0) / 60.0;
        assert!(approx(h.value, expected), "expected {expected}h, got {}", h.value);
        assert_eq!(h.detail, "3 commits");
    }

    #[test]
    fn gap_clamps_idle_periods() {
        // Two commits 2 hours apart → clamped to 30 min + 30 min buffer = 60 min.
        let commits = vec![c(0), c(7200)];
        let refs: Vec<&CommitRecord> = commits.iter().collect();
        let h = CommitGap::default().estimate(&refs);
        assert!(approx(h.value, 1.0), "expected 1.0h, got {}", h.value);
        assert!(
            h.detail.contains("clamped"),
            "detail should mention clamp: {}",
            h.detail
        );
    }

    #[test]
    fn span_single_commit_is_zero() {
        let commits = vec![c(0)];
        let refs: Vec<&CommitRecord> = commits.iter().collect();
        let h = FirstToLast.estimate(&refs);
        assert!(approx(h.value, 0.0));
    }

    #[test]
    fn span_measures_first_to_last() {
        // 09:00 to 11:30 = 2.5 hours.
        let commits = vec![c(0), c(3600), c(9000)];
        let refs: Vec<&CommitRecord> = commits.iter().collect();
        let h = FirstToLast.estimate(&refs);
        assert!(approx(h.value, 2.5), "expected 2.5h, got {}", h.value);
    }
}
