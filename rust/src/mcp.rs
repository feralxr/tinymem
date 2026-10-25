//! Minimal Model Context Protocol server: JSON-RPC 2.0 over stdio, newline-delimited.

use crate::store::{SearchOpts, Store, VERSION};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;
use std::time::{Duration, Instant};

const PROTOCOL: &str = "2025-06-18";
const DEFAULT_BUDGET: u64 = 4000;

fn tools() -> Value {
    json!([
        {
            "name": "search",
            "description": "Hybrid (keyword + semantic) search over the indexed code, docs, PDFs and saved memories. Returns ranked chunks with file path, line range (or PDF page) and a text snippet.",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string", "description": "Natural-language or identifier query"},
                "k": {"type": "integer", "description": "Max results (default 8)"},
                "budget": {"type": "integer", "description": "Max estimated tokens to return (default 4000)"},
                "mode": {"type": "string", "enum": ["hybrid", "lexical", "semantic"]},
                "path": {"type": "string", "description": "Only paths matching this substring or glob"},
                "kind": {"type": "array", "items": {"type": "string", "enum": ["code", "doc", "pdf", "memory"]}}
            }, "required": ["query"]}
        },
        {
            "name": "remember",
            "description": "Save a note (fact, decision, preference) to long-term memory so it can be found by later searches.",
            "inputSchema": {"type": "object", "properties": {
                "text": {"type": "string"}, "tags": {"type": "array", "items": {"type": "string"}}
            }, "required": ["text"]}
        },
        {
            "name": "forget",
            "description": "Delete a saved memory by id.",
            "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}
        },
        {
            "name": "reindex",
            "description": "Re-scan the project folder and update the index incrementally.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "status",
            "description": "Index statistics: file and chunk counts, last index time.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ])
}

struct Server {
    store: Store,
    refresh_interval: Duration,
    last_refresh: Option<Instant>,
}

impl Server {
    fn refresh(&mut self) -> Result<()> {
        let Some(root) = self.store.meta("root")? else {
            return Ok(());
        };
        if !Path::new(&root).is_dir() {
            return Ok(());
        }
        if let Some(t) = self.last_refresh {
            if t.elapsed() < self.refresh_interval {
                return Ok(());
            }
        }
        self.store.index(Path::new(&root), &[], crate::store::MAX_FILE_BYTES)?;
        self.last_refresh = Some(Instant::now());
        Ok(())
    }

    fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
        match name {
            "search" => {
                self.refresh()?;
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("missing query"))?;
                let k = args.get("k").and_then(Value::as_u64).unwrap_or(8) as usize;
                let mode = args.get("mode").and_then(Value::as_str).unwrap_or("hybrid");
                let path = args.get("path").and_then(Value::as_str);
                let kinds: Vec<String> = args
                    .get("kind")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let results = self.store.search(
                    query,
                    &SearchOpts {
                        k,
                        mode,
                        path,
                        kinds: &kinds,
                        budget: args.get("budget").and_then(Value::as_u64).unwrap_or(DEFAULT_BUDGET) as usize,
                        ..Default::default()
                    },
                )?;
                Ok(json!({ "results": results }))
            }
            "remember" => {
                let text = args
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("missing text"))?;
                let tags: Vec<String> = args
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                Ok(json!({ "id": self.store.remember(text, &tags)? }))
            }
            "forget" => {
                let id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("missing id"))?;
                Ok(json!({ "forgotten": self.store.forget(id)? }))
            }
            "reindex" => {
                let root = self
                    .store
                    .meta("root")?
                    .ok_or_else(|| anyhow!("no project folder indexed yet; run `tinymem index` first"))?;
                self.last_refresh = Some(Instant::now());
                self.store.index(Path::new(&root), &[], crate::store::MAX_FILE_BYTES)
            }
            "status" => self.store.status(),
            other => Err(anyhow!("unknown tool: {}", other)),
        }
    }

    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").filter(|v| !v.is_null())?.clone();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let result = match method {
            "initialize" => json!({
                "protocolVersion": msg.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "tinymem", "version": VERSION},
            }),
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => {
                let name = msg
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let empty = json!({});
                let args = msg.pointer("/params/arguments").unwrap_or(&empty).clone();
                match self.call(&name, &args) {
                    Ok(v) => json!({"content": [{"type": "text", "text": v.to_string()}], "isError": false}),
                    Err(e) => json!({"content": [{"type": "text", "text": format!("error: {}", e)}], "isError": true}),
                }
            }
            other => {
                return Some(json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": format!("method not found: {}", other)}
                }))
            }
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }
}

pub fn serve(store: Store, refresh_interval: f64) -> Result<()> {
    let mut srv = Server {
        store,
        refresh_interval: Duration::from_secs_f64(refresh_interval),
        last_refresh: None,
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(line) {
            Ok(msg) => srv.handle(&msg),
            Err(_) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}})),
        };
        if let Some(r) = resp {
            writeln!(out, "{}", r)?;
            out.flush()?;
        }
    }
    Ok(())
}
