mod config;
mod fuzzy;
mod hours;
mod jira;
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
