# Update Agent (`evo-kernel-agent-update`)

The update agent automates dependency version bumps and config synchronization
across the entire Evo ecosystem — work that was previously done manually.

---

## What It Does

Each time it runs, the agent performs six phases:

| Phase | Description |
|-------|-------------|
| 1 | Check crates.io for the latest stable version of `evo-common` and `evo-agent-sdk` |
| 2 | Scan every managed repo's `Cargo.toml` (and CI workflow files) for stale dep versions |
| 3 | Ask the LLM gateway for a brief changelog-risk assessment |
| 4 | Patch stale files and commit via `gh` CLI (falls back to local `git push`) |
| 5 | POST `king /admin/config-sync` to trigger a gateway config health recheck |
| 6 | Return a structured JSON summary |

---

## Trigger Methods

### Manual trigger

```sh
curl -X POST http://localhost:3000/pipeline/start \
     -H "Content-Type: application/json" \
     -d '{"trigger":"manual","metadata":{}}'
```

### Dry-run (no files committed)

```sh
curl -X POST http://localhost:3000/pipeline/start \
     -H "Content-Type: application/json" \
     -d '{"trigger":"manual","metadata":{"dry_run":true}}'
```

### Automatic (king daily cron)

King seeds a `daily_update_check` cron job on startup that dispatches to the
`role:update` Socket.IO room every 24 hours at midnight UTC.  No configuration
required — it runs automatically whenever the agent is connected.

You can also trigger it manually via the king admin API:

```sh
curl -X POST http://localhost:3000/admin/crons/daily_update_check/run
```

---

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `GITHUB_ORG` | `ai-evo-agents` | GitHub organisation owning the managed repos |
| `GITHUB_TOKEN` | — | Token used by `gh` CLI for API commits (needs `repo` write scope) |
| `KERNEL_AGENTS_DIR` | `..` | Base directory containing all `evo-*` repo checkouts |
| `KING_ADDRESS` | `http://localhost:3000` | King server URL (for config-sync POST) |
| `GATEWAY_ADDRESS` | `http://localhost:8080` | Gateway URL (for LLM analysis in Phase 3) |

### gh CLI authentication

The agent uses `gh` CLI for remote commits.  Verify auth is set up correctly:

```sh
gh auth status          # should show repo write access
gh auth token           # should print a token
```

If `gh` CLI is unavailable or auth fails, the agent automatically falls back to
local `git add / commit / push` for repos that are checked out under
`KERNEL_AGENTS_DIR`.

---

## Managed Repos

The list of repos is hardcoded in `src/main.rs` as `MANAGED_REPOS`.  Each entry
specifies:

- `repo` — GitHub repo slug
- `local` — local folder name relative to `KERNEL_AGENTS_DIR`
- `cargo_files` — Cargo.toml paths to scan for tracked dep versions
- `workflow_files` — CI/release workflow files that contain `sed` version patterns

### Adding a new repo

1. Open `src/main.rs`
2. Append a new `RepoSpec` to the `MANAGED_REPOS` slice:

```rust
RepoSpec {
    repo: "evo-my-new-agent",
    local: "evo-my-new-agent",
    cargo_files: &["Cargo.toml"],
    workflow_files: &[
        ".github/workflows/ci.yml",
        ".github/workflows/release.yml",
    ],
},
```

3. Rebuild and redeploy the agent binary.

---

## JSON Output Schema

The agent returns JSON with the following structure:

```json
{
  "run_id": "abc-123",
  "dry_run": false,
  "versions": {
    "evo-common": "0.4.0",
    "evo-agent-sdk": "0.3.0"
  },
  "pending_updates": 4,
  "committed": [
    {
      "repo": "evo-king",
      "file": "Cargo.toml",
      "sha": "a1b2c3d",
      "strategy": "GhCli"
    }
  ],
  "errors": [],
  "config_synced": true,
  "analysis_summary": "Minor version bumps — no breaking changes expected..."
}
```

---

## Building and Running Locally

```sh
cd evo-kernel-agent-update

# Build
cargo build --release

# Run standalone (connects to king at localhost:3000)
KING_ADDRESS=http://localhost:3000 \
GATEWAY_ADDRESS=http://localhost:8080 \
KERNEL_AGENTS_DIR=.. \
./target/release/evo-agent-update
```

King will auto-discover this agent at startup because the folder name matches
the `evo-kernel-agent-*` prefix and contains a `soul.md` file.

---

## Release Builds

Tagged releases trigger the `release.yml` workflow which cross-compiles for
five platforms:

| Platform | Target |
|----------|--------|
| Linux x86_64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |
| macOS Intel | `x86_64-apple-darwin` |
| macOS Apple Silicon | `aarch64-apple-darwin` |
| Windows x86_64 | `x86_64-pc-windows-msvc` |

To cut a release:

```sh
git tag v0.1.0
git push origin v0.1.0
```
