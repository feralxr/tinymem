//! `tinymem init`: wire a project's coding agents up to tinymem in one command.
//!
//! Writes (or merges into) each agent's MCP config and a short instruction block telling the agent when to
//! search and when to remember. Existing files are merged, never overwritten, and re-running is a no-op.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};
use std::path::Path;

const START: &str = "<!-- tinymem:start -->";
const END: &str = "<!-- tinymem:end -->";

const INSTRUCTIONS: &str = "<!-- tinymem:start -->
## Project search and memory (tinymem)

This project is indexed by tinymem. Use it instead of reading files blindly.

- To answer a question about the codebase, call the `search` tool first with a short plain-English or identifier query. Each result gives the file, line range and a snippet.
- When you learn something durable (a decision, a gotcha, a convention, why something is the way it is), call `remember` with one or two sentences so later sessions can find it.
- At the start of a task, `search` with `kind: [\"memory\"]` to recall earlier decisions.
- After large edits made outside this session, call `reindex`.
<!-- tinymem:end -->
";

pub const CODEX_NOTE: &str = "Codex reads MCP servers from ~/.codex/config.toml. Add:

  [mcp_servers.tinymem]
  command = \"tinymem\"
  args = [\"serve\"]
";

/// agent -> (MCP config path, instruction file)
const AGENTS: &[(&str, Option<&str>, &str)] = &[
    ("claude", Some(".mcp.json"), "CLAUDE.md"),
    ("cursor", Some(".cursor/mcp.json"), "AGENTS.md"),
    ("gemini", Some(".gemini/settings.json"), "GEMINI.md"),
    ("codex", None, "AGENTS.md"),
];

fn server() -> Value {
    json!({"command": "tinymem", "args": ["serve"]})
}

fn merge_mcp_config(path: &Path, dry_run: bool) -> Result<&'static str> {
    let existed = path.exists();
    let mut data: Value = Value::Object(Map::new());
    if existed {
        let text = std::fs::read_to_string(path)?;
        let text = if text.trim().is_empty() { "{}".to_string() } else { text };
        data = serde_json::from_str(&text).map_err(|e| {
            anyhow!(
                "{} is not valid JSON ({}); fix it or remove it, then re-run",
                path.display(),
                e
            )
        })?;
        if !data.is_object() {
            bail!("{} must contain a JSON object", path.display());
        }
    }
    let obj = data.as_object_mut().unwrap();
    let servers = obj.entry("mcpServers").or_insert_with(|| Value::Object(Map::new()));
    let Some(servers) = servers.as_object_mut() else {
        bail!("{}: mcpServers must be an object", path.display());
    };
    if servers.get("tinymem") == Some(&server()) {
        return Ok("unchanged");
    }
    servers.insert("tinymem".into(), server());
    if !dry_run {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&data)? + "\n")?;
    }
    Ok(if existed { "updated" } else { "created" })
}

fn merge_instructions(path: &Path, dry_run: bool) -> Result<&'static str> {
    let existing = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    let new = match (existing.find(START), existing.find(END)) {
        (Some(a), Some(b)) if b > a => {
            format!(
                "{}{}{}",
                &existing[..a],
                INSTRUCTIONS.trim_end_matches('\n'),
                &existing[b + END.len()..]
            )
        }
        _ if !existing.is_empty() => format!("{}\n\n{}", existing.trim_end_matches('\n'), INSTRUCTIONS),
        _ => INSTRUCTIONS.to_string(),
    };
    if new == existing {
        return Ok("unchanged");
    }
    if !dry_run {
        std::fs::write(path, &new)?;
    }
    Ok(if existing.is_empty() { "created" } else { "updated" })
}

/// Returns `(relative path, "created" | "updated" | "unchanged")` in a stable order.
pub fn init_project(root: &Path, agents: &[String], dry_run: bool) -> Result<Vec<(String, &'static str)>> {
    let mut names: Vec<&str> = Vec::new();
    if agents.iter().any(|a| a == "all") {
        names.extend(AGENTS.iter().map(|a| a.0));
    } else {
        for a in agents {
            if !names.contains(&a.as_str()) {
                names.push(a);
            }
        }
    }
    let mut done: Vec<(String, &'static str)> = Vec::new();
    for n in names {
        let Some(&(_, cfg, doc)) = AGENTS.iter().find(|a| a.0 == n) else {
            bail!(
                "unknown agent {:?} (choose from: claude, cursor, gemini, codex, all)",
                n
            );
        };
        if let Some(cfg) = cfg {
            if !done.iter().any(|d| d.0 == cfg) {
                done.push((cfg.to_string(), merge_mcp_config(&root.join(cfg), dry_run)?));
            }
        }
        if !done.iter().any(|d| d.0 == doc) {
            done.push((doc.to_string(), merge_instructions(&root.join(doc), dry_run)?));
        }
    }
    Ok(done)
}
