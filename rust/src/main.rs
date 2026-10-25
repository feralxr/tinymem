//! tinymem: tiny local hybrid search and memory for agents.

mod chunk;
mod discover;
mod embed;
mod extract;
mod glob;
mod init;
mod mcp;
mod store;
#[cfg(test)]
mod tests;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;
use store::{find_db, SearchOpts, Store, MAX_FILE_BYTES, VERSION};

#[derive(Parser)]
#[command(name = "tinymem", version = VERSION, about = "Tiny local hybrid search and memory for agents.")]
struct Cli {
    /// Path to index.db (default: nearest .tinymem/index.db)
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index a folder (incremental)
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Skip matching paths (repeatable)
        #[arg(long, value_name = "GLOB")]
        exclude: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Hybrid search
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        /// Max results
        #[arg(short, default_value_t = 10)]
        k: usize,
        #[arg(long, default_value = "hybrid", value_parser = ["hybrid", "lexical", "semantic"])]
        mode: String,
        /// Restrict to paths matching a substring or glob
        #[arg(long)]
        path: Option<String>,
        /// Comma list: code,doc,pdf,memory
        #[arg(long)]
        kind: Option<String>,
        /// Max results per file (0 = unlimited)
        #[arg(long, default_value_t = 3)]
        per_file: usize,
        /// Snippet height in lines
        #[arg(long, default_value_t = 12)]
        lines: usize,
        /// Return whole chunks instead of snippets
        #[arg(long)]
        full: bool,
        /// Cap the total size of results (estimated tokens)
        #[arg(long, default_value_t = 0, value_name = "TOKENS")]
        budget: usize,
        /// Re-scan for changes before searching
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        json: bool,
    },
    /// Index this project and set up your coding agent(s)
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Agent to configure (repeatable; default: claude)
        #[arg(long, value_parser = ["claude", "cursor", "gemini", "codex", "all"])]
        agent: Vec<String>,
        /// Only write agent config, do not index
        #[arg(long)]
        no_index: bool,
        /// Show what would change without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Save a note to memory ('-' reads stdin)
    Remember {
        text: String,
        #[arg(long)]
        tag: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Delete a memory by id
    Forget { id: String },
    /// List saved memories
    Memories {
        #[arg(long)]
        json: bool,
    },
    /// Index statistics
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Run an MCP server on stdio
    Serve {
        /// Seconds between automatic re-scans
        #[arg(long, default_value_t = 5.0)]
        refresh_interval: f64,
    },
}

fn open(db: &Option<PathBuf>, create: bool, root: Option<&Path>) -> Result<Store> {
    if let Some(p) = db {
        return Store::open(p, create);
    }
    if let Some(found) = find_db() {
        return Store::open(&found, false);
    }
    if create {
        let base = match root {
            Some(r) => r.to_path_buf(),
            None => std::env::current_dir()?,
        };
        return Store::open(&base.join(".tinymem").join("index.db"), true);
    }
    anyhow::bail!("no .tinymem index found here or in a parent folder (run `tinymem index` first, or pass --db)")
}

fn emit(v: &Value) {
    println!("{}", v);
}

fn run(cli: Cli) -> Result<i32> {
    match cli.cmd {
        Cmd::Index { path, exclude, json } => {
            let root = path.canonicalize().unwrap_or(path.clone());
            let store = open(&cli.db, true, Some(&root))?;
            let res = store.index(&root, &exclude, MAX_FILE_BYTES)?;
            if json {
                emit(&res);
            } else {
                println!(
                    "indexed {}: {} added, {} updated, {} unchanged, {} removed, {} skipped ({} chunks, {:.2}s)",
                    root.display(),
                    res["added"],
                    res["updated"],
                    res["unchanged"],
                    res["removed"],
                    res["skipped"],
                    res["chunks"],
                    res["seconds"].as_f64().unwrap_or(0.0)
                );
                for e in res["errors"].as_array().into_iter().flatten() {
                    eprintln!("  warning: {}", e.as_str().unwrap_or(""));
                }
            }
        }
        Cmd::Search {
            query,
            k,
            mode,
            path,
            kind,
            per_file,
            lines,
            full,
            budget,
            refresh,
            json,
        } => {
            let store = open(&cli.db, false, None)?;
            let t0 = Instant::now();
            if refresh {
                if let Some(root) = store.meta("root")? {
                    store.index(Path::new(&root), &[], MAX_FILE_BYTES)?;
                }
            }
            let kinds: Vec<String> = kind
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            let q = query.join(" ");
            let opts = SearchOpts {
                k,
                mode: &mode,
                path: path.as_deref(),
                kinds: &kinds,
                per_file,
                lines: if full { 0 } else { lines },
                budget,
            };
            let results = store.search(&q, &opts)?;
            if json {
                emit(&json!({
                    "query": q, "root": store.meta("root")?, "took_ms": t0.elapsed().as_millis() as u64,
                    "results": results,
                }));
            } else {
                for (i, r) in results.iter().enumerate() {
                    let page = r.get("page").map(|p| format!(" p.{}", p)).unwrap_or_default();
                    println!(
                        "{:>2}. {}{}:{}-{}  [{}]  {:.4}",
                        i + 1,
                        r["path"].as_str().unwrap_or(""),
                        page,
                        r["start_line"],
                        r["end_line"],
                        r["kind"].as_str().unwrap_or(""),
                        r["score"].as_f64().unwrap_or(0.0)
                    );
                    let start = r["snippet_start_line"].as_i64().unwrap_or(1);
                    for (n, ln) in r["text"].as_str().unwrap_or("").split('\n').enumerate() {
                        let clipped: String = ln.chars().take(160).collect();
                        println!("    {:>4}  {}", start + n as i64, clipped);
                    }
                    println!();
                }
                if results.is_empty() {
                    eprintln!("no results");
                }
            }
        }
        Cmd::Init {
            path,
            agent,
            no_index,
            dry_run,
        } => {
            let root = path
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("not a directory: {}: {}", path.display(), e))?;
            if !root.is_dir() {
                anyhow::bail!("not a directory: {}", root.display());
            }
            let agents = if agent.is_empty() {
                vec!["claude".to_string()]
            } else {
                agent
            };
            if !no_index && !dry_run {
                let store = open(&cli.db, true, Some(&root))?;
                let res = store.index(&root, &[], MAX_FILE_BYTES)?;
                let n = |k: &str| res[k].as_u64().unwrap_or(0);
                println!(
                    "indexed {}: {} files, {} chunks",
                    root.display(),
                    n("added") + n("updated") + n("unchanged"),
                    n("chunks")
                );
            }
            for (rel, status) in init::init_project(&root, &agents, dry_run)? {
                let note = if dry_run && status != "unchanged" {
                    " (dry run)"
                } else {
                    ""
                };
                println!("{:<9} {}{}", status, rel, note);
            }
            if agents.iter().any(|a| a == "codex" || a == "all") {
                println!("\n{}", init::CODEX_NOTE);
            }
            println!("Restart your agent so it picks up the new MCP server.");
        }
        Cmd::Remember { text, tag, json } => {
            let text = if text == "-" {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s
            } else {
                text
            };
            let store = open(&cli.db, true, None)?;
            let id = store.remember(&text, &tag)?;
            if json {
                emit(&json!({ "id": id }));
            } else {
                println!("{}", id);
            }
        }
        Cmd::Forget { id } => {
            if !open(&cli.db, false, None)?.forget(&id)? {
                eprintln!("no such memory: {}", id);
                return Ok(1);
            }
        }
        Cmd::Memories { json } => {
            let mems = open(&cli.db, false, None)?.memories()?;
            if json {
                emit(&Value::Array(mems));
            } else {
                for m in mems {
                    let t = m["text"].as_str().unwrap_or("").replace('\n', " ");
                    let t: String = t.chars().take(120).collect();
                    println!("{}  {}", m["id"].as_str().unwrap_or(""), t);
                }
            }
        }
        Cmd::Status { json } => {
            let st = open(&cli.db, false, None)?.status()?;
            if json {
                emit(&st);
            } else if let Some(o) = st.as_object() {
                for (k, v) in o {
                    let v = match v.as_str() {
                        Some(s) => s.to_string(),
                        None => v.to_string(),
                    };
                    println!("{:<11} {}", k, v);
                }
            }
        }
        Cmd::Serve { refresh_interval } => {
            mcp::serve(open(&cli.db, false, None)?, refresh_interval)?;
        }
    }
    Ok(0)
}

fn main() {
    // Silence panic messages from the PDF parser (it is run under catch_unwind); keep others.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !extract::in_pdf() {
            default_hook(info);
        }
    }));
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("tinymem: {}", e);
            std::process::exit(1);
        }
    }
}
