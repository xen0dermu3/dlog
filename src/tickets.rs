use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::hours::parse_duration;

pub fn extract(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"[A-Z][A-Z0-9]+-\d+").unwrap());
    let mut seen = BTreeSet::new();
    for m in re.find_iter(text) {
        seen.insert(m.as_str().to_owned());
    }
    seen.into_iter().collect()
}

/// Pull explicit time markers out of a commit message, e.g. `[2h]`,
/// `[30m]`, `[1h 15m]`. Anything bracketed that doesn't parse as a
/// duration (e.g. `[TODO]`, `[WIP]`) is ignored. Returns hours.
pub fn extract_time_markers(text: &str) -> Vec<f32> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\[([^\]]+)\]").unwrap());
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        if let Some(inner) = cap.get(1) {
            if let Some(h) = parse_duration(inner.as_str()) {
                if h > 0.0 {
                    out.push(h);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: &[f32], b: &[f32]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3)
    }

    #[test]
    fn time_markers_bracketed_durations() {
        assert!(approx_eq(&extract_time_markers("fix [30m]"), &[0.5]));
        assert!(approx_eq(&extract_time_markers("[1h] and [2h 30m]"), &[1.0, 2.5]));
        assert!(approx_eq(&extract_time_markers("[1.5h]"), &[1.5]));
    }

    #[test]
    fn time_markers_ignores_non_duration_brackets() {
        assert!(extract_time_markers("[TODO] do thing").is_empty());
        assert!(extract_time_markers("no markers at all").is_empty());
        assert!(extract_time_markers("[WIP] [refactor]").is_empty());
    }

    #[test]
    fn time_markers_mixed() {
        // Ignores non-duration brackets, collects the rest.
        assert!(approx_eq(
            &extract_time_markers("[WIP] init login [1h 30m]"),
            &[1.5]
        ));
    }
}
