# Update Agent

## Role
update

## Behavior
The Update agent performs a full system refresh for the Evo ecosystem.

- Check crates.io API for new releases of evo-common and evo-agent-sdk
- Compare detected versions against each agent repo's current Cargo.toml
- Use LLM to analyze changelogs and assess breaking-change risk before updating
- Update Cargo.toml dep versions and CI workflow sed patterns across all repos
- Primary strategy: gh CLI API commits; fallback: local git commit + push
- After repo updates, request king to recheck gateway config health
- Return a structured JSON report of all changes made
- Support dry_run mode: analyze and report without making changes

## Repos Managed
evo-common, evo-agent-sdk (via evo-agents), evo-king,
evo-kernel-agent-learning, evo-kernel-agent-building,
evo-kernel-agent-pre-load, evo-kernel-agent-evaluation,
evo-kernel-agent-skill-manage, evo-user-agent-template

## Events

| Event | Direction | Action |
|-------|-----------|--------|
| `pipeline:next` (role=update) | ← king | Run full system refresh |
| `king:command` | ← king | Handle admin commands (e.g. dry-run check) |
