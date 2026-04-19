use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use munin_memory::{analytics, core, proactivity_cmd, session_brain, strategy_cmd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "munin",
    version,
    about = "Munin - Local memory for agent-driven development",
    long_about = "Munin reads local agent sessions, compiles startup memory, surfaces friction, and keeps noisy shell output out of agent context."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Verbosity level (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Commands {
    /// Startup memory brief for the current project
    Resume {
        /// Inspection scope
        #[arg(long, default_value = "user")]
        scope: String,
        /// Output format
        #[arg(short, long, default_value = "prompt")]
        format: String,
    },

    /// Startup brain for the current session and project
    Brain {
        /// Output format
        #[arg(short, long, default_value = "prompt")]
        format: String,
    },

    /// Show the next strategy-backed nudge
    Nudge {
        /// Strategy scope (defaults to configured default scope)
        #[arg(long)]
        scope: Option<String>,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: strategy_cmd::StrategyFormat,
    },

    /// Show replay proof for the promoted Memory OS read path
    Prove {
        /// Accepted for product-copy compatibility; proof currently reports Memory OS promotion state
        #[arg(long = "last-resume", default_value_t = false)]
        last_resume: bool,
        /// Output format
        #[arg(short, long, default_value = "text")]
        format: String,
    },

    /// Show repeated correction and friction patterns
    Friction {
        /// Filter evidence to an agent label when available
        #[arg(long)]
        agent: Option<String>,
        /// Limit to a recent window label such as 30d when available
        #[arg(long)]
        last: Option<String>,
        /// Inspection scope
        #[arg(long, default_value = "user")]
        scope: String,
        /// Output format
        #[arg(short, long, default_value = "text")]
        format: String,
    },

    /// Record an observe-only rule candidate
    Promote {
        /// Rule text to record
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        rule: Vec<String>,
    },

    /// Read compiled memory for a query
    Recall {
        /// Topic to answer from compiled Memory OS evidence
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        query: Vec<String>,
        /// Output format
        #[arg(short, long, default_value = "text")]
        format: String,
    },

    /// Install Munin skills/plugin assets for Claude and Codex
    Install {
        /// Install Claude skills into ~/.claude/skills
        #[arg(long, default_value_t = false)]
        claude: bool,
        /// Install Codex skills into ~/.codex/skills and plugin assets into ~/.codex/plugins
        #[arg(long, default_value_t = false)]
        codex: bool,
        /// Print planned writes without changing files
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Replace existing Munin skill/plugin files
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Keep old Munin/context-wrapper skill names instead of archiving them
        #[arg(long, default_value_t = false)]
        keep_legacy: bool,
        /// Validate generated skills and resolver targets without writing files
        #[arg(long, default_value_t = false)]
        check_resolvable: bool,
    },

    /// Resolve a natural-language request to the Munin read surface that should answer it
    Resolve {
        #[arg(short, long, default_value = "text")]
        format: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        query: Vec<String>,
    },

    /// Read or update strategy KPI metrics
    Metrics {
        #[command(subcommand)]
        command: MetricsCommands,
    },

    /// Audit and prune duplicated agent memory guidance files
    Hygiene {
        /// Root directory to scan
        #[arg(long, default_value = ".")]
        root: String,
        /// Apply exact duplicate removals with .munin-bak backups
        #[arg(long, default_value_t = false)]
        write: bool,
        /// Include .codex memory files in the scan
        #[arg(long, default_value_t = false)]
        include_codex: bool,
        /// Output format
        #[arg(short, long, default_value = "text")]
        format: String,
    },

    /// Fast Memory OS health check, with optional release checks
    Doctor {
        #[arg(long, default_value = "user")]
        scope: String,
        #[arg(short, long, default_value = "text")]
        format: String,
        #[arg(long, default_value_t = false)]
        release: bool,
        #[arg(long)]
        repo_root: Option<String>,
        #[arg(long)]
        site_root: Option<String>,
    },

    /// Read and inspect Memory OS state
    #[command(name = "memory-os")]
    MemoryOs {
        #[command(subcommand)]
        command: MemoryOsCommands,
    },

    /// Strategy kernel setup and read surfaces
    Strategy {
        #[command(subcommand)]
        command: StrategyCommands,
    },
    /// Morning strategic proactivity runner, queue, and scheduling surface
    Proactivity {
        #[command(subcommand)]
        command: ProactivityCommands,
    },
}

#[derive(Subcommand)]
enum MemoryOsCommands {
    /// Ingest local agent sessions into Memory OS and show timing
    Ingest {
        #[arg(long)]
        force: bool,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Compact startup briefing from compiled Memory OS state
    Brief {
        #[arg(long, default_value = "user")]
        scope: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Summarize compiled Memory OS state
    Overview {
        #[arg(long, default_value = "user")]
        scope: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Show full Memory OS inspection report
    Inspect {
        #[arg(long, default_value = "user")]
        scope: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Diagnose Memory OS pipeline health
    Doctor {
        #[arg(long, default_value = "user")]
        scope: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Summarize operating style and preferences
    Profile {
        #[arg(long, default_value = "user")]
        scope: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Derive memory-to-action defaults and guardrails
    #[command(name = "action-policy")]
    ActionPolicy {
        #[arg(long, default_value = "user")]
        scope: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Summarize friction and behavior-change signals
    Friction {
        #[arg(long, default_value = "user")]
        scope: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Show current project snapshot
    Snapshot {
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Rebuild narrow Memory OS kernel from journal events
    Kernel {
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Derive observe-only action-memory candidates
    Actions {
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Inspect trust observations
    Trust {
        #[arg(long, default_value = "user")]
        scope: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Show whether replay proof promotes Memory OS cutover
    Promotion {
        #[arg(short, long, default_value = "text")]
        format: String,
    },
}

#[derive(Subcommand)]
enum MetricsCommands {
    /// Record the current value for a configured KPI metric
    Set {
        metric_key: String,
        value: f64,
        #[arg(long, default_value = "default")]
        scope: String,
        #[arg(long)]
        unit: Option<String>,
        #[arg(long)]
        at: Option<String>,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Read recorded KPI metrics
    Get {
        metric_key: Option<String>,
        #[arg(long, default_value = "default")]
        scope: String,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
    /// Import a metrics snapshot into the configured metrics path
    Sync {
        #[arg(long, default_value = "default")]
        scope: String,
        #[arg(long)]
        from: String,
        #[arg(short, long, default_value = "text")]
        format: String,
    },
}

#[derive(Subcommand)]
enum StrategyCommands {
    /// Configure or import a strategy kernel source
    Setup {
        #[arg(long, default_value = "default")]
        scope: String,
        #[arg(long = "import")]
        import_path: Option<std::path::PathBuf>,
        #[arg(long, default_value_t = false)]
        bootstrap_claude: bool,
        #[arg(long, default_value_t = false)]
        template: bool,
        #[arg(short, long, value_enum, default_value = "text")]
        format: strategy_cmd::StrategyFormat,
    },
    /// Inspect the imported strategy kernel and source registry
    Inspect {
        #[arg(long, default_value = "default")]
        scope: String,
        #[arg(short, long, value_enum, default_value = "text")]
        format: strategy_cmd::StrategyFormat,
    },
    /// Show deterministic strategy status
    Status {
        #[arg(long, default_value = "default")]
        scope: String,
        #[arg(short, long, value_enum, default_value = "text")]
        format: strategy_cmd::StrategyFormat,
    },
    /// Synthesize bounded strategic nudges
    Recommend {
        #[arg(long, default_value = "default")]
        scope: String,
        #[arg(short, long, value_enum, default_value = "text")]
        format: strategy_cmd::StrategyFormat,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ProactivityStatusArg {
    Complete,
    Failed,
    Deferred,
    Suppressed,
}

impl From<ProactivityStatusArg> for core::proactivity::ProactivityTerminalStatus {
    fn from(value: ProactivityStatusArg) -> Self {
        match value {
            ProactivityStatusArg::Complete => {
                core::proactivity::ProactivityTerminalStatus::Complete
            }
            ProactivityStatusArg::Failed => core::proactivity::ProactivityTerminalStatus::Failed,
            ProactivityStatusArg::Deferred => {
                core::proactivity::ProactivityTerminalStatus::Deferred
            }
            ProactivityStatusArg::Suppressed => {
                core::proactivity::ProactivityTerminalStatus::Suppressed
            }
        }
    }
}

#[derive(Subcommand)]
enum ProactivityCommands {
    /// Run the morning proactivity evaluation and optionally spawn a session
    Run {
        /// Optional scope override
        #[arg(long)]
        scope: Option<String>,
        /// Optional provider override
        #[arg(long, value_enum)]
        provider: Option<core::config::ProactivityProvider>,
        /// Compute artifacts but do not launch a visible session
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Immediately launch the queued intervention after writing approval artifacts
        #[arg(long, default_value_t = false)]
        auto_spawn: bool,
        /// Skip the actual spawn but still write queue/brief/decision artifacts
        #[arg(long, default_value_t = false)]
        no_spawn: bool,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: proactivity_cmd::ProactivityFormat,
    },
    /// Sweep stale queue claims/results and finalize daemon state
    Sweep {
        /// Optional scope override
        #[arg(long)]
        scope: Option<String>,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: proactivity_cmd::ProactivityFormat,
    },
    /// Show current proactivity status, schedules, and today's artifact state
    Status {
        /// Optional scope override
        #[arg(long)]
        scope: Option<String>,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: proactivity_cmd::ProactivityFormat,
    },
    /// Install the 8am morning task and maintenance sweep task
    #[command(name = "schedule-install")]
    ScheduleInstall {
        /// Optional scope override
        #[arg(long)]
        scope: Option<String>,
        /// Optional provider override
        #[arg(long, value_enum)]
        provider: Option<core::config::ProactivityProvider>,
        /// Optional project path override
        #[arg(long)]
        project_path: Option<PathBuf>,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: proactivity_cmd::ProactivityFormat,
    },
    /// Remove installed proactivity scheduled tasks for the scope
    #[command(name = "schedule-remove")]
    ScheduleRemove {
        /// Optional scope override
        #[arg(long)]
        scope: Option<String>,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: proactivity_cmd::ProactivityFormat,
    },
    /// Claim a queued morning proactivity job by atomic rename
    Claim {
        /// Job id to claim
        #[arg(long)]
        job_id: String,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: proactivity_cmd::ProactivityFormat,
    },
    /// Approve a queued morning proactivity job and optionally launch it
    Approve {
        /// Job id to approve
        #[arg(long)]
        job_id: String,
        /// Claim the job without launching a visible session
        #[arg(long, default_value_t = false)]
        no_spawn: bool,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: proactivity_cmd::ProactivityFormat,
    },
    /// Reject a queued morning proactivity job with a terminal result
    Reject {
        /// Job id to reject
        #[arg(long)]
        job_id: String,
        /// Summary line for the rejection
        #[arg(long)]
        summary: String,
        /// Optional rejection notes (repeatable)
        #[arg(long = "note")]
        notes: Vec<String>,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: proactivity_cmd::ProactivityFormat,
    },
    /// Write a terminal result for a claimed morning proactivity job
    Complete {
        /// Job id to complete
        #[arg(long)]
        job_id: String,
        /// Final status to write
        #[arg(long, value_enum)]
        status: ProactivityStatusArg,
        /// Summary line for the result
        #[arg(long)]
        summary: String,
        /// Optional error detail
        #[arg(long)]
        error: Option<String>,
        /// Optional notes (repeatable)
        #[arg(long = "note")]
        notes: Vec<String>,
        /// Output format
        #[arg(short, long, value_enum, default_value = "text")]
        format: proactivity_cmd::ProactivityFormat,
    },
}

fn main() {
    let code = match run_cli() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("munin: {error:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run_cli() -> Result<i32> {
    let cli = Cli::parse();
    let code = match cli.command {
        Commands::Resume { scope, format } => {
            analytics::memory_os_cmd::run_brief(&scope, None, &format, false, cli.verbose)?;
            0
        }
        Commands::Brain { format } => {
            session_brain::run_inspect_current(&format, cli.verbose)?;
            0
        }
        Commands::Nudge { scope, format } => {
            strategy_cmd::run_recommend(strategy_cmd::StrategyReadRequest {
                scope: scope.unwrap_or_else(configured_strategy_scope_or_default),
                format,
            })?;
            0
        }
        Commands::Prove {
            last_resume: _,
            format,
        } => {
            analytics::memory_os_cmd::run_promotion(&format, cli.verbose)?;
            0
        }
        Commands::Friction {
            agent,
            last,
            scope,
            format,
        } => {
            analytics::memory_os_cmd::run_friction_filtered(
                &scope,
                None,
                &format,
                agent.as_deref(),
                last.as_deref(),
            )?;
            0
        }
        Commands::Promote { rule } => {
            run_munin_promote(&rule)?;
            0
        }
        Commands::Recall { query, format } => {
            analytics::memory_os_cmd::run_recall(
                "user",
                None,
                query.join(" ").as_str(),
                &format,
                cli.verbose,
            )?;
            0
        }
        Commands::Install {
            claude,
            codex,
            dry_run,
            force,
            keep_legacy,
            check_resolvable,
        } => {
            run_install(InstallOptions {
                claude,
                codex,
                dry_run,
                force,
                keep_legacy,
                check_resolvable,
            })?;
            0
        }
        Commands::Resolve { format, query } => {
            let source_status = session_brain::current_source_status();
            let report = core::resolver::resolve_with_source_status(
                query.join(" ").as_str(),
                source_status.as_deref(),
            );
            render_resolve_report(&report, &format)?;
            0
        }
        Commands::Metrics { command } => {
            run_metrics(command)?;
            0
        }
        Commands::Hygiene {
            root,
            write,
            include_codex,
            format,
        } => {
            let report = core::memory_hygiene::run(&core::memory_hygiene::MemoryHygieneOptions {
                root: PathBuf::from(root),
                write,
                include_codex,
            })?;
            render_hygiene_report(&report, &format)?;
            0
        }
        Commands::Doctor {
            scope,
            format,
            release,
            repo_root,
            site_root,
        } => {
            analytics::memory_os_cmd::run_doctor(&scope, None, &format, cli.verbose)?;
            if release {
                run_release_doctor_checks(repo_root.as_deref(), site_root.as_deref())?;
            }
            0
        }
        Commands::MemoryOs { command } => run_memory_os(command, cli.verbose)?,
        Commands::Strategy { command } => run_strategy(command)?,
        Commands::Proactivity { command } => run_proactivity(command)?,
    };
    Ok(code)
}

fn run_metrics(command: MetricsCommands) -> Result<()> {
    match command {
        MetricsCommands::Set {
            metric_key,
            value,
            scope,
            unit,
            at,
            format,
        } => {
            let report = core::strategy::metrics_set(core::strategy::StrategyMetricSetOptions {
                scope,
                metric_key,
                value,
                unit,
                updated_at: at,
            })?;
            render_metrics_report(&report, &format)
        }
        MetricsCommands::Get {
            metric_key,
            scope,
            format,
        } => {
            let report = core::strategy::metrics_get(core::strategy::StrategyMetricGetOptions {
                scope,
                metric_key,
            })?;
            render_metrics_report(&report, &format)
        }
        MetricsCommands::Sync {
            scope,
            from,
            format,
        } => {
            let report = core::strategy::metrics_sync(core::strategy::StrategyMetricSyncOptions {
                scope,
                from_path: PathBuf::from(from),
            })?;
            render_metrics_report(&report, &format)
        }
    }
}

fn render_json_or_text<T: Serialize>(
    report: &T,
    format: &str,
    text: impl FnOnce() -> String,
) -> Result<()> {
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(report)?),
        "text" => println!("{}", text()),
        other => anyhow::bail!("unsupported format `{other}`; expected text or json"),
    }
    Ok(())
}

fn render_resolve_report(report: &core::resolver::ResolveReport, format: &str) -> Result<()> {
    render_json_or_text(report, format, || {
        format!(
            "Munin Resolve\n-------------\nRoute: {}\nCommand: {}\nWhy: {}",
            report.route, report.command, report.reason
        )
    })
}

fn render_metrics_report(
    report: &core::strategy::StrategyMetricsReport,
    format: &str,
) -> Result<()> {
    render_json_or_text(report, format, || {
        let mut lines = vec![
            "Munin Metrics".to_string(),
            "-------------".to_string(),
            format!("Scope: {}", report.scope_id),
            format!("Path: {}", report.metrics_path),
        ];
        if report.kpis.is_empty() {
            lines.push("No metric values recorded.".to_string());
        } else {
            for (key, record) in &report.kpis {
                let value = record
                    .current
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let unit = record.unit.as_deref().unwrap_or("unitless");
                let updated = record.updated_at.as_deref().unwrap_or("unknown time");
                lines.push(format!("- {key}: {value} {unit} ({updated})"));
            }
        }
        for warning in &report.warnings {
            lines.push(format!("warning: {warning}"));
        }
        lines.join("\n")
    })
}

fn render_hygiene_report(
    report: &core::memory_hygiene::MemoryHygieneReport,
    format: &str,
) -> Result<()> {
    render_json_or_text(report, format, || {
        let mut lines = vec![
            "Munin Memory Hygiene".to_string(),
            "--------------------".to_string(),
            format!("Root: {}", report.root),
            format!("Files scanned: {}", report.files_scanned.len()),
            format!("Duplicate groups: {}", report.duplicate_groups.len()),
            format!("Planned removals: {}", report.planned_removals.len()),
        ];
        if report.write_applied {
            lines.push(format!("Backups written: {}", report.backups.len()));
        }
        if !report.skipped_dirs.is_empty() {
            lines.push(format!(
                "Skipped directories: {} (worktree/runtime/cache exclusions)",
                report.skipped_dirs.len()
            ));
        }
        if !report.files_scanned.is_empty() {
            lines.push("Memory files:".to_string());
            for file in &report.files_scanned {
                lines.push(format!(
                    "- [{}] {} ({} guidance units)",
                    file.store_kind, file.path, file.guidance_units
                ));
            }
        }
        if !report.duplicate_groups.is_empty() {
            lines.push("Duplicates:".to_string());
            for group in report.duplicate_groups.iter().take(8) {
                let mode = if group.auto_prunable {
                    "auto-prunable"
                } else {
                    "report-only"
                };
                lines.push(format!("- {}: {}", mode, group.reason));
                for occurrence in group.occurrences.iter().take(4) {
                    lines.push(format!(
                        "  - {}:{} [{}] {}",
                        occurrence.path,
                        occurrence.line_number,
                        occurrence.store_kind,
                        occurrence.text
                    ));
                }
            }
        }
        for warning in &report.warnings {
            lines.push(format!("warning: {warning}"));
        }
        lines.join("\n")
    })
}

fn run_release_doctor_checks(repo_root: Option<&str>, site_root: Option<&str>) -> Result<()> {
    let repo_root = repo_root.unwrap_or(".");
    let cargo_toml = Path::new(repo_root).join("Cargo.toml");
    let cargo = fs::read_to_string(&cargo_toml)
        .with_context(|| format!("failed to read {}", cargo_toml.display()))?;
    if !cargo
        .lines()
        .any(|line| line.trim() == r#"license = "Apache-2.0""#)
    {
        anyhow::bail!("release doctor failed: Cargo.toml must declare Apache-2.0");
    }
    let brain = session_brain::build_current_session_brain()
        .context("release doctor failed: could not inspect Session Brain")?;
    if !session_brain_source_is_release_safe(&brain.meta.source_status) {
        anyhow::bail!("release doctor failed: Session Brain is reading stale fallback context");
    }
    let recall_source = fs::read_to_string(Path::new(repo_root).join("src/bin/munin.rs"))
        .context("release doctor failed: could not inspect CLI source")?;
    let stale_recall_warning = ["munin recall", ": query search is not active yet"].concat();
    if recall_source.contains(stale_recall_warning.as_str()) {
        anyhow::bail!("release doctor failed: recall still contains overview fallback wiring");
    }
    let site_root = site_root
        .map(PathBuf::from)
        .or_else(|| std::env::var("MUNIN_SITE_ROOT").ok().map(PathBuf::from));
    if let Some(path) = site_root.as_deref() {
        if !path.exists() {
            anyhow::bail!(
                "release doctor failed: site root {} does not exist",
                path.display()
            );
        }
        assert_public_docs_parity(Path::new(repo_root), Some(path))?;
    } else {
        assert_public_docs_parity(Path::new(repo_root), None)?;
    }
    println!("release checks: package guard, freshness, recall, and docs parity checks passed");
    Ok(())
}

fn assert_public_docs_parity(repo_root: &Path, site_root: Option<&Path>) -> Result<()> {
    let banned = [
        "munin init",
        "munin gain",
        "munin pack",
        "munin vitest",
        "munin cargo test",
        "munin git diff",
        "munin replay-eval",
    ];
    let mut docs = vec![repo_root.join("README.md")];
    if let Some(site_root) = site_root {
        for entry in fs::read_dir(site_root)
            .with_context(|| format!("failed to read site root {}", site_root.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("html") {
                docs.push(path);
            }
        }
    }
    for path in docs {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let lowered = content.to_lowercase();
        for banned in banned {
            if lowered.contains(banned) {
                anyhow::bail!(
                    "release doctor failed: {} contains unsupported `{}`",
                    path.display(),
                    banned
                );
            }
        }
    }
    Ok(())
}

fn session_brain_source_is_release_safe(source_status: &str) -> bool {
    !matches!(source_status, "stale" | "stale-fallback")
}

fn run_memory_os(command: MemoryOsCommands, verbose: u8) -> Result<i32> {
    match command {
        MemoryOsCommands::Ingest { force, format } => run_memory_os_ingest(force, &format)?,
        MemoryOsCommands::Brief {
            scope,
            project,
            format,
        } => analytics::memory_os_cmd::run_brief(
            &scope,
            project.as_deref(),
            &format,
            false,
            verbose,
        )?,
        MemoryOsCommands::Overview {
            scope,
            project,
            format,
        } => analytics::memory_os_cmd::run_overview(&scope, project.as_deref(), &format, verbose)?,
        MemoryOsCommands::Inspect {
            scope,
            project,
            format,
        } => analytics::memory_os_cmd::run_inspect(&scope, project.as_deref(), &format, verbose)?,
        MemoryOsCommands::Doctor {
            scope,
            project,
            format,
        } => analytics::memory_os_cmd::run_doctor(&scope, project.as_deref(), &format, verbose)?,
        MemoryOsCommands::Profile {
            scope,
            project,
            format,
        } => analytics::memory_os_cmd::run_profile(&scope, project.as_deref(), &format, verbose)?,
        MemoryOsCommands::ActionPolicy {
            scope,
            project,
            format,
        } => analytics::memory_os_cmd::run_action_policy(
            &scope,
            project.as_deref(),
            &format,
            verbose,
        )?,
        MemoryOsCommands::Friction {
            scope,
            project,
            format,
        } => analytics::memory_os_cmd::run_friction(&scope, project.as_deref(), &format, verbose)?,
        MemoryOsCommands::Snapshot { project, format } => {
            analytics::memory_os_cmd::run_snapshot(project.as_deref(), &format, verbose)?
        }
        MemoryOsCommands::Kernel { project, format } => {
            analytics::memory_os_cmd::run_kernel(project.as_deref(), &format, verbose)?
        }
        MemoryOsCommands::Actions { project, format } => {
            analytics::memory_os_cmd::run_actions(project.as_deref(), &format, verbose)?
        }
        MemoryOsCommands::Trust {
            scope,
            project,
            format,
        } => analytics::memory_os_cmd::run_trust(&scope, project.as_deref(), &format, verbose)?,
        MemoryOsCommands::Promotion { format } => {
            analytics::memory_os_cmd::run_promotion(&format, verbose)?
        }
    }
    Ok(0)
}

fn run_memory_os_ingest(force: bool, format: &str) -> Result<()> {
    if !matches!(format, "text" | "json") {
        anyhow::bail!("unsupported format `{format}`; expected text or json");
    }
    let started = std::time::Instant::now();
    let report = analytics::session_backfill::ensure_memory_os_session_backfill_with_force(force)?;
    let elapsed = started.elapsed();
    let elapsed_ms = elapsed.as_millis();
    match format {
        "json" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": if report.is_some() { "imported" } else { "current" },
                    "force": force,
                    "elapsed_ms": elapsed_ms,
                    "sessions_processed": report.as_ref().map(|report| report.sessions_processed).unwrap_or(0),
                    "shells_ingested": report.as_ref().map(|report| report.shells_ingested).unwrap_or(0),
                    "corrections_ingested": report.as_ref().map(|report| report.corrections_ingested).unwrap_or(0),
                    "completed_at": report.as_ref().map(|report| report.completed_at.as_str()),
                }))?
            );
        }
        "text" => {
            println!("Memory OS Ingest");
            println!("----------------");
            println!("Elapsed: {}", format_elapsed(elapsed));
            println!(
                "Mode: {}",
                if force {
                    "forced replay"
                } else {
                    "incremental"
                }
            );
            if let Some(report) = report {
                println!("Status: imported");
                println!("Sessions processed: {}", report.sessions_processed);
                println!("Shell executions ingested: {}", report.shells_ingested);
                println!("Corrections ingested: {}", report.corrections_ingested);
                println!("Completed: {}", report.completed_at);
            } else {
                println!("Status: current");
                println!("No new sessions needed ingestion.");
            }
        }
        _ => unreachable!("format is validated before rendering"),
    }
    Ok(())
}

fn format_elapsed(elapsed: std::time::Duration) -> String {
    if elapsed.as_secs() > 0 {
        format!("{:.2}s", elapsed.as_secs_f64())
    } else {
        format!("{}ms", elapsed.as_millis())
    }
}

fn run_strategy(command: StrategyCommands) -> Result<i32> {
    match command {
        StrategyCommands::Setup {
            scope,
            import_path,
            bootstrap_claude,
            template,
            format,
        } => strategy_cmd::run_setup(strategy_cmd::StrategySetupRequest {
            scope,
            import_path,
            bootstrap_claude,
            template,
            format,
        })?,
        StrategyCommands::Inspect { scope, format } => {
            strategy_cmd::run_inspect(strategy_cmd::StrategyReadRequest { scope, format })?
        }
        StrategyCommands::Status { scope, format } => {
            strategy_cmd::run_status(strategy_cmd::StrategyReadRequest { scope, format })?
        }
        StrategyCommands::Recommend { scope, format } => {
            strategy_cmd::run_recommend(strategy_cmd::StrategyReadRequest { scope, format })?
        }
    }
    Ok(0)
}

fn run_proactivity(command: ProactivityCommands) -> Result<i32> {
    match command {
        ProactivityCommands::Run {
            scope,
            provider,
            dry_run,
            auto_spawn,
            no_spawn,
            format,
        } => {
            proactivity_cmd::run(proactivity_cmd::ProactivityRunRequest {
                scope,
                provider,
                dry_run,
                auto_spawn,
                no_spawn,
                format,
            })?;
        }
        ProactivityCommands::Sweep { scope, format } => {
            proactivity_cmd::sweep(proactivity_cmd::ProactivityScopeRequest { scope, format })?;
        }
        ProactivityCommands::Status { scope, format } => {
            proactivity_cmd::status(proactivity_cmd::ProactivityScopeRequest { scope, format })?;
        }
        ProactivityCommands::ScheduleInstall {
            scope,
            provider,
            project_path,
            format,
        } => {
            proactivity_cmd::schedule_install(
                proactivity_cmd::ProactivityScheduleInstallRequest {
                    scope,
                    provider,
                    project_path,
                    format,
                },
            )?;
        }
        ProactivityCommands::ScheduleRemove { scope, format } => {
            proactivity_cmd::schedule_remove(proactivity_cmd::ProactivityScopeRequest {
                scope,
                format,
            })?;
        }
        ProactivityCommands::Claim { job_id, format } => {
            proactivity_cmd::claim(proactivity_cmd::ProactivityClaimRequest { job_id, format })?;
        }
        ProactivityCommands::Approve {
            job_id,
            no_spawn,
            format,
        } => {
            proactivity_cmd::approve(proactivity_cmd::ProactivityApproveRequest {
                job_id,
                no_spawn,
                format,
            })?;
        }
        ProactivityCommands::Reject {
            job_id,
            summary,
            notes,
            format,
        } => {
            proactivity_cmd::complete(proactivity_cmd::ProactivityCompleteRequest {
                job_id,
                status: core::proactivity::ProactivityTerminalStatus::Suppressed,
                summary,
                error: None,
                notes,
                format,
            })?;
        }
        ProactivityCommands::Complete {
            job_id,
            status,
            summary,
            error,
            notes,
            format,
        } => {
            proactivity_cmd::complete(proactivity_cmd::ProactivityCompleteRequest {
                job_id,
                status: status.into(),
                summary,
                error,
                notes,
                format,
            })?;
        }
    }
    Ok(0)
}

fn configured_strategy_scope_or_default() -> String {
    core::config::Config::load()
        .ok()
        .and_then(|config| config.strategy.configured_scope_name(None))
        .unwrap_or_else(|| "default".to_string())
}

fn run_munin_promote(rule: &[String]) -> Result<()> {
    let rule_text = rule.join(" ").trim().to_string();
    if rule_text.is_empty() {
        anyhow::bail!(
            "munin promote needs rule text, for example: munin promote \"use bun, not npm\""
        );
    }
    let key = format!("munin-promoted-rule:{}", short_sha256(&rule_text));
    analytics::claims_cmd::set_user_decision(&key, &rule_text)?;
    println!(
        "Recorded observe-only Munin rule candidate. Enforcement is not active yet; `munin promote` only records the candidate today."
    );
    Ok(())
}

fn short_sha256(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    hash[..12].to_string()
}

struct InstallOptions {
    claude: bool,
    codex: bool,
    dry_run: bool,
    force: bool,
    keep_legacy: bool,
    check_resolvable: bool,
}

#[derive(Debug, Clone, Copy)]
struct InstallQuickSkill {
    name: &'static str,
    description: &'static str,
    when_to_use: &'static str,
    primary_command: &'static str,
    how_to_read_output: &'static str,
    done: &'static str,
}

const INSTALL_QUICK_SKILLS: &[InstallQuickSkill] = &[
    InstallQuickSkill {
        name: "munin-install-check",
        description: "Validate Munin's generated skills and resolver targets before installing.",
        when_to_use: "Use when demonstrating or verifying that a fresh Munin binary can generate parseable Codex and Claude skills before writing files.",
        primary_command: "munin install --check-resolvable",
        how_to_read_output: "Success prints the number of resolver, skill, and fixture checks that passed. Any failure means the installer contract is broken and should be fixed before recording or shipping.",
        done: "The install contract check passes, and the output names resolver, skill, and fixture checks.",
    },
    InstallQuickSkill {
        name: "munin-install-preview",
        description: "Preview the Codex skill and plugin files Munin will install.",
        when_to_use: "Use when you want a recording-friendly dry run that shows the planned Codex skill/plugin writes without changing the target profile.",
        primary_command: "munin install --codex --dry-run",
        how_to_read_output: "The output should list `would write` paths, `Codex skill` paths, and `Codex plugin skill` paths, then finish with the dry-run write count.",
        done: "The dry run shows the planned Codex skills/plugin files and reports planned writes without modifying files.",
    },
    InstallQuickSkill {
        name: "munin-install-codex",
        description: "Install or refresh Munin's Codex skills and plugin assets.",
        when_to_use: "Use after previewing the install when you are ready to place Munin's Codex skills and plugin files into the active user profile.",
        primary_command: "munin install --codex --force",
        how_to_read_output: "The output should list written Codex skill/plugin paths and end with the number of files written or refreshed.",
        done: "The Codex skill/plugin files are written, and a new Codex session can invoke the installed Munin skills by name.",
    },
    InstallQuickSkill {
        name: "munin-install-claude-preview",
        description: "Preview the Claude skills and slash commands Munin will install.",
        when_to_use: "Use when you want a recording-friendly dry run that shows the planned Claude skill and slash-command writes without changing the target profile.",
        primary_command: "munin install --claude --dry-run",
        how_to_read_output: "The output should list `would write` paths, `Claude skill` paths, and `Claude command` paths, then finish with the dry-run write count.",
        done: "The dry run shows the planned Claude skills and slash commands without modifying files.",
    },
    InstallQuickSkill {
        name: "munin-install-claude",
        description: "Install or refresh Munin's Claude skills and slash commands.",
        when_to_use: "Use after previewing the Claude install when you are ready to place Munin's Claude skills and slash commands into the active user profile.",
        primary_command: "munin install --claude --force",
        how_to_read_output: "The output should list written Claude skill and command paths and end with the number of files written or refreshed.",
        done: "The Claude skill and slash-command files are written, and a new Claude session can invoke Munin via `/munin-*` commands.",
    },
    InstallQuickSkill {
        name: "munin-memory-os-ingest",
        description: "Ingest local agent sessions into Memory OS and show timing.",
        when_to_use: "Use in demos or fresh-install verification when you want to show Munin importing session data into the local Memory OS store.",
        primary_command: "munin memory-os ingest --force --format text",
        how_to_read_output: "The output shows elapsed time plus sessions, shell executions, and corrections imported. The recording command uses `--force` so repeated takes still show timing and corpus counts.",
        done: "The run reports ingestion timing and either imported session counts or a clear already-current result.",
    },
    InstallQuickSkill {
        name: "munin-proactive",
        description: "Run Munin's morning strategic proactivity evaluation on demand.",
        when_to_use: "Use when the user asks to invoke proactivity manually, kick off a morning proactivity cycle now, or see the latest strategic next-move recommendations without waiting for the scheduled 8am task.",
        primary_command: "munin proactivity run --no-spawn --format text",
        how_to_read_output: "The output is the recommendation report: strategic nudges with confidence, continuity tasks, any queued job id, and warnings. Because `--no-spawn` is set, no follow-up session is launched; the user can approve the queued brief later with `munin proactivity approve <job-id>`.",
        done: "The run reports one or more nudges (or a clean no-action state), names any queued job id, and confirms no session was spawned.",
    },
];

#[derive(Debug, Clone, Copy)]
struct InstallProseSkill {
    name: &'static str,
    description: &'static str,
    body: &'static str,
    references: &'static [(&'static str, &'static str)],
}

const INSTALL_PROSE_SKILLS: &[InstallProseSkill] = &[InstallProseSkill {
    name: "munin-strategy",
    description: "One-Page Strategic Plan (OPSP) coaching shipped with Munin. Use for strategy creation, bootstrap, update, or Eisenhower triage.",
    body: include_str!("../assets/skills/munin-strategy/SKILL.md"),
    references: &[(
        "opsp-template.md",
        include_str!("../assets/skills/munin-strategy/references/opsp-template.md"),
    )],
}];

const LEGACY_SKILL_NAMES: &[&str] = &[
    "munin-discover",
    "munin-gain",
    "munin-learn",
    "munin-memory-os-brief",
    "munin-memory-os-friction",
    "munin-memory-os-inspect",
    "munin-memory-os-overview",
    "munin-memory-os-profile",
    "munin-memory-os-promotion",
    "munin-rewrite",
];

fn run_install(options: InstallOptions) -> Result<()> {
    if options.check_resolvable {
        return run_check_resolvable();
    }
    let install_claude = options.claude || !options.codex;
    let install_codex = options.codex || !options.claude;
    let home = install_home_dir()?;
    let mut writes = 0usize;

    if install_claude {
        let root = home.join(".claude").join("skills");
        writes += install_skills_at(
            &root,
            options.force,
            options.dry_run,
            options.keep_legacy,
            "Claude",
        )?;
        let command_root = home.join(".claude").join("commands");
        writes += install_claude_commands_at(&command_root, options.force, options.dry_run)?;
    }
    if install_codex {
        let skill_root = home.join(".codex").join("skills");
        writes += install_skills_at(
            &skill_root,
            options.force,
            options.dry_run,
            options.keep_legacy,
            "Codex",
        )?;
        let plugin_root = home.join(".codex").join("plugins").join("munin-memory");
        writes += install_codex_plugin(&plugin_root, options.force, options.dry_run)?;
    }

    if options.dry_run {
        println!("Munin install dry-run complete: {writes} planned writes.");
    } else {
        println!("Munin install complete: {writes} files written or refreshed.");
        emit_strategy_bootstrap_hint();
    }
    Ok(())
}

fn emit_strategy_bootstrap_hint() {
    let has_kernel = match core::strategy::discover_inspect_reports(1) {
        Ok(reports) => !reports.is_empty(),
        Err(_) => false,
    };
    if has_kernel {
        return;
    }
    let scope = core::strategy::default_strategy_scope_hint();
    println!();
    println!("No strategy kernel detected yet. Want a first-cut OPSP plan?");
    println!("  Bootstrap a starter (auto-populated, refine later):");
    println!("    munin strategy setup --scope {scope} --bootstrap-claude");
    println!("  Refine interactively with the OPSP coach:");
    println!("    /munin-strategy        (Claude Code)");
    println!("    skill run munin-strategy   (Codex)");
}

fn install_home_dir() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("MUNIN_INSTALL_HOME") {
        return Ok(PathBuf::from(home));
    }
    dirs::home_dir().context("could not determine home directory")
}

fn run_check_resolvable() -> Result<()> {
    let mut checked = 0usize;
    for skill in INSTALL_QUICK_SKILLS {
        validate_munin_command(skill.primary_command).with_context(|| {
            format!(
                "install quick skill `{}` command is not resolvable",
                skill.name
            )
        })?;
        let rendered = render_install_quick_skill(skill);
        assert_install_quick_skill_contract(skill.name, &rendered, skill)?;
        checked += 2;
    }
    for rule in core::access_layer::intent_rules::INTENT_RULES {
        let command = rule
            .primary_command
            .replace("<query>", "resolver decisions");
        validate_munin_command(command.as_str())
            .with_context(|| format!("skill `{}` command is not resolvable", rule.skill_name))?;
        checked += 1;
        if let Some(fallback) = rule.fallback_command {
            if fallback.command.starts_with("munin ") {
                validate_munin_command(
                    fallback
                        .command
                        .replace("<query>", "resolver decisions")
                        .as_str(),
                )
                .with_context(|| format!("fallback for `{}` is not resolvable", rule.skill_name))?;
                checked += 1;
            } else if fallback.command != "qmd \"<query>\"" {
                anyhow::bail!(
                    "external fallback for `{}` must be an explicit raw archive fallback",
                    rule.skill_name
                );
            }
        }
        let rendered = render_narrow_skill(rule);
        assert_skill_contract(rule.skill_name, &rendered, rule)?;
        if core::access_layer::intent_rules::intent_by_skill_name(rule.skill_name).is_none() {
            anyhow::bail!("intent registry cannot look up skill `{}`", rule.skill_name);
        }
        checked += 1;
    }
    let umbrella = render_umbrella_skill();
    if !umbrella.contains("munin resolve") || !umbrella.contains("narrow skill") {
        anyhow::bail!("umbrella skill is missing resolver flow");
    }
    checked += 1;
    for command in core::resolver::known_resolver_commands() {
        if command.starts_with("munin ") {
            validate_munin_command(command.replace("<query>", "resolver decisions").as_str())
                .with_context(|| format!("resolver target `{command}` is not resolvable"))?;
        }
        checked += 1;
    }
    checked += validate_resolver_trigger_fixtures()?;
    for invalid in [
        "munin nudge --format xml",
        "munin recall --format prompt resolver",
        "munin resolve --format yaml resolver",
    ] {
        if validate_munin_command(invalid).is_ok() {
            anyhow::bail!("invalid command unexpectedly parsed: {invalid}");
        }
        checked += 1;
    }
    println!("install check-resolvable: {checked} resolver, skill, and fixture checks passed");
    Ok(())
}

fn assert_install_quick_skill_contract(
    skill_name: &str,
    rendered: &str,
    skill: &InstallQuickSkill,
) -> Result<()> {
    for section in [
        "## When to use",
        "## Primary command",
        "## How to read output",
        "## Trust",
        "## Done",
    ] {
        if !rendered.contains(section) {
            anyhow::bail!(
                "generated install quick skill `{skill_name}` is missing section `{section}`"
            );
        }
    }
    if !rendered.contains(skill.primary_command) {
        anyhow::bail!("generated install quick skill `{skill_name}` is missing its command");
    }
    Ok(())
}

fn assert_skill_contract(
    skill_name: &str,
    rendered: &str,
    rule: &core::access_layer::intent_rules::IntentRule,
) -> Result<()> {
    for section in [
        "## When to use",
        "## Primary command",
        "## How to read output",
        "## Trust",
        "## Fallback",
        "## What not to do",
        "## Done",
    ] {
        if !rendered.contains(section) {
            anyhow::bail!("generated skill `{skill_name}` is missing section `{section}`");
        }
    }
    for required in rule
        .output_contract
        .iter()
        .chain(rule.trust_rules)
        .chain(rule.fallback_rules)
        .chain(rule.done_criteria)
        .chain(rule.what_not_to_do)
    {
        if !rendered.contains(required) {
            anyhow::bail!("generated skill `{skill_name}` is missing rule `{required}`");
        }
    }
    Ok(())
}

fn validate_munin_command(command: &str) -> Result<()> {
    if command.contains("--format xml")
        || command.contains("--format yaml")
        || command.contains("recall --format prompt")
    {
        anyhow::bail!("unsupported runtime format in `{command}`");
    }
    let mut args = command_line_words(command);
    if args.first().map(|value| value.as_str()) != Some("munin") {
        anyhow::bail!("command must start with munin: {command}");
    }
    args.remove(0);
    let mut argv = vec!["munin".to_string()];
    argv.extend(args);
    Cli::try_parse_from(argv).with_context(|| format!("failed to parse `{command}`"))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ResolverTriggerFixture {
    route: String,
    source_status: Option<String>,
    triggers: Vec<String>,
    negative_triggers: Vec<String>,
}

fn validate_resolver_trigger_fixtures() -> Result<usize> {
    let fixture_dir = Path::new("tests")
        .join("fixtures")
        .join("resolver_triggers");
    if !fixture_dir.exists() {
        anyhow::bail!(
            "resolver trigger fixture directory missing: {}",
            fixture_dir.display()
        );
    }

    let mut checked = 0usize;
    let mut fixture_routes = std::collections::BTreeSet::new();
    for entry in fs::read_dir(&fixture_dir)
        .with_context(|| format!("failed to read {}", fixture_dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read fixture {}", path.display()))?;
        let fixture: ResolverTriggerFixture = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse fixture {}", path.display()))?;
        let rule = core::access_layer::intent_rules::INTENT_RULES
            .iter()
            .find(|rule| rule.route == fixture.route)
            .with_context(|| format!("fixture {} does not match an intent rule", path.display()))?;
        fixture_routes.insert(fixture.route.clone());
        if fixture.triggers.len() < 5 || fixture.negative_triggers.len() < 2 {
            anyhow::bail!(
                "fixture {} needs at least five positive and two negative triggers",
                path.display()
            );
        }
        for query in fixture.triggers {
            let report = core::resolver::resolve_with_source_status(
                query.as_str(),
                fixture.source_status.as_deref(),
            );
            if report.route != fixture.route {
                anyhow::bail!(
                    "fixture {} trigger `{}` routed to `{}` instead of `{}`",
                    path.display(),
                    query,
                    report.route,
                    fixture.route
                );
            }
            if report.command.starts_with("munin ") {
                validate_munin_command(&report.command).with_context(|| {
                    format!("fixture {} command is not resolvable", path.display())
                })?;
            }
            checked += 1;
        }
        for query in fixture.negative_triggers {
            let report = core::resolver::resolve_with_source_status(
                query.as_str(),
                fixture.source_status.as_deref(),
            );
            if report.route == fixture.route {
                anyhow::bail!(
                    "fixture {} negative trigger `{}` unexpectedly routed to `{}`",
                    path.display(),
                    query,
                    fixture.route
                );
            }
            checked += 1;
        }
        checked += rule.trigger_phrases.len().min(1);
    }
    for rule in core::access_layer::intent_rules::INTENT_RULES {
        if !fixture_routes.contains(rule.route) {
            anyhow::bail!("resolver trigger fixture missing route `{}`", rule.route);
        }
    }
    if checked < 50 {
        anyhow::bail!("resolver trigger coverage too low: {checked} assertions");
    }
    Ok(checked)
}

fn command_line_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for ch in command.chars() {
        match ch {
            '"' => in_quote = !in_quote,
            ' ' | '\t' if !in_quote => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn install_skills_at(
    root: &Path,
    force: bool,
    dry_run: bool,
    keep_legacy: bool,
    label: &str,
) -> Result<usize> {
    let mut writes = 0usize;
    if !keep_legacy {
        writes += archive_legacy_skills(root, force, dry_run, label)?;
    }
    let umbrella = core::access_layer::intent_rules::UMBRELLA_SKILL;
    let path = root.join(umbrella.name).join("SKILL.md");
    let content = render_umbrella_skill();
    if write_installer_file(&path, &content, force, dry_run)? {
        writes += 1;
        println!("{label} skill: {}", path.display());
    }
    for rule in core::access_layer::intent_rules::INTENT_RULES {
        let path = root.join(rule.skill_name).join("SKILL.md");
        let content = render_narrow_skill(rule);
        if write_installer_file(&path, &content, force, dry_run)? {
            writes += 1;
            println!("{label} skill: {}", path.display());
        }
    }
    for skill in INSTALL_QUICK_SKILLS {
        let path = root.join(skill.name).join("SKILL.md");
        let content = render_install_quick_skill(skill);
        if write_installer_file(&path, &content, force, dry_run)? {
            writes += 1;
            println!("{label} skill: {}", path.display());
        }
    }
    for skill in INSTALL_PROSE_SKILLS {
        let skill_dir = root.join(skill.name);
        let path = skill_dir.join("SKILL.md");
        if write_installer_file(&path, skill.body, force, dry_run)? {
            writes += 1;
            println!("{label} skill: {}", path.display());
        }
        for (filename, content) in skill.references {
            let ref_path = skill_dir.join("references").join(filename);
            if write_installer_file(&ref_path, content, force, dry_run)? {
                writes += 1;
                println!("{label} skill reference: {}", ref_path.display());
            }
        }
    }
    Ok(writes)
}

fn install_claude_commands_at(root: &Path, force: bool, dry_run: bool) -> Result<usize> {
    let mut writes = 0usize;
    let umbrella = core::access_layer::intent_rules::UMBRELLA_SKILL;
    let path = root.join(format!("{}.md", umbrella.name));
    let content = render_claude_umbrella_command();
    if write_installer_file(&path, &content, force, dry_run)? {
        writes += 1;
        println!("Claude command: {}", path.display());
    }
    for rule in core::access_layer::intent_rules::INTENT_RULES {
        let path = root.join(format!("{}.md", rule.skill_name));
        let content = render_claude_narrow_command(rule);
        if write_installer_file(&path, &content, force, dry_run)? {
            writes += 1;
            println!("Claude command: {}", path.display());
        }
    }
    for skill in INSTALL_QUICK_SKILLS {
        let path = root.join(format!("{}.md", skill.name));
        let content = render_claude_install_quick_command(skill);
        if write_installer_file(&path, &content, force, dry_run)? {
            writes += 1;
            println!("Claude command: {}", path.display());
        }
    }
    for skill in INSTALL_PROSE_SKILLS {
        let path = root.join(format!("{}.md", skill.name));
        let content = render_claude_install_prose_command(skill);
        if write_installer_file(&path, &content, force, dry_run)? {
            writes += 1;
            println!("Claude command: {}", path.display());
        }
    }
    Ok(writes)
}

fn archive_legacy_skills(root: &Path, force: bool, dry_run: bool, label: &str) -> Result<usize> {
    let archive_root = root.join(".munin-legacy");
    let mut archived = 0usize;
    for name in LEGACY_SKILL_NAMES {
        let source = root.join(name);
        if !source.is_dir() {
            continue;
        }
        let destination = archive_root.join(name);
        if destination.exists() {
            if force {
                if dry_run {
                    println!(
                        "would replace archived legacy skill: {}",
                        destination.display()
                    );
                } else {
                    fs::remove_dir_all(&destination).with_context(|| {
                        format!("failed to replace archived {}", destination.display())
                    })?;
                }
            } else {
                println!(
                    "skip legacy archive, destination exists: {}",
                    destination.display()
                );
                continue;
            }
        }
        if dry_run {
            println!(
                "would archive {label} legacy skill: {} -> {}",
                source.display(),
                destination.display()
            );
        } else {
            fs::create_dir_all(&archive_root)
                .with_context(|| format!("failed to create {}", archive_root.display()))?;
            fs::rename(&source, &destination).with_context(|| {
                format!(
                    "failed to archive legacy skill {} -> {}",
                    source.display(),
                    destination.display()
                )
            })?;
            println!(
                "archived {label} legacy skill: {} -> {}",
                source.display(),
                destination.display()
            );
        }
        archived += 1;
    }
    Ok(archived)
}

fn install_codex_plugin(plugin_root: &Path, force: bool, dry_run: bool) -> Result<usize> {
    let mut writes = 0usize;
    let manifest_path = plugin_root.join(".codex-plugin").join("plugin.json");
    if write_installer_file(&manifest_path, CODEX_PLUGIN_JSON, force, dry_run)? {
        writes += 1;
        println!("Codex plugin: {}", manifest_path.display());
    }
    let umbrella = core::access_layer::intent_rules::UMBRELLA_SKILL;
    let path = plugin_root
        .join("skills")
        .join(umbrella.name)
        .join("SKILL.md");
    let content = render_umbrella_skill();
    if write_installer_file(&path, &content, force, dry_run)? {
        writes += 1;
        println!("Codex plugin skill: {}", path.display());
    }
    for rule in core::access_layer::intent_rules::INTENT_RULES {
        let path = plugin_root
            .join("skills")
            .join(rule.skill_name)
            .join("SKILL.md");
        let content = render_narrow_skill(rule);
        if write_installer_file(&path, &content, force, dry_run)? {
            writes += 1;
            println!("Codex plugin skill: {}", path.display());
        }
    }
    for skill in INSTALL_QUICK_SKILLS {
        let path = plugin_root.join("skills").join(skill.name).join("SKILL.md");
        let content = render_install_quick_skill(skill);
        if write_installer_file(&path, &content, force, dry_run)? {
            writes += 1;
            println!("Codex plugin skill: {}", path.display());
        }
    }
    for skill in INSTALL_PROSE_SKILLS {
        let skill_dir = plugin_root.join("skills").join(skill.name);
        let path = skill_dir.join("SKILL.md");
        if write_installer_file(&path, skill.body, force, dry_run)? {
            writes += 1;
            println!("Codex plugin skill: {}", path.display());
        }
        for (filename, content) in skill.references {
            let ref_path = skill_dir.join("references").join(filename);
            if write_installer_file(&ref_path, content, force, dry_run)? {
                writes += 1;
                println!("Codex plugin skill reference: {}", ref_path.display());
            }
        }
    }
    Ok(writes)
}

fn write_installer_file(path: &Path, content: &str, force: bool, dry_run: bool) -> Result<bool> {
    if path.exists() && !force {
        println!("skip existing: {}", path.display());
        return Ok(false);
    }
    if dry_run {
        println!("would write: {}", path.display());
        return Ok(true);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

fn render_umbrella_skill() -> String {
    let umbrella = core::access_layer::intent_rules::UMBRELLA_SKILL;
    let routes = core::access_layer::intent_rules::INTENT_RULES
        .iter()
        .map(|rule| rule.route)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "---\nname: {}\ndescription: {}\n---\n# {}\n\n## When to use\n{}\n\n## Flow\n1. Run `munin resolve --format text \"<user ask>\"`.\n2. Run the returned command.\n3. Follow the matching narrow skill's Trust, Fallback, What not to do, and Done rules.\n\n## Resolver output\nRoutes: {}.\n\nLive-session continuity routes to `brain` only when Session Brain is live. Fallback or stale continuity routes to `resume`.\n\n## Trust\n- Trust the route unless the user clearly asked for a different narrow surface.\n- If route is `recall` and the command returns zero topic matches, do not silently fall back to overview.\n- If route is `brain`, check the freshness label before saying anything is current.\n\n## Fallback\n- If the route looks wrong, ask `munin resolve` with the user's exact words and compare the route to the narrow skill descriptions.\n- If the command fails to parse, run `munin install --check-resolvable` before using installed skills.\n\n## Done\nThe user has a compiled answer from the chosen Munin surface, not a raw transcript dump.\n",
        umbrella.name, umbrella.description, umbrella.name, umbrella.description_expanded, routes
    )
}

fn render_install_quick_skill(skill: &InstallQuickSkill) -> String {
    format!(
        "---\nname: {}\ndescription: {}\n---\n# {}\n\n## When to use\n{}\n\n## Primary command\n\n```powershell\n{}\n```\n\n## How to read output\n{}\n\n## Trust\n- Treat this as an installer demonstration skill, not a memory read surface.\n- Do not change unrelated user data while running it.\n- If this is a fresh-install recording, set `MUNIN_INSTALL_HOME` to the intended demo profile before running it.\n\n## Done\n{}\n",
        skill.name,
        skill.description,
        skill.name,
        skill.when_to_use,
        skill.primary_command,
        skill.how_to_read_output,
        skill.done
    )
}

fn render_claude_umbrella_command() -> String {
    let umbrella = core::access_layer::intent_rules::UMBRELLA_SKILL;
    format!(
        "---\ndescription: {}\ndisable-model-invocation: true\n---\nInvoke the `{}` skill and follow it exactly. If the user provided arguments, pass them through: $ARGUMENTS\n",
        umbrella.description, umbrella.name
    )
}

fn render_claude_narrow_command(rule: &core::access_layer::intent_rules::IntentRule) -> String {
    format!(
        "---\ndescription: {}\ndisable-model-invocation: true\n---\nInvoke the `{}` skill and follow it exactly. If the user provided arguments, use them as the request context: $ARGUMENTS\n",
        rule.description, rule.skill_name
    )
}

fn render_claude_install_quick_command(skill: &InstallQuickSkill) -> String {
    format!(
        "---\ndescription: {}\ndisable-model-invocation: true\n---\nInvoke the `{}` skill and follow it exactly. If this is a recording or fresh-install demo, keep the output concise and preserve the command timing/counts. Arguments: $ARGUMENTS\n",
        skill.description, skill.name
    )
}

fn render_claude_install_prose_command(skill: &InstallProseSkill) -> String {
    format!(
        "---\ndescription: {}\ndisable-model-invocation: true\n---\nInvoke the `{}` skill and follow it exactly. If the user provided arguments, use them as the request context: $ARGUMENTS\n",
        skill.description, skill.name
    )
}

fn render_narrow_skill(rule: &core::access_layer::intent_rules::IntentRule) -> String {
    let fallback_command = rule
        .fallback_command
        .map(|fallback| {
            format!(
                "\nFallback command:\n\n```powershell\n{}\n```",
                fallback.command
            )
        })
        .unwrap_or_default();
    format!(
        "---\nname: {}\ndescription: {}\n---\n# {}\n\n## When to use\n{}\n\n## Primary command\n\n```powershell\n{}\n```\n{}\n\n## How to read output\n{}\n\n## Trust\n{}\n\n## Fallback\n- If output is empty, stale, or generic, do not invent an answer.\n{}\n- If unsure this is the right skill, run `munin resolve \"<ask>\"` and follow its route.\n\n## What not to do\n{}\n\n## Done\nYou're done when the answer:\n{}\n",
        rule.skill_name,
        rule.description,
        rule.skill_name,
        rule.description_expanded,
        rule.primary_command,
        fallback_command,
        render_bullets(rule.output_contract),
        render_bullets(rule.trust_rules),
        render_bullets(rule.fallback_rules),
        render_bullets(rule.what_not_to_do),
        render_bullets(rule.done_criteria)
    )
}

fn render_bullets(items: &[&str]) -> String {
    items
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

const CODEX_PLUGIN_JSON: &str = r#"{
  "name": "munin-memory",
  "version": "0.5.0-beta.2",
  "description": "Munin local memory surfaces for Codex.",
  "interface": {
    "displayName": "Munin Memory",
    "shortDescription": "Local memory, friction, nudges, and proof for agentic coding."
  }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) {
        Cli::try_parse_from(args).unwrap_or_else(|err| panic!("{args:?} failed: {err}"));
    }

    #[test]
    fn public_munin_commands_parse() {
        parse_ok(&["munin", "resume", "--format", "prompt"]);
        parse_ok(&["munin", "brain", "--format", "prompt"]);
        parse_ok(&["munin", "nudge", "--scope", "sitesorted-business"]);
        parse_ok(&["munin", "prove", "--last-resume"]);
        parse_ok(&["munin", "friction", "--agent", "codex", "--last", "30d"]);
        parse_ok(&["munin", "promote", "use bun, not npm"]);
        parse_ok(&["munin", "recall", "refund SLA"]);
        parse_ok(&["munin", "install", "--dry-run"]);
        parse_ok(&["munin", "install", "--claude", "--force"]);
        parse_ok(&["munin", "install", "--keep-legacy"]);
        parse_ok(&["munin", "install", "--codex", "--dry-run"]);
        parse_ok(&["munin", "install", "--check-resolvable"]);
        parse_ok(&["munin", "resolve", "what", "keeps", "going", "wrong"]);
        parse_ok(&["munin", "metrics", "get", "--scope", "sitesorted-business"]);
        parse_ok(&["munin", "hygiene"]);
        parse_ok(&["munin", "hygiene", "--root", ".", "--include-codex"]);
        parse_ok(&[
            "munin",
            "proactivity",
            "run",
            "--scope",
            "sitesorted-business",
            "--provider",
            "claude",
            "--auto-spawn",
            "--dry-run",
        ]);
        parse_ok(&["munin", "proactivity", "sweep"]);
        parse_ok(&["munin", "proactivity", "status"]);
        parse_ok(&[
            "munin",
            "proactivity",
            "schedule-install",
            "--scope",
            "sitesorted-business",
            "--provider",
            "codex",
            "--project-path",
            ".",
        ]);
        parse_ok(&["munin", "proactivity", "schedule-remove"]);
        parse_ok(&[
            "munin",
            "proactivity",
            "claim",
            "--job-id",
            "morning-sitesorted-business-2026-04-19",
        ]);
        parse_ok(&[
            "munin",
            "proactivity",
            "approve",
            "--job-id",
            "morning-sitesorted-business-2026-04-19",
            "--no-spawn",
        ]);
        parse_ok(&[
            "munin",
            "proactivity",
            "reject",
            "--job-id",
            "morning-sitesorted-business-2026-04-19",
            "--summary",
            "not today",
        ]);
        parse_ok(&[
            "munin",
            "proactivity",
            "complete",
            "--job-id",
            "morning-sitesorted-business-2026-04-19",
            "--status",
            "complete",
            "--summary",
            "done",
        ]);
        parse_ok(&[
            "munin",
            "metrics",
            "set",
            "sales.pipeline",
            "12",
            "--scope",
            "sitesorted-business",
            "--unit",
            "leads",
        ]);
        parse_ok(&["munin", "doctor", "--scope", "user"]);
        parse_ok(&["munin", "doctor", "--release", "--repo-root", "."]);
    }

    #[test]
    fn munin_friction_skill_matches_golden_contract() {
        let rule = core::access_layer::intent_rules::intent_by_skill_name("munin-friction")
            .expect("friction rule");
        let rendered = render_narrow_skill(rule);
        assert_eq!(
            rendered.replace("\r\n", "\n").trim_end(),
            include_str!("../../tests/golden/munin_friction_skill.md")
                .replace("\r\n", "\n")
                .trim_end()
        );
    }

    #[test]
    fn install_quick_skills_render_parseable_commands() {
        let mut names = std::collections::BTreeSet::new();
        for skill in INSTALL_QUICK_SKILLS {
            assert!(
                names.insert(skill.name),
                "duplicate install quick skill {}",
                skill.name
            );
            validate_munin_command(skill.primary_command)
                .unwrap_or_else(|err| panic!("{} did not parse: {err}", skill.primary_command));
            let rendered = render_install_quick_skill(skill);
            assert_install_quick_skill_contract(skill.name, &rendered, skill)
                .unwrap_or_else(|err| panic!("{} contract failed: {err}", skill.name));
        }
        assert_eq!(names.len(), 7);
    }

    #[test]
    fn claude_slash_commands_invoke_matching_skills() {
        let umbrella = render_claude_umbrella_command();
        assert!(umbrella.contains("Invoke the `munin` skill"));
        assert!(umbrella.contains("disable-model-invocation: true"));

        let doctor = core::access_layer::intent_rules::intent_by_skill_name("munin-doctor")
            .expect("doctor rule");
        let doctor_command = render_claude_narrow_command(doctor);
        assert!(doctor_command.contains("Invoke the `munin-doctor` skill"));

        let ingest = INSTALL_QUICK_SKILLS
            .iter()
            .find(|skill| skill.name == "munin-memory-os-ingest")
            .expect("ingest quick skill");
        let ingest_command = render_claude_install_quick_command(ingest);
        assert!(ingest_command.contains("Invoke the `munin-memory-os-ingest` skill"));
    }

    #[test]
    fn access_layer_registry_has_unique_routes_and_skill_names() {
        let mut routes = std::collections::BTreeSet::new();
        let mut skills = std::collections::BTreeSet::new();
        for rule in core::access_layer::intent_rules::INTENT_RULES {
            assert!(routes.insert(rule.route), "duplicate route {}", rule.route);
            assert!(
                skills.insert(rule.skill_name),
                "duplicate skill {}",
                rule.skill_name
            );
            assert!(!rule.trigger_phrases.is_empty());
            assert!(!rule.output_contract.is_empty());
            assert!(!rule.trust_rules.is_empty());
            assert!(!rule.fallback_rules.is_empty());
            assert!(!rule.done_criteria.is_empty());
            assert!(!rule.what_not_to_do.is_empty());
        }
        assert_eq!(routes.len(), 8);
    }

    #[test]
    fn memory_os_subcommands_parse() {
        parse_ok(&["munin", "memory-os", "brief"]);
        parse_ok(&["munin", "memory-os", "ingest"]);
        parse_ok(&["munin", "memory-os", "ingest", "--force"]);
        parse_ok(&["munin", "memory-os", "ingest", "--format", "json"]);
        parse_ok(&["munin", "memory-os", "overview"]);
        parse_ok(&["munin", "memory-os", "doctor"]);
        parse_ok(&["munin", "memory-os", "friction"]);
        parse_ok(&["munin", "memory-os", "promotion"]);
    }

    #[test]
    fn release_doctor_rejects_stale_session_brain_statuses() {
        assert!(!session_brain_source_is_release_safe("stale"));
        assert!(!session_brain_source_is_release_safe("stale-fallback"));
        assert!(session_brain_source_is_release_safe("fallback-latest"));
        assert!(session_brain_source_is_release_safe("live"));
    }
}
