use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ─── crates.io API types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CratesIoCrate {
    #[serde(rename = "crate")]
    krate: CratesIoInfo,
}

#[derive(Debug, Deserialize)]
struct CratesIoInfo {
    max_stable_version: String,
}

// ─── Public types ─────────────────────────────────────────────────────────────

/// Result of a version comparison for a single crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionReport {
    pub crate_name: String,
    pub current: String,
    pub latest: String,
    pub needs_update: bool,
}

// ─── Version detection ───────────────────────────────────────────────────────

/// Calls the crates.io API and returns the latest stable version string for a crate.
///
/// Uses the `User-Agent` header required by crates.io policy.
pub async fn latest_crate_version(client: &reqwest::Client, crate_name: &str) -> Result<String> {
    let url = format!("https://crates.io/api/v1/crates/{crate_name}");
    let resp = client
        .get(&url)
        .header("User-Agent", "evo-kernel-agent-update/0.1.0 (github.com/ai-evo-agents)")
        .send()
        .await
        .with_context(|| format!("HTTP request to crates.io for {crate_name}"))?;

    if !resp.status().is_success() {
        anyhow::bail!("crates.io returned {} for crate {crate_name}", resp.status());
    }

    let data: CratesIoCrate = resp
        .json()
        .await
        .with_context(|| format!("parse crates.io response for {crate_name}"))?;

    Ok(data.krate.max_stable_version)
}

/// Reads the current simple version of a dependency from a Cargo.toml string.
///
/// Handles both:
/// - `dep_name = "X.Y.Z"` (simple string)
/// - `dep_name = { version = "X.Y.Z", ... }` (table form)
pub fn current_dep_version(cargo_toml: &str, dep_name: &str) -> Option<String> {
    let doc: toml_edit::DocumentMut = cargo_toml.parse().ok()?;
    let deps = doc.get("dependencies")?;

    let dep = deps.get(dep_name)?;

    if let Some(ver_str) = dep.as_str() {
        // Simple: `dep = "1.2"`
        return Some(ver_str.to_string());
    }

    // Inline table: `dep = { version = "1.2", features = [...] }`
    if let Some(table) = dep.as_inline_table() {
        if table.get("path").is_some() {
            return None;
        }
        if let Some(v) = table.get("version") {
            return v.as_str().map(|s| s.to_string());
        }
    }

    // Block table:
    // [dependencies.dep]
    // version = "1.2"
    if let Some(table) = dep.as_table() {
        if table.get("path").is_some() {
            return None;
        }
        if let Some(v) = table.get("version") {
            return v.as_str().map(|s| s.to_string());
        }
    }

    None
}

/// Compares two semver strings. Returns `true` if `latest` is strictly newer than `current`.
///
/// Parses major.minor.patch components, ignoring pre-release suffixes for simplicity.
pub fn needs_update(current: &str, latest: &str) -> bool {
    parse_semver(latest) > parse_semver(current)
}

fn parse_semver(v: &str) -> (u64, u64, u64) {
    let parts: Vec<u64> = v
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_needs_update_newer() {
        assert!(needs_update("0.2.0", "0.3.0"));
        assert!(needs_update("0.2.0", "1.0.0"));
        assert!(needs_update("0.2.1", "0.2.2"));
    }

    #[test]
    fn test_needs_update_same_or_older() {
        assert!(!needs_update("0.3.0", "0.3.0"));
        assert!(!needs_update("0.3.0", "0.2.0"));
    }

    #[test]
    fn test_current_dep_version_simple() {
        let toml = r#"
[dependencies]
evo-common = "0.2"
tokio = { version = "1", features = ["full"] }
"#;
        assert_eq!(current_dep_version(toml, "evo-common"), Some("0.2".to_string()));
        assert_eq!(current_dep_version(toml, "tokio"), Some("1".to_string()));
    }

    #[test]
    fn test_current_dep_version_path_dep() {
        let toml = r#"
[dependencies]
evo-agent-sdk = { path = "../evo-agents/evo-agent-sdk" }
"#;
        // Path deps have no semver to compare
        assert_eq!(current_dep_version(toml, "evo-agent-sdk"), None);
    }

    #[test]
    fn test_current_dep_version_missing() {
        let toml = "[dependencies]\n";
        assert_eq!(current_dep_version(toml, "missing-crate"), None);
    }
}
