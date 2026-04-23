use crate::config::EstimationConfig;
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

/// Session-based estimator: break a ticket's commits into **work sessions**
/// separated by gaps longer than `session_break_min`. Each session's
/// contribution is `(last - first) + lead + trail`. Single-commit sessions
/// contribute `lead + trail`.
///
/// Replaces the old per-gap-clamp heuristic, which under-counted time for
/// users who commit sparsely (every 1–2 hours) while actually working
/// continuously in between.
pub struct SessionSpan {
    pub session_break_min: u32,
    pub lead_min: u32,
    pub trail_min: u32,
}

impl Default for SessionSpan {
    fn default() -> Self {
        Self {
            session_break_min: 120,
            lead_min: 15,
            trail_min: 15,
        }
    }
}

impl SessionSpan {
    pub fn from_config(cfg: &EstimationConfig) -> Self {
        Self {
            session_break_min: cfg.session_break_min,
            lead_min: cfg.lead_min,
            trail_min: cfg.trail_min,
        }
    }
}

impl HoursEstimator for SessionSpan {
    fn name(&self) -> &'static str {
        "session"
    }

    fn estimate(&self, commits: &[&CommitRecord]) -> Hours {
        if commits.is_empty() {
            return Hours::zero("no commits");
        }
        let mut times: Vec<i64> = commits.iter().map(|c| c.author_time).collect();
        times.sort_unstable();

        let break_secs = (self.session_break_min as i64) * 60;
        let buffer_secs = (self.lead_min as i64 + self.trail_min as i64) * 60;

        // Split times into sessions.
        let mut sessions: Vec<(i64, i64)> = Vec::new();
        let mut session_start = times[0];
        let mut session_end = times[0];
        for &t in &times[1..] {
            if t - session_end > break_secs {
                sessions.push((session_start, session_end));
                session_start = t;
            }
            session_end = t;
        }
        sessions.push((session_start, session_end));

        let n_sessions = sessions.len();
        let total_secs: i64 = sessions
            .iter()
            .map(|(s, e)| (e - s) + buffer_secs)
            .sum();
        let value = total_secs as f32 / 3600.0;
        let n = commits.len();
        let detail = if n_sessions == 1 {
            format!("{n} commits, 1 session")
        } else {
            format!("{n} commits, {n_sessions} sessions")
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
    fn session_single_commit_returns_lead_plus_trail() {
        let commits = vec![c(0)];
        let refs: Vec<&CommitRecord> = commits.iter().collect();
        let h = SessionSpan::default().estimate(&refs);
        assert!(approx(h.value, 0.5), "expected 0.5h, got {}", h.value);
    }

    #[test]
    fn session_three_close_commits_one_session() {
        // 10-minute gaps → one session spanning 20 minutes + 15+15 buffer.
        let commits = vec![c(0), c(600), c(1200)];
        let refs: Vec<&CommitRecord> = commits.iter().collect();
        let h = SessionSpan::default().estimate(&refs);
        let expected = (20.0 + 30.0) / 60.0;
        assert!(approx(h.value, expected), "expected {expected}h, got {}", h.value);
        assert_eq!(h.detail, "3 commits, 1 session");
    }

    #[test]
    fn session_two_splits_past_threshold() {
        // 121-minute gap (just past default 120 threshold) → two sessions.
        let commits = vec![c(0), c(121 * 60)];
        let refs: Vec<&CommitRecord> = commits.iter().collect();
        let h = SessionSpan::default().estimate(&refs);
        // Each single-commit session gets 30 min (lead + trail).
        let expected = 1.0;
        assert!(approx(h.value, expected), "expected {expected}h, got {}", h.value);
        assert_eq!(h.detail, "2 commits, 2 sessions");
    }

    #[test]
    fn session_stays_together_below_threshold() {
        // 60-minute gap (under 120) → one session of 60 min + buffer.
        let commits = vec![c(0), c(60 * 60)];
        let refs: Vec<&CommitRecord> = commits.iter().collect();
        let h = SessionSpan::default().estimate(&refs);
        let expected = 1.5; // 60m + 15 + 15 = 90m
        assert!(approx(h.value, expected), "expected {expected}h, got {}", h.value);
    }

    #[test]
    fn session_real_apr_23_timeline() {
        // Regression guard: 9 commits spanning 10:01–17:27, 120-min break.
        // From the user's real scan. Should yield ~6h 22m.
        // Times below are seconds since 10:01 (origin) per commit.
        let commits = vec![
            c(0),              // 10:01
            c(2 * 60),         // 10:03
            c((2 * 60 + 6) * 60),  // 12:07  (gap 124m → new session)
            c((3 * 60 + 42) * 60), // 13:43
            c((4 * 60 + 12) * 60), // 14:13
            c((4 * 60 + 24) * 60), // 14:25
            c((6 * 60 + 19) * 60), // 16:20  (gap 115m, still same session)
            c((7 * 60 + 21) * 60), // 17:22
            c((7 * 60 + 26) * 60), // 17:27
        ];
        let refs: Vec<&CommitRecord> = commits.iter().collect();
        let h = SessionSpan::default().estimate(&refs);
        // Session 1 (10:01–10:03) = 2m + 30m buffer = 32m
        // Session 2 (12:07–17:27) = 320m + 30m buffer = 350m
        // Total = 382m ≈ 6.366h
        assert!(h.value > 6.3 && h.value < 6.5, "got {}", h.value);
        assert_eq!(h.detail, "9 commits, 2 sessions");
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
