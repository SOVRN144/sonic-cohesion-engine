use anyhow::Context;
use clap::{Parser, Subcommand};
use sce_core::{cll, constitution_advisor, policy_registry, storage, util};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::MissedTickBehavior;

#[derive(Debug, Parser)]
#[command(name = "sce_cli", about = "Sonic Cohesion Engine utility CLI")]
struct Cli {
    /// SQLite database URL (required for constitution canary/status commands).
    #[arg(long, default_value = "sqlite://./sce.db")]
    db_url: String,

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
    /// Constitution registry and policy rollout commands.
    Constitution {
        #[command(subcommand)]
        command: ConstitutionCommands,
    },
}

#[derive(Debug, Subcommand)]
enum ConstitutionCommands {
    /// Initialize registry from <ProjectRoot>/SCE/constitution.yaml.
    /// Example: sce_cli constitution init --project-root /tmp/project --version v1.0
    Init {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long, default_value = "v1.0")]
        version: String,
    },

    /// Add a candidate constitution YAML into registry/constitutions/<version>.yaml.
    /// Example: sce_cli constitution add --project-root /tmp/project --version v1.1 --file /tmp/v1.1.yaml
    Add {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        version: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        note: Option<String>,
    },

    /// Show active pointer, index versions, and DB-backed canary state.
    /// Example: sce_cli constitution status --project-root /tmp/project
    Status {
        #[arg(long)]
        project_root: PathBuf,
    },

    /// Build and write shadow impact to registry/shadow/<version>/impact.json.
    /// Example: sce_cli constitution shadow --project-root /tmp/project --version v1.1 --last-n 50
    Shadow {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        version: String,
        #[arg(long, default_value_t = 25)]
        last_n: usize,
    },

    /// Build deterministic impact report to registry/shadow/<version>/impact.json.
    /// Example: sce_cli constitution impact --project-root /tmp/project --version v1.1 --last-n 50
    Impact {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        version: String,
        #[arg(long, default_value_t = 25)]
        last_n: usize,
    },

    /// Set DB-authoritative canary budget for a candidate constitution version.
    /// Example: sce_cli constitution canary --project-root /tmp/project --version v1.1 --budget-runs 1
    Canary {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        version: String,
        #[arg(long, value_parser = clap::value_parser!(u64))]
        budget_runs: u64,
    },

    /// Activate an existing registry constitution version by updating active.json.
    /// Example: sce_cli constitution activate --project-root /tmp/project --version v1.1
    Activate {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        version: String,
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
        Commands::Constitution { command } => {
            run_constitution_command(&cli.db_url, command).await?
        }
    }

    Ok(())
}

async fn run_constitution_command(
    db_url: &str,
    command: ConstitutionCommands,
) -> anyhow::Result<()> {
    match command {
        ConstitutionCommands::Init {
            project_root,
            version,
        } => {
            let result =
                policy_registry::init_registry(&project_root, &version).with_context(|| {
                    format!(
                        "initialize constitution registry for {}",
                        project_root.to_string_lossy()
                    )
                })?;

            println!("active_version={}", result.active.version);
            println!("active_hash={}", result.active.hash);
            println!("active_path={}", result.active.path);
            println!("source_path={}", result.source_path.to_string_lossy());
        }
        ConstitutionCommands::Add {
            project_root,
            version,
            file,
            note,
        } => {
            let result =
                policy_registry::add_registry_constitution(&project_root, &version, &file, note)
                    .with_context(|| {
                        format!(
                            "add constitution version {} for {}",
                            version,
                            project_root.to_string_lossy()
                        )
                    })?;

            println!("added_version={}", result.version);
            println!("added_hash={}", result.hash);
            println!("added_path={}", result.path.to_string_lossy());
        }
        ConstitutionCommands::Status { project_root } => {
            let db = connect_db(db_url).await?;
            let status = policy_registry::status(&db, &project_root)
                .await
                .with_context(|| {
                    format!(
                        "read constitution status for {}",
                        project_root.to_string_lossy()
                    )
                })?;

            println!(
                "project_root={}",
                status.canonical_project_root.to_string_lossy()
            );

            if let Some(active) = status.active {
                println!("active_version={}", active.version);
                println!("active_hash={}", active.hash);
                println!("active_path={}", active.path);
            } else {
                println!("active_version=");
                println!("active_hash=");
                println!("active_path=");
            }

            println!("registry_versions_count={}", status.index.versions.len());
            for entry in status.index.versions {
                println!(
                    "registry_version={} hash={} path={}",
                    entry.version, entry.hash, entry.path
                );
            }

            if let Some(canary) = status.canary_state {
                println!("canary_candidate_version={}", canary.candidate_version);
                println!("canary_remaining_budget={}", canary.remaining_budget);
                println!("canary_started_at={}", canary.started_at);
                println!("canary_updated_at={}", canary.updated_at);
            } else {
                println!("canary_candidate_version=");
                println!("canary_remaining_budget=0");
            }

            for warning in status.version_warnings {
                println!("warning={warning}");
            }
        }
        ConstitutionCommands::Shadow {
            project_root,
            version,
            last_n,
        } => {
            let result = policy_registry::build_shadow_impact(&project_root, &version, last_n)
                .with_context(|| {
                    format!(
                        "build shadow impact for version {} under {}",
                        version,
                        project_root.to_string_lossy()
                    )
                })?;
            print_impact_summary(&result);
        }
        ConstitutionCommands::Impact {
            project_root,
            version,
            last_n,
        } => {
            let result = policy_registry::build_impact(&project_root, &version, last_n)
                .with_context(|| {
                    format!(
                        "build impact report for version {} under {}",
                        version,
                        project_root.to_string_lossy()
                    )
                })?;
            print_impact_summary(&result);
        }
        ConstitutionCommands::Canary {
            project_root,
            version,
            budget_runs,
        } => {
            let db = connect_db(db_url).await?;
            let state =
                policy_registry::set_canary_budget(&db, &project_root, &version, budget_runs)
                    .await
                    .with_context(|| {
                        format!(
                            "set canary for version {} under {}",
                            version,
                            project_root.to_string_lossy()
                        )
                    })?;

            println!("project_id={}", state.project_id);
            println!("candidate_version={}", state.candidate_version);
            println!("remaining_budget={}", state.remaining_budget);
            println!("started_at={}", state.started_at);
            println!("updated_at={}", state.updated_at);
        }
        ConstitutionCommands::Activate {
            project_root,
            version,
        } => {
            let activated = policy_registry::activate_registry_version(&project_root, &version)
                .with_context(|| {
                    format!(
                        "activate constitution version {} under {}",
                        version,
                        project_root.to_string_lossy()
                    )
                })?;

            println!("active_version={}", activated.active.version);
            println!("active_hash={}", activated.active.hash);
            println!("active_path={}", activated.active.path);
        }
    }

    Ok(())
}

async fn connect_db(db_url: &str) -> anyhow::Result<storage::Db> {
    let db = storage::Db::connect(db_url)
        .await
        .with_context(|| format!("connect db for CLI command using {}", db_url))?;
    storage::enforce_startup_invariants(&db)
        .await
        .with_context(|| format!("enforce startup invariants for {db_url}"))?;
    Ok(db)
}

fn print_impact_summary(result: &policy_registry::ImpactResult) {
    println!("output_path={}", result.output_path.to_string_lossy());
    println!("schema_version={}", result.report.schema_version);
    println!("candidate_version={}", result.report.candidate_version);
    println!("candidate_hash={}", result.report.candidate_hash);
    println!("requested_last_n={}", result.report.requested_last_n);
    println!("selected_run_count={}", result.report.selected_run_count);
    println!("new_blockers_count={}", result.report.new_blockers_count);
    println!("new_fail_count={}", result.report.new_fail_count);
    println!("new_fail_share={:.6}", result.report.new_fail_share);
    println!("risk_level={:?}", result.report.risk_level);
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
    use super::{Cli, Commands, ConstitutionCommands};
    use clap::{CommandFactory, Parser};

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
    fn parses_constitution_subcommands() {
        let cli = Cli::try_parse_from([
            "sce_cli",
            "constitution",
            "impact",
            "--project-root",
            "/tmp/project",
            "--version",
            "v1.1",
            "--last-n",
            "50",
        ])
        .expect("parse");

        match cli.command {
            Commands::Constitution { command } => match command {
                ConstitutionCommands::Impact {
                    project_root,
                    version,
                    last_n,
                } => {
                    assert_eq!(project_root, std::path::PathBuf::from("/tmp/project"));
                    assert_eq!(version, "v1.1");
                    assert_eq!(last_n, 50);
                }
                _ => panic!("expected impact subcommand"),
            },
            _ => panic!("expected constitution command"),
        }
    }

    #[test]
    fn constitution_help_contains_examples() {
        let mut root = Cli::command();
        let constitution = root
            .find_subcommand_mut("constitution")
            .expect("constitution subcommand");

        let expected = [
            ("init", "Example: sce_cli constitution init"),
            ("add", "Example: sce_cli constitution add"),
            ("status", "Example: sce_cli constitution status"),
            ("shadow", "Example: sce_cli constitution shadow"),
            ("impact", "Example: sce_cli constitution impact"),
            ("canary", "Example: sce_cli constitution canary"),
            ("activate", "Example: sce_cli constitution activate"),
        ];

        for (name, snippet) in expected {
            let about = constitution
                .find_subcommand_mut(name)
                .and_then(|cmd| cmd.get_long_about().or(cmd.get_about()))
                .map(|value| value.to_string())
                .expect("subcommand about");
            assert!(
                about.contains(snippet),
                "expected {name} about to contain snippet {snippet}, got: {about}"
            );
        }
    }
}
