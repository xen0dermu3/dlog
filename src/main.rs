mod report;
mod scanner;
mod tickets;

use std::path::PathBuf;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dlog", version, about = "Personal git -> Jira daily logger")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan a repo and print a day's commits grouped by Jira ticket.
    Scan {
        /// Path to the git repository (default: current directory).
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Date to scan in YYYY-MM-DD (local timezone). Defaults to today.
        #[arg(long)]
        date: Option<NaiveDate>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan { path, date } => {
            let records = scanner::scan(&path, date)?;
            report::print_grouped(&records);
        }
    }
    Ok(())
}
