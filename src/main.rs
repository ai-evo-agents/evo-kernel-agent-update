mod git;
mod updater;
mod versions;

use async_trait::async_trait;
use evo_agent_sdk::prelude::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use git::commit_file;
use updater::{patch_cargo_toml, patch_workflow_sed};
use versions::{VersionReport, current_dep_version, latest_crate_version, needs_update};

// ─── Crates we track on crates.io ────────────────────────────────────────────

/// Crates whose versions are checked on crates.io and propagated to all repos.
const TRACKED_CRATES: &[&str] = &["evo-common", "evo-agent-sdk"];

// ─── Managed repo table ───────────────────────────────────────────────────────

/// Configuration for a single managed repository.
struct RepoSpec {
    /// GitHub repo slug (without org prefix).
    repo: &'static str,
    /// Local folder name relative to the kernel-agents base dir.
    local: &'static str,
    /// Cargo.toml paths inside the repo that may contain tracked deps.
    cargo_files: &'static [&'static str],
    /// CI workflow files that contain `sed` version substitution patterns.
    /// These are updated whenever `evo-agent-sdk` changes.
    workflow_files: &'static [&'static str],
}

/// All repos managed by this agent.
///
/// To add a new repo, append a `RepoSpec` entry here.  No other changes are
/// required.
const MANAGED_REPOS: &[RepoSpec] = &[
    RepoSpec {
        repo: "evo-king",
        local: "evo-king",
        cargo_files: &["Cargo.toml"],
        workflow_files: &[],
    },
    RepoSpec {
        repo: "evo-agents",
        local: "evo-agents",
        cargo_files: &["evo-agent-sdk/Cargo.toml"],
        workflow_files: &[],
    },
    RepoSpec {
        repo: "evo-kernel-agent-learning",
        local: "evo-kernel-agent-learning",
        cargo_files: &["Cargo.toml"],
        workflow_files: &[".github/workflows/ci.yml", ".github/workflows/release.yml"],
    },
    RepoSpec {
        repo: "evo-kernel-agent-building",
        local: "evo-kernel-agent-building",
        cargo_files: &["Cargo.toml"],
        workflow_files: &[".github/workflows/ci.yml", ".github/workflows/release.yml"],
    },
    RepoSpec {
        repo: "evo-kernel-agent-pre-load",
        local: "evo-kernel-agent-pre-load",
        cargo_files: &["Cargo.toml"],
        workflow_files: &[".github/workflows/ci.yml", ".github/workflows/release.yml"],
    },
    RepoSpec {
        repo: "evo-kernel-agent-evaluation",
        local: "evo-kernel-agent-evaluation",
        cargo_files: &["Cargo.toml"],
        workflow_files: &[".github/workflows/ci.yml", ".github/workflows/release.yml"],
    },
    RepoSpec {
        repo: "evo-kernel-agent-skill-manage",
        local: "evo-kernel-agent-skill-manage",
        cargo_files: &["Cargo.toml"],
        workflow_files: &[".github/workflows/ci.yml", ".github/workflows/release.yml"],
    },
    RepoSpec {
        repo: "evo-kernel-agent-update",
        local: "evo-kernel-agent-update",
        cargo_files: &["Cargo.toml"],
        workflow_files: &[".github/workflows/ci.yml", ".github/workflows/release.yml"],
    },
    RepoSpec {
        repo: "evo-user-agent-template",
        local: "evo-user-agent-template",
        cargo_files: &["Cargo.toml"],
        workflow_files: &[".github/workflows/ci.yml", ".github/workflows/release.yml"],
    },
];

// ─── Internal tracking types ──────────────────────────────────────────────────

/// A single pending file update, discovered in Phase 2.
#[derive(Debug)]
struct PendingUpdate {
    repo: &'static str,
    local_base: PathBuf,
    file_path: &'static str,
    patched_content: String,
    commit_message: String,
}

// ─── UpdateHandler ────────────────────────────────────────────────────────────

/// Handles the `pipeline:next` event for the `update` role.
///
/// Phases:
/// 1. Check crates.io for latest stable versions of tracked crates.
/// 2. Scan every managed repo's Cargo.toml and workflow files for stale deps.
/// 3. Ask the LLM gateway for a brief changelog-risk analysis.
/// 4. Apply all patches and commit (skipped in dry-run mode).
/// 5. Notify king's `/admin/config-sync` endpoint.
/// 6. Return a structured JSON summary.
struct UpdateHandler;

#[async_trait]
impl AgentHandler for UpdateHandler {
    async fn on_pipeline(&self, ctx: PipelineContext<'_>) -> anyhow::Result<Value> {
        let dry_run = ctx
            .metadata
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if dry_run {
            info!("running in DRY-RUN mode — no files will be committed");
        }

        let org = std::env::var("GITHUB_ORG").unwrap_or_else(|_| "ai-evo-agents".to_string());
        let king_addr =
            std::env::var("KING_ADDRESS").unwrap_or_else(|_| "http://localhost:3000".to_string());
        let base_dir: PathBuf = std::env::var("KERNEL_AGENTS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".."));

        // ── Phase 1: Check crates.io ────────────────────────────────────────
        info!("Phase 1: checking crates.io for latest versions");
        let http = reqwest::Client::new();
        let mut latest_versions: HashMap<&str, String> = HashMap::new();
        let mut version_reports: Vec<VersionReport> = Vec::new();

        for &crate_name in TRACKED_CRATES {
            match latest_crate_version(&http, crate_name).await {
                Ok(latest) => {
                    info!(crate = crate_name, latest = %latest, "fetched latest version");
                    latest_versions.insert(crate_name, latest);
                }
                Err(e) => {
                    warn!(crate = crate_name, error = %e, "failed to fetch version — skipping");
                }
            }
        }

        // ── Phase 2: Scan repos for stale deps ──────────────────────────────
        info!("Phase 2: scanning managed repos for outdated dependencies");
        let mut pending_updates: Vec<PendingUpdate> = Vec::new();
        let sdk_latest = latest_versions.get("evo-agent-sdk").cloned();
        let sdk_needs_update_any = sdk_latest.is_some(); // we'll check per-file below

        for spec in MANAGED_REPOS {
            let repo_base = base_dir.join(spec.local);

            // ── Cargo.toml files ──
            for &cargo_file in spec.cargo_files {
                let path = repo_base.join(cargo_file);
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(repo = spec.repo, file = cargo_file, error = %e, "cannot read file — skipping");
                        continue;
                    }
                };

                let mut patched = content.clone();
                let mut file_changed = false;

                for (&crate_name, latest) in &latest_versions {
                    if let Some(current) = current_dep_version(&patched, crate_name)
                        && needs_update(&current, latest)
                    {
                        info!(
                            repo = spec.repo,
                            file = cargo_file,
                            dep = crate_name,
                            current = %current,
                            latest = %latest,
                            "update needed"
                        );
                        version_reports.push(VersionReport {
                            crate_name: crate_name.to_string(),
                            current: current.clone(),
                            latest: latest.clone(),
                            needs_update: true,
                        });
                        match patch_cargo_toml(&patched, crate_name, latest) {
                            Ok(new) => {
                                patched = new;
                                file_changed = true;
                            }
                            Err(e) => {
                                warn!(repo = spec.repo, dep = crate_name, error = %e, "patch failed");
                            }
                        }
                    }
                }

                if file_changed {
                    let msg = format!(
                        "chore(deps): update dependencies in {cargo_file} [run_id={}]",
                        ctx.run_id
                    );
                    pending_updates.push(PendingUpdate {
                        repo: spec.repo,
                        local_base: repo_base.clone(),
                        file_path: cargo_file,
                        patched_content: patched,
                        commit_message: msg,
                    });
                }
            }

            // ── Workflow files (evo-agent-sdk sed pattern) ──
            if let Some(ref sdk_ver) = sdk_latest {
                for &wf_file in spec.workflow_files {
                    let path = repo_base.join(wf_file);
                    let content = match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    let patched = patch_workflow_sed(&content, "evo-agent-sdk", sdk_ver);
                    if patched != content {
                        info!(repo = spec.repo, file = wf_file, sdk = %sdk_ver, "workflow sed update needed");
                        pending_updates.push(PendingUpdate {
                            repo: spec.repo,
                            local_base: repo_base.clone(),
                            file_path: wf_file,
                            patched_content: patched,
                            commit_message: format!(
                                "ci: bump evo-agent-sdk to {sdk_ver} in sed pattern [run_id={}]",
                                ctx.run_id
                            ),
                        });
                    }
                }
            }
        }

        let _ = sdk_needs_update_any; // used implicitly via sdk_latest

        // ── Phase 3: LLM changelog analysis ────────────────────────────────
        info!("Phase 3: LLM changelog risk analysis");
        let analysis_summary = if pending_updates.is_empty() {
            "No dependency updates required — all repos are up to date.".to_string()
        } else {
            let update_list: Vec<String> = version_reports
                .iter()
                .map(|r| format!("{}: {} → {}", r.crate_name, r.current, r.latest))
                .collect();

            let prompt = format!(
                "The following Rust crate dependencies are being updated:\n{}\n\n\
                 Please provide a brief (2-3 sentence) risk assessment:\n\
                 - Are any of these likely to contain breaking changes?\n\
                 - Should automated dependency updates be applied immediately or held for review?\n\
                 - Any specific migration notes?",
                update_list.join("\n")
            );

            match ctx
                .gateway
                .chat_completion(
                    "gpt-4o-mini",
                    &ctx.soul.behavior,
                    &prompt,
                    Some(0.3),
                    Some(300),
                )
                .await
            {
                Ok(response) => response,
                Err(e) => {
                    warn!(error = %e, "LLM analysis failed — continuing without it");
                    format!("Analysis unavailable (gateway error: {e})")
                }
            }
        };

        info!(analysis = %analysis_summary, "LLM analysis complete");

        // ── Phase 4: Apply updates ──────────────────────────────────────────
        info!(
            count = pending_updates.len(),
            dry_run, "Phase 4: applying updates"
        );

        let mut committed: Vec<Value> = Vec::new();
        let mut errors: Vec<Value> = Vec::new();

        if !dry_run {
            for update in &pending_updates {
                match commit_file(
                    &org,
                    update.repo,
                    update.file_path,
                    &update.patched_content,
                    &update.commit_message,
                    Some(Path::new(&update.local_base)),
                )
                .await
                {
                    Ok(result) => {
                        info!(
                            repo = update.repo,
                            file = update.file_path,
                            sha = %result.sha,
                            strategy = ?result.strategy,
                            "committed"
                        );
                        committed.push(json!({
                            "repo": update.repo,
                            "file": update.file_path,
                            "sha": result.sha,
                            "strategy": format!("{:?}", result.strategy),
                        }));
                    }
                    Err(e) => {
                        warn!(repo = update.repo, file = update.file_path, error = %e, "commit failed");
                        errors.push(json!({
                            "repo": update.repo,
                            "file": update.file_path,
                            "error": e.to_string(),
                        }));
                    }
                }
            }
        } else {
            // In dry-run, list what would have been committed
            for update in &pending_updates {
                committed.push(json!({
                    "repo": update.repo,
                    "file": update.file_path,
                    "dry_run": true,
                    "commit_message": update.commit_message,
                }));
            }
        }

        // ── Phase 5: Config sync ────────────────────────────────────────────
        info!("Phase 5: requesting config sync from king");
        let config_synced = if !dry_run && !committed.is_empty() {
            let sync_url = format!("{king_addr}/admin/config-sync");
            match http.post(&sync_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!("config-sync accepted by king");
                    true
                }
                Ok(resp) => {
                    warn!(status = %resp.status(), "config-sync returned non-success");
                    false
                }
                Err(e) => {
                    warn!(error = %e, "config-sync request failed");
                    false
                }
            }
        } else {
            false
        };

        // ── Phase 6: Return JSON summary ────────────────────────────────────
        info!(
            committed = committed.len(),
            errors = errors.len(),
            config_synced,
            "Phase 6: done"
        );

        Ok(json!({
            "run_id": ctx.run_id,
            "dry_run": dry_run,
            "versions": latest_versions,
            "pending_updates": pending_updates.len(),
            "committed": committed,
            "errors": errors,
            "config_synced": config_synced,
            "analysis_summary": analysis_summary,
        }))
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    AgentRunner::run(UpdateHandler).await
}
