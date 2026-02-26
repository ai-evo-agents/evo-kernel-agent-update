use anyhow::{Context, Result};
use regex::Regex;

// ─── Cargo.toml patching ─────────────────────────────────────────────────────

/// Updates the version of `dep_name` in a Cargo.toml string using `toml_edit`,
/// preserving existing formatting and comments.
///
/// Handles both:
/// - `dep_name = "X.Y.Z"` (simple string form)
/// - `dep_name = { version = "X.Y.Z", ... }` (inline table form)
pub fn patch_cargo_toml(content: &str, dep_name: &str, new_version: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .with_context(|| format!("parse Cargo.toml to patch {dep_name}"))?;

    let deps = doc
        .get_mut("dependencies")
        .with_context(|| "no [dependencies] section found")?;

    let dep = deps
        .get_mut(dep_name)
        .with_context(|| format!("dependency {dep_name} not found in [dependencies]"))?;

    if dep.is_str() {
        // Simple form: `dep = "1.2"`
        *dep = toml_edit::value(new_version);
    } else if let Some(table) = dep.as_inline_table_mut() {
        // Inline table: `dep = { version = "1.2", ... }`
        if let Some(v) = table.get_mut("version") {
            *v = toml_edit::Value::from(new_version);
        }
    } else if let Some(table) = dep.as_table_mut() {
        // Block table:
        // [dependencies.dep]
        // version = "1.2"
        if let Some(v) = table.get_mut("version") {
            *v = toml_edit::value(new_version);
        }
    } else {
        anyhow::bail!("unexpected TOML shape for dependency {dep_name} — cannot patch version");
    }

    Ok(doc.to_string())
}

// ─── Workflow YAML patching ───────────────────────────────────────────────────

/// Updates the version string for `dep_name` inside GitHub Actions workflow
/// YAML files that use a `sed` substitution pattern, e.g.:
///
/// ```yaml
/// run: |
///   sed -i.bak 's|evo-agent-sdk = { path = "[^"]*" }|evo-agent-sdk = "0.2"|' Cargo.toml
/// ```
///
/// The function rewrites *only* the literal crates.io version on the right-hand
/// side of that `|dep_name = "VERSION"|` sed replacement target, leaving
/// everything else in the file unchanged.
///
/// Returns the (possibly unchanged) content — never errors so the caller can
/// decide whether the absence of a match is a problem.
pub fn patch_workflow_sed(content: &str, dep_name: &str, new_version: &str) -> String {
    // Match: dep_name = "OLD_VERSION" at the end of a sed replacement block.
    // The sed line looks like:  …|dep_name = "OLD"|' …
    // We specifically target the escaped-quote pattern used in shell sed args.
    let pattern = format!(
        r#"(\|{dep_name} = ")([\d][^"]*)(")"#,
        dep_name = regex::escape(dep_name)
    );

    // SAFETY: the pattern is constructed from known-safe components.
    let re = Regex::new(&pattern).expect("patch_workflow_sed regex is valid");

    re.replace_all(content, |caps: &regex::Captures| {
        format!("{}{}{}", &caps[1], new_version, &caps[3])
    })
    .into_owned()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Cargo.toml patching ──

    #[test]
    fn test_patch_simple_dep() {
        let toml = r#"
[dependencies]
evo-common = "0.2"
tokio = { version = "1", features = ["full"] }
"#;
        let patched = patch_cargo_toml(toml, "evo-common", "0.3").unwrap();
        assert!(patched.contains("evo-common = \"0.3\""));
        // Other deps should be untouched
        assert!(patched.contains("tokio"));
    }

    #[test]
    fn test_patch_inline_table_dep() {
        let toml = r#"
[dependencies]
evo-agent-sdk = { version = "0.1", features = ["full"] }
"#;
        let patched = patch_cargo_toml(toml, "evo-agent-sdk", "0.2").unwrap();
        assert!(patched.contains("\"0.2\""));
        // features should remain
        assert!(patched.contains("features"));
    }

    #[test]
    fn test_patch_missing_dep_errors() {
        let toml = "[dependencies]\n";
        let result = patch_cargo_toml(toml, "missing-crate", "1.0");
        assert!(result.is_err());
    }

    #[test]
    fn test_patch_preserves_other_content() {
        let toml = r#"
[package]
name = "my-crate"
version = "1.0.0"

[dependencies]
serde = "1"
evo-common = "0.2"
"#;
        let patched = patch_cargo_toml(toml, "evo-common", "0.3").unwrap();
        assert!(patched.contains("[package]"));
        assert!(patched.contains("name = \"my-crate\""));
        assert!(patched.contains("serde = \"1\""));
        assert!(patched.contains("evo-common = \"0.3\""));
    }

    // ── Workflow sed patching ──

    #[test]
    fn test_patch_workflow_sed_basic() {
        let yaml = r#"
      - name: Use crates.io dependencies
        run: |
          sed -i.bak 's|evo-agent-sdk = { path = "[^"]*" }|evo-agent-sdk = "0.1"|' Cargo.toml
          rm -f Cargo.toml.bak
"#;
        let patched = patch_workflow_sed(yaml, "evo-agent-sdk", "0.2");
        assert!(patched.contains("evo-agent-sdk = \"0.2\""));
        assert!(!patched.contains("\"0.1\""));
    }

    #[test]
    fn test_patch_workflow_sed_no_match_unchanged() {
        let yaml = "steps:\n  - run: echo hello\n";
        let patched = patch_workflow_sed(yaml, "evo-agent-sdk", "0.2");
        assert_eq!(yaml, patched.as_str());
    }

    #[test]
    fn test_patch_workflow_sed_leaves_other_lines_untouched() {
        let yaml = r#"
      - run: cargo fmt --check
      - run: |
          sed -i.bak 's|evo-agent-sdk = { path = "[^"]*" }|evo-agent-sdk = "0.1"|' Cargo.toml
      - run: cargo test
"#;
        let patched = patch_workflow_sed(yaml, "evo-agent-sdk", "0.2");
        assert!(patched.contains("cargo fmt --check"));
        assert!(patched.contains("cargo test"));
        assert!(patched.contains("\"0.2\""));
    }
}
