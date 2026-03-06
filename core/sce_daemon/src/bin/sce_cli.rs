use anyhow::Context;
use clap::{Parser, Subcommand};
use sce_core::{cll, constitution_advisor, util};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::MissedTickBehavior;

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
    /// Foreground trend generation loop (governance-only; no daemon integration).
    WatchTrends {
        /// Absolute or relative project root path.
        #[arg(long)]
        project_root: PathBuf,

        /// Last N valid runs to include. Use 0 for full valid history (example: --last-n 0).
        #[arg(long, default_value_t = 25)]
        last_n: usize,

        /// Interval between trend regeneration cycles (seconds).
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        interval_seconds: u64,
    },
    /// Suggest constitution corridors from observed metrics (advisory only).
    SuggestConstitution {
        /// Absolute or relative project root path.
        #[arg(long)]
        project_root: PathBuf,

        /// Last N valid runs to include. Use 0 for full valid history (example: --last-n 0).
        #[arg(long, default_value_t = 25)]
        last_n: usize,

        /// Output file path (default: <ProjectRoot>/SCE/constitution.suggested.yaml).
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Trends {
            project_root,
            last_n,
        } => run_trends_once(&project_root, last_n)?,
        Commands::WatchTrends {
            project_root,
            last_n,
            interval_seconds,
        } => run_watch_trends(&project_root, last_n, interval_seconds).await?,
        Commands::SuggestConstitution {
            project_root,
            last_n,
            out,
        } => run_suggest_constitution(&project_root, last_n, out.as_deref())?,
    }

    Ok(())
}

fn run_trends_once(project_root: &Path, last_n: usize) -> anyhow::Result<()> {
    let summary = cll::generate_project_trends(project_root, last_n).with_context(|| {
        format!(
            "generate project trends for root {}",
            project_root.to_string_lossy()
        )
    })?;
    print_trends_summary(&summary);
    Ok(())
}

fn run_suggest_constitution(
    project_root: &Path,
    last_n: usize,
    out: Option<&Path>,
) -> anyhow::Result<()> {
    let summary = constitution_advisor::suggest_constitution(project_root, last_n, out)
        .with_context(|| {
            format!(
                "suggest constitution for root {}",
                project_root.to_string_lossy()
            )
        })?;
    println!("requested_last_n={}", summary.requested_last_n);
    println!("selected_run_count={}", summary.selected_run_count);
    println!("diversity_warn={}", summary.diversity_warn);
    println!("output_path={}", summary.output_path.to_string_lossy());
    Ok(())
}

async fn run_watch_trends(
    project_root: &Path,
    last_n: usize,
    interval_seconds: u64,
) -> anyhow::Result<()> {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_seconds));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await;

    let mut cycle: u64 = 0;
    let mut previous_normalized: Option<Value> = None;
    loop {
        cycle += 1;
        let cycle_result = run_watch_cycle(project_root, last_n, previous_normalized.as_ref())
            .with_context(|| format!("watch-trends cycle {cycle}"))?;
        previous_normalized = Some(cycle_result.normalized_trends_json);

        println!(
            "cycle={} heartbeat_at={} status={} selected_run_count={} valid_runs={} skipped_runs={} warn_alert_count={} info_alert_count={} output_path={}",
            cycle,
            util::now_rfc3339(),
            if cycle_result.updated {
                "updated"
            } else {
                "unchanged"
            },
            cycle_result.summary.selected_run_count,
            cycle_result.summary.valid_runs,
            cycle_result.summary.skipped_runs,
            cycle_result.summary.warn_alert_count,
            cycle_result.summary.info_alert_count,
            cycle_result.summary.output_path.to_string_lossy()
        );

        ticker.tick().await;
    }
}

#[derive(Debug)]
struct WatchCycleResult {
    summary: cll::TrendsGenerationSummary,
    normalized_trends_json: Value,
    updated: bool,
}

fn run_watch_cycle(
    project_root: &Path,
    last_n: usize,
    previous_normalized: Option<&Value>,
) -> anyhow::Result<WatchCycleResult> {
    let summary = cll::generate_project_trends(project_root, last_n).with_context(|| {
        format!(
            "generate project trends for root {}",
            project_root.to_string_lossy()
        )
    })?;
    let normalized = load_normalized_trends_json(&summary.output_path)?;
    let updated = previous_normalized
        .map(|previous| previous != &normalized)
        .unwrap_or(true);

    Ok(WatchCycleResult {
        summary,
        normalized_trends_json: normalized,
        updated,
    })
}

fn load_normalized_trends_json(path: &Path) -> anyhow::Result<Value> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read trends file for watch status: {}", path.display()))?;
    let parsed = serde_json::from_str::<Value>(&content)
        .with_context(|| format!("parse trends JSON for watch status: {}", path.display()))?;
    Ok(normalize_trends_json(parsed))
}

fn normalize_trends_json(mut value: Value) -> Value {
    // mask only meta.generated_at before comparison
    if let Some(meta) = value.get_mut("meta").and_then(Value::as_object_mut) {
        meta.insert(
            "generated_at".to_string(),
            Value::String("normalized".to_string()),
        );
    }
    value
}

fn print_trends_summary(summary: &cll::TrendsGenerationSummary) {
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

#[cfg(test)]
mod tests {
    use super::{run_watch_cycle, Cli, Commands};
    use clap::Parser;

    #[test]
    fn parses_watch_trends_defaults() {
        let cli =
            Cli::try_parse_from(["sce_cli", "watch-trends", "--project-root", "/tmp/project"])
                .expect("parse");

        match cli.command {
            Commands::WatchTrends {
                project_root,
                last_n,
                interval_seconds,
            } => {
                assert_eq!(project_root, std::path::PathBuf::from("/tmp/project"));
                assert_eq!(last_n, 25);
                assert_eq!(interval_seconds, 30);
            }
            _ => panic!("expected watch-trends command"),
        }
    }

    #[test]
    fn parses_suggest_constitution_defaults() {
        let cli = Cli::try_parse_from([
            "sce_cli",
            "suggest-constitution",
            "--project-root",
            "/tmp/project",
        ])
        .expect("parse");

        match cli.command {
            Commands::SuggestConstitution {
                project_root,
                last_n,
                out,
            } => {
                assert_eq!(project_root, std::path::PathBuf::from("/tmp/project"));
                assert_eq!(last_n, 25);
                assert!(out.is_none());
            }
            _ => panic!("expected suggest-constitution command"),
        }
    }

    #[test]
    fn watch_cycle_detects_unchanged_when_only_generated_at_differs() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        std::fs::create_dir_all(project_root.join("SCE/assets")).expect("mkdir");

        let first = run_watch_cycle(&project_root, 0, None).expect("first cycle");
        assert!(first.updated);

        let second = run_watch_cycle(&project_root, 0, Some(&first.normalized_trends_json))
            .expect("second cycle");
        assert!(!second.updated);
    }
}
