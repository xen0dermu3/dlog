//! Provider-agnostic Pull Request types. Both `github` and `bitbucket`
//! modules produce values of these shapes so the rest of the app doesn't
//! care which host the PR came from.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Clone, Debug)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub head_branch: String,
    #[allow(dead_code)] // exposed for future "open PR in browser" action
    pub url: String,
    pub state: PrState,
    pub commit_oids: Vec<String>,
}
