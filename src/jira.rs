use anyhow::{bail, Context, Result};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::config::JiraConfig;

const KEYRING_SERVICE: &str = "dlog";

pub struct JiraClient {
    base_url: String,
    email: String,
    token: String,
}

#[derive(Clone, Debug)]
pub struct IssueInfo {
    pub key: String,
    pub summary: String,
    pub status: String,
}

#[derive(Debug)]
pub enum WorklogOutcome {
    Ok {
        #[allow(dead_code)] // surfaced in logs / future "jump to worklog in Jira"
        worklog_id: String,
    },
    Err {
        status: u16,
        message: String,
    },
}

impl JiraClient {
    /// Build a client from the on-disk config + the token stored in the OS
    /// keyring. Errors if the token isn't set, or if config is incomplete.
    pub fn from_config(cfg: &JiraConfig) -> Result<Self> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &cfg.email)
            .context("opening keyring entry")?;
        let token = entry
            .get_password()
            .context("reading Jira API token from keyring")?;
        Ok(Self {
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            email: cfg.email.clone(),
            token,
        })
    }

    pub fn post_worklog(
        &self,
        issue_key: &str,
        started: DateTime<FixedOffset>,
        seconds: u64,
    ) -> Result<WorklogOutcome> {
        if seconds == 0 {
            bail!("refusing to post a zero-second worklog for {issue_key}");
        }
        let url = format!(
            "{}/rest/api/3/issue/{}/worklog",
            self.base_url, issue_key
        );

        #[derive(Serialize)]
        struct Body {
            #[serde(rename = "timeSpentSeconds")]
            time_spent_seconds: u64,
            started: String,
        }
        // Jira wants the started time with milliseconds + timezone offset,
        // e.g. "2026-04-22T09:00:00.000+0000".
        let started_str = started.format("%Y-%m-%dT%H:%M:%S%.3f%z").to_string();
        let body = Body {
            time_spent_seconds: seconds,
            started: started_str,
        };

        let response = ureq::post(&url)
            .config()
            .http_status_as_error(false)
            .build()
            .header("Authorization", &format!("Basic {}", self.basic_auth()))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .send_json(&body);

        let mut response = match response {
            Ok(r) => r,
            Err(e) => {
                return Ok(WorklogOutcome::Err {
                    status: 0,
                    message: format!("{e}"),
                });
            }
        };

        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            let body: serde_json::Value = response
                .body_mut()
                .read_json()
                .context("parsing Jira response JSON")?;
            let id = body
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            Ok(WorklogOutcome::Ok { worklog_id: id })
        } else {
            let text = response
                .body_mut()
                .read_to_string()
                .unwrap_or_else(|_| "<no body>".to_string());
            let message = extract_error_message(&text).unwrap_or(text);
            Ok(WorklogOutcome::Err { status, message })
        }
    }

    /// Search Jira issues assigned to the current user whose status is in
    /// `statuses`. Empty statuses → empty result. Returns up to 50 issues
    /// ordered by most-recently-updated.
    pub fn search_issues(&self, statuses: &[String]) -> Result<Vec<IssueInfo>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let quoted: Vec<String> = statuses.iter().map(|s| format!("\"{s}\"")).collect();
        let jql = format!(
            "assignee = currentUser() AND status in ({}) ORDER BY updated DESC",
            quoted.join(",")
        );
        let url = format!("{}/rest/api/3/search/jql", self.base_url);
        let body = serde_json::json!({
            "jql": jql,
            "fields": ["summary", "status"],
            "maxResults": 50,
        });

        let mut response = ureq::post(&url)
            .config()
            .http_status_as_error(false)
            .build()
            .header("Authorization", &format!("Basic {}", self.basic_auth()))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .send_json(&body)
            .context("Jira search request")?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let text = response
                .body_mut()
                .read_to_string()
                .unwrap_or_else(|_| "<no body>".to_string());
            let message = extract_error_message(&text).unwrap_or(text);
            bail!("Jira search HTTP {status}: {message}");
        }

        #[derive(Deserialize)]
        struct SearchResp {
            issues: Vec<IssueRaw>,
        }
        #[derive(Deserialize)]
        struct IssueRaw {
            key: String,
            fields: FieldsRaw,
        }
        #[derive(Deserialize)]
        struct FieldsRaw {
            summary: String,
            status: StatusRaw,
        }
        #[derive(Deserialize)]
        struct StatusRaw {
            name: String,
        }

        let parsed: SearchResp = response
            .body_mut()
            .read_json()
            .context("parsing Jira search JSON")?;
        Ok(parsed
            .issues
            .into_iter()
            .map(|i| IssueInfo {
                key: i.key,
                summary: i.fields.summary,
                status: i.fields.status.name,
            })
            .collect())
    }

    fn basic_auth(&self) -> String {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine;
        B64.encode(format!("{}:{}", self.email, self.token))
    }
}

/// Write a new API token to the keyring for the given email.
pub fn save_token(email: &str, token: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, email).context("opening keyring entry")?;
    entry
        .set_password(token)
        .context("saving Jira API token to keyring")?;
    Ok(())
}

/// Best-effort extraction of a human-readable error from Jira's JSON payload.
fn extract_error_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if let Some(arr) = v.get("errorMessages").and_then(|v| v.as_array()) {
        let msgs: Vec<String> = arr
            .iter()
            .filter_map(|m| m.as_str().map(str::to_owned))
            .collect();
        if !msgs.is_empty() {
            return Some(msgs.join("; "));
        }
    }
    if let Some(obj) = v.get("errors").and_then(|v| v.as_object()) {
        let msgs: Vec<String> = obj
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| format!("{k}: {s}")))
            .collect();
        if !msgs.is_empty() {
            return Some(msgs.join("; "));
        }
    }
    None
}

/// Round an hours value to the nearest minute and return Jira-worklog seconds.
pub fn hours_to_jira_seconds(hours: f32) -> u64 {
    let minutes = (hours * 60.0).round().max(0.0) as u64;
    minutes * 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_to_seconds_rounds_to_minute() {
        assert_eq!(hours_to_jira_seconds(0.0), 0);
        assert_eq!(hours_to_jira_seconds(0.5), 1800);
        assert_eq!(hours_to_jira_seconds(1.0), 3600);
        assert_eq!(hours_to_jira_seconds(1.25), 75 * 60);
        assert_eq!(hours_to_jira_seconds(0.009), 60); // 0.54m rounds to 1m
    }

    #[test]
    fn negative_hours_are_clamped_to_zero() {
        assert_eq!(hours_to_jira_seconds(-1.0), 0);
    }

    #[test]
    fn extract_error_from_jira_payload() {
        let body = r#"{"errorMessages":["Issue Does Not Exist"],"errors":{}}"#;
        assert_eq!(
            extract_error_message(body),
            Some("Issue Does Not Exist".to_string())
        );

        let body2 = r#"{"errorMessages":[],"errors":{"timeSpentSeconds":"Must be > 0"}}"#;
        assert_eq!(
            extract_error_message(body2),
            Some("timeSpentSeconds: Must be > 0".to_string())
        );
    }
}
