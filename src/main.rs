mod config;
mod fuzzy;
mod pr;
mod report;
mod scanner;
mod tickets;
mod tui;

use std::path::PathBuf;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{Parser, Subcommand};

use crate::pr::PrEnrichment;

#[derive(Parser)]
#[command(name = "dlog", version, about = "Personal git -> Jira daily logger")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Scan one or more repos and print a day's commits grouped by Jira ticket.
    Scan {
        /// Paths to git repositories. Defaults to current directory.
        paths: Vec<PathBuf>,

        /// Date to scan in YYYY-MM-DD (local timezone). Defaults to today.
        #[arg(long)]
        date: Option<NaiveDate>,

        /// Enrich ticket extraction with GitHub PR title/body/head-branch via `gh` CLI.
        #[arg(long)]
        with_prs: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => tui::run(),
        Some(Command::Scan {
            paths,
            date,
            with_prs,
        }) => {
            let paths = if paths.is_empty() {
                vec![PathBuf::from(".")]
            } else {
                paths
            };

            let mut all_records = Vec::new();
            let mut pr_merged: Option<PrEnrichment> = None;

            for path in &paths {
                let mut records = scanner::scan(path, date)?;
                all_records.append(&mut records);

                if with_prs {
                    let pr = pr::fetch(path)?;
                    match &mut pr_merged {
                        Some(existing) => existing.merge(pr),
                        None => pr_merged = Some(pr),
                    }
                }
            }

            all_records.sort_by_key(|r| r.author_time);
            report::print_grouped(&all_records, pr_merged.as_ref());
            Ok(())
        }
    }
}
