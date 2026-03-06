use anyhow::Context;
use clap::{Parser, Subcommand};
use sce_core::{
    ci_check, cll, constitution_advisor, graph, operators, plugin_chain, policy_registry,
    ref_canon, storage, translation_matrix, util,
};
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
    /// Reference canon management.
    Refs {
        #[command(subcommand)]
        command: RefCommands,
    },
    /// Operator registry and deterministic operator runs.
    Operators {
        #[command(subcommand)]
        command: OperatorCommands,
    },
    /// Governance-only CI summary for evaluated runs.
    CiCheck {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long, default_value_t = 25)]
        last_n: usize,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        json_out: Option<PathBuf>,
    },
    /// Translation matrix scaffold commands.
    Translation {
        #[command(subcommand)]
        command: TranslationCommands,
    },
    /// Stem graph lineage commands.
    Graph {
        #[command(subcommand)]
        command: GraphCommands,
    },
    /// Plugin chain sidecar inspection.
    Chain {
        #[command(subcommand)]
        command: ChainCommands,
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

#[derive(Debug, Subcommand)]
enum RefCommands {
    Add {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "")]
        tags: String,
        #[arg(long, default_value_t = false)]
        copy_file: bool,
    },
    List {
        #[arg(long)]
        project_root: PathBuf,
    },
    Show {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        content_hash: String,
    },
    Remove {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        content_hash: String,
    },
}

#[derive(Debug, Subcommand)]
enum OperatorCommands {
    List,
    Run {
        name: String,
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long, default_value_t = 25)]
        last_n: usize,
    },
}

#[derive(Debug, Subcommand)]
enum TranslationCommands {
    Run {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        run_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum GraphCommands {
    Link {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
    ExportDot {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum ChainCommands {
    Show {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        run_id: String,
    },
}

#[tokio::main]
async fn main() {
    let code = match run_cli().await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{err:#}");
            1
        }
    };
    std::process::exit(code);
}

async fn run_cli() -> anyhow::Result<i32> {
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
        Commands::Refs { command } => run_refs_command(command)?,
        Commands::Operators { command } => run_operator_command(command)?,
        Commands::CiCheck {
            project_root,
            last_n,
            run_id,
            json_out,
        } => {
            let summary = ci_check::build_summary(&project_root, run_id.as_deref(), last_n)
                .with_context(|| format!("build ci summary for {}", project_root.display()))?;
            if let Some(output_path) = json_out.as_deref() {
                ci_check::write_json_out(&summary, output_path).with_context(|| {
                    format!("write ci summary json to {}", output_path.display())
                })?;
            }
            print_json(&summary)?;
            return Ok(ci_check::exit_code(&summary));
        }
        Commands::Translation { command } => run_translation_command(command)?,
        Commands::Graph { command } => run_graph_command(command)?,
        Commands::Chain { command } => run_chain_command(command)?,
    }

    Ok(0)
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

fn run_refs_command(command: RefCommands) -> anyhow::Result<()> {
    match command {
        RefCommands::Add {
            project_root,
            file,
            name,
            tags,
            copy_file,
        } => {
            let output =
                ref_canon::add_ref(&project_root, &file, name.as_deref(), &tags, copy_file)?;
            print_json(&output)?;
        }
        RefCommands::List { project_root } => {
            let output = ref_canon::list_refs(&project_root)?;
            print_json(&output)?;
        }
        RefCommands::Show {
            project_root,
            content_hash,
        } => {
            let output = ref_canon::show_ref(&project_root, &content_hash)?;
            print_json(&output)?;
        }
        RefCommands::Remove {
            project_root,
            content_hash,
        } => {
            let output = ref_canon::remove_ref(&project_root, &content_hash)?;
            print_json(&output)?;
        }
    }
    Ok(())
}

fn run_operator_command(command: OperatorCommands) -> anyhow::Result<()> {
    match command {
        OperatorCommands::List => {
            print_json(&operators::list())?;
        }
        OperatorCommands::Run {
            name,
            project_root,
            run_id,
            last_n,
        } => {
            let result = operators::run(
                operators::OperatorRunRequest {
                    project_root: &project_root,
                    run_id: run_id.as_deref(),
                    last_n,
                },
                &name,
            )?;
            let _ = &result.output_path;
            print_json(&result.output)?;
        }
    }
    Ok(())
}

fn run_translation_command(command: TranslationCommands) -> anyhow::Result<()> {
    match command {
        TranslationCommands::Run {
            project_root,
            run_id,
        } => {
            let (output, _output_path) = translation_matrix::run(&project_root, &run_id)?;
            print_json(&output)?;
        }
    }
    Ok(())
}

fn run_graph_command(command: GraphCommands) -> anyhow::Result<()> {
    match command {
        GraphCommands::Link {
            project_root,
            from,
            to,
        } => {
            let output = graph::link(&project_root, &from, &to)?;
            print_json(&output)?;
        }
        GraphCommands::ExportDot { project_root, out } => {
            let dot = graph::export_dot(&project_root)?;
            if let Some(output_path) = out {
                if let Some(parent) = output_path.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("create graph dot directory: {}", parent.display())
                    })?;
                }
                std::fs::write(&output_path, dot.as_bytes())
                    .with_context(|| format!("write graph dot: {}", output_path.display()))?;
            }
            print!("{dot}");
        }
    }
    Ok(())
}

fn run_chain_command(command: ChainCommands) -> anyhow::Result<()> {
    match command {
        ChainCommands::Show {
            project_root,
            run_id,
        } => {
            let output = plugin_chain::show(&project_root, &run_id)?;
            print_json(&output)?;
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

fn print_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
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
    use super::{
        ChainCommands, Cli, Commands, ConstitutionCommands, GraphCommands, OperatorCommands,
        RefCommands, TranslationCommands,
    };
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

    #[test]
    fn parses_commit7_commands() {
        let refs_cli = Cli::try_parse_from([
            "sce_cli",
            "refs",
            "add",
            "--project-root",
            "/tmp/project",
            "--file",
            "/tmp/ref.wav",
            "--name",
            "Alpha",
            "--tags",
            "warm,bright",
            "--copy-file",
        ])
        .expect("parse refs add");

        match refs_cli.command {
            Commands::Refs { command } => match command {
                RefCommands::Add {
                    project_root,
                    file,
                    name,
                    tags,
                    copy_file,
                } => {
                    assert_eq!(project_root, std::path::PathBuf::from("/tmp/project"));
                    assert_eq!(file, std::path::PathBuf::from("/tmp/ref.wav"));
                    assert_eq!(name.as_deref(), Some("Alpha"));
                    assert_eq!(tags, "warm,bright");
                    assert!(copy_file);
                }
                _ => panic!("expected refs add"),
            },
            _ => panic!("expected refs command"),
        }

        let operators_cli = Cli::try_parse_from([
            "sce_cli",
            "operators",
            "run",
            "mixops_ci",
            "--project-root",
            "/tmp/project",
            "--last-n",
            "10",
        ])
        .expect("parse operators run");

        match operators_cli.command {
            Commands::Operators { command } => match command {
                OperatorCommands::Run {
                    name,
                    project_root,
                    run_id,
                    last_n,
                } => {
                    assert_eq!(name, "mixops_ci");
                    assert_eq!(project_root, std::path::PathBuf::from("/tmp/project"));
                    assert!(run_id.is_none());
                    assert_eq!(last_n, 10);
                }
                _ => panic!("expected operators run"),
            },
            _ => panic!("expected operators command"),
        }
    }

    #[test]
    fn parses_translation_graph_and_chain_commands() {
        let translation_cli = Cli::try_parse_from([
            "sce_cli",
            "translation",
            "run",
            "--project-root",
            "/tmp/project",
            "--run-id",
            "run-1",
        ])
        .expect("parse translation");

        match translation_cli.command {
            Commands::Translation { command } => match command {
                TranslationCommands::Run {
                    project_root,
                    run_id,
                } => {
                    assert_eq!(project_root, std::path::PathBuf::from("/tmp/project"));
                    assert_eq!(run_id, "run-1");
                }
            },
            _ => panic!("expected translation command"),
        }

        let graph_cli = Cli::try_parse_from([
            "sce_cli",
            "graph",
            "export-dot",
            "--project-root",
            "/tmp/project",
            "--out",
            "/tmp/graph.dot",
        ])
        .expect("parse graph export");

        match graph_cli.command {
            Commands::Graph { command } => match command {
                GraphCommands::ExportDot { project_root, out } => {
                    assert_eq!(project_root, std::path::PathBuf::from("/tmp/project"));
                    assert_eq!(out, Some(std::path::PathBuf::from("/tmp/graph.dot")));
                }
                _ => panic!("expected graph export"),
            },
            _ => panic!("expected graph command"),
        }

        let chain_cli = Cli::try_parse_from([
            "sce_cli",
            "chain",
            "show",
            "--project-root",
            "/tmp/project",
            "--run-id",
            "run-1",
        ])
        .expect("parse chain show");

        match chain_cli.command {
            Commands::Chain { command } => match command {
                ChainCommands::Show {
                    project_root,
                    run_id,
                } => {
                    assert_eq!(project_root, std::path::PathBuf::from("/tmp/project"));
                    assert_eq!(run_id, "run-1");
                }
            },
            _ => panic!("expected chain command"),
        }
    }
}
