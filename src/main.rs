mod bitbucket;
mod config;
mod fuzzy;
mod github;
mod hours;
mod jira;
mod pr;
mod report;
mod scanner;
mod standup;
mod store;
mod tickets;
mod tui;

use anyhow::Result;

fn main() -> Result<()> {
    tui::run()
}
