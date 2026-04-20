use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

pub fn extract(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"[A-Z][A-Z0-9]+-\d+").unwrap());
    let mut seen = BTreeSet::new();
    for m in re.find_iter(text) {
        seen.insert(m.as_str().to_owned());
    }
    seen.into_iter().collect()
}
