use anyhow::Context;
use clap::{Parser, Subcommand};
use sce_core::cll;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "sce_cli", about = "Sonic Cohesion Engine utility CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Generate Commit 4 CLL trends + diversity artifact for a project root.
    Trends {
        /// Absolute or relative project root path.
        #[arg(long)]
        project_root: PathBuf,

        /// Last N valid runs to include. Use 0 for full valid history.
        #[arg(long, default_value_t = 25)]
        last_n: usize,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Trends {
            project_root,
            last_n,
        } => {
            let summary =
                cll::generate_project_trends(&project_root, last_n).with_context(|| {
                    format!(
                        "generate project trends for root {}",
                        project_root.to_string_lossy()
                    )
                })?;

            println!("requested_last_n={}", summary.requested_last_n);
            println!("selected_run_count={}", summary.selected_run_count);
            println!("valid_runs={}", summary.valid_runs);
            println!("skipped_runs={}", summary.skipped_runs);
            println!("data_gaps={}", summary.data_gaps);
            println!("baseline_run_count={}", summary.baseline_run_count);
            println!("recent_run_count={}", summary.recent_run_count);
            println!("warn_alert_count={}", summary.warn_alert_count);
            println!("info_alert_count={}", summary.info_alert_count);
            println!("output_path={}", summary.output_path.to_string_lossy());
        }
    }

    Ok(())
}
