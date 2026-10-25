//! SQLite-backed index: files, chunks, FTS5 and int8 vectors, plus hybrid search and memory.

use crate::chunk::{chunk_lines, split_lines};
use crate::discover::{classify, discover, should_skip};
use crate::embed::{
    embed, embed_blob, fts_query, is_low_priority, symbols_of, terms_of, DIM, EMBEDDER, LOW_PRIORITY_QUERY_WORDS,
};
use crate::extract::pdf_pages;
use crate::glob::glob_match;
use anyhow::{bail, Result};
use rayon::prelude::*;
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const FORMAT: &str = "2";
const RRF_K: f64 = 60.0; // rank smoothing for the semantic list
const RRF_K_LEX: f64 = 20.0; // rank smoothing for the keyword list
const W_LEXICAL: f64 = 1.0;
const W_SEMANTIC: f64 = 0.2;
const BM25_WEIGHTS: &str = "1.0, 0.8, 4.0, 6.0"; // text, terms, path, symbols
const LOW_PRIORITY_FACTOR: f64 = 0.55;
const SEMANTIC_MIN_SCORE: f32 = 0.10;
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS files(
  path TEXT PRIMARY KEY, size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,
  sha256 TEXT NOT NULL, kind TEXT NOT NULL, chunks INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS chunks(
  id INTEGER PRIMARY KEY, path TEXT NOT NULL, kind TEXT NOT NULL,
  start_line INTEGER NOT NULL, end_line INTEGER NOT NULL, page INTEGER,
  text TEXT NOT NULL, vec BLOB NOT NULL);
CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path);
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
  text, terms, path, symbols, tokenize='porter unicode61 remove_diacritics 2');
";

/// Walk upwards from the current directory looking for `.tinymem/index.db`.
pub fn find_db() -> Option<PathBuf> {
    let cur = std::env::current_dir().ok()?;
    let mut dir: Option<&Path> = Some(&cur);
    while let Some(d) = dir {
        let cand = d.join(".tinymem").join("index.db");
        if cand.is_file() {
            return Some(cand);
        }
        dir = d.parent();
    }
    None
}

fn glob_pattern(p: Option<&str>) -> Option<String> {
    let p = p?;
    if p.is_empty() {
        return None;
    }
    Some(if p.contains(['*', '?', '[']) {
        p.to_string()
    } else {
        format!("*{}*", p)
    })
}

struct ChunkRec {
    start: usize,
    end: usize,
    page: Option<i64>,
    text: String,
    terms: String,
    symbols: String,
    vec: Vec<u8>,
}

fn make_rec(kind: &str, start: usize, end: usize, page: Option<i64>, text: String) -> ChunkRec {
    ChunkRec {
        start,
        end,
        page,
        terms: terms_of(&text),
        symbols: symbols_of(kind, &text),
        vec: embed_blob(&text),
        text,
    }
}

struct Todo {
    rel: String,
    abs: PathBuf,
    size: i64,
    mtime: i64,
    kind: &'static str,
    old_sha: Option<String>,
}

enum Built {
    Touch,
    Skip,
    Err(String),
    New(String, Vec<ChunkRec>),
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn build(t: &Todo) -> Built {
    let data = match std::fs::read(&t.abs) {
        Ok(d) => d,
        Err(e) => return Built::Err(format!("{}: {}", t.rel, e)),
    };
    let sha = hex(&Sha256::digest(&data));
    if t.old_sha.as_deref() == Some(sha.as_str()) {
        return Built::Touch;
    }
    let mut recs = Vec::new();
    if t.kind == "pdf" {
        match pdf_pages(&t.abs) {
            Ok(pages) => {
                for (i, page) in pages.iter().enumerate() {
                    for (s, e, text) in chunk_lines(&split_lines(page)) {
                        recs.push(make_rec(t.kind, s, e, Some(i as i64 + 1), text));
                    }
                }
            }
            Err(m) => return Built::Err(format!("{}: {}", t.rel, m)),
        }
    } else {
        if data.iter().take(8192).any(|&b| b == 0) {
            return Built::Skip;
        }
        let text = String::from_utf8_lossy(&data);
        if text.len() > 200_000 {
            let n = text.chars().count();
            let nl = text.bytes().filter(|&b| b == b'\n').count();
            if n > 200_000 && (n as f64) / ((nl + 1) as f64) > 300.0 {
                return Built::Skip;
            }
        }
        for (s, e, text) in chunk_lines(&split_lines(&text)) {
            recs.push(make_rec(t.kind, s, e, None, text));
        }
    }
    Built::New(sha, recs)
}

/// Cheap token estimate for a result: about 4 characters per token plus fixed overhead for the metadata.
fn est_tokens(item: &Json) -> usize {
    let text = item["text"].as_str().unwrap_or("").chars().count();
    let path = item["path"].as_str().unwrap_or("").chars().count();
    (text + path + 40) / 4
}

/// Keep leading results whose estimated tokens fit in `budget`; always keep the first, clipped if it alone is too big.
fn apply_budget(results: Vec<Json>, budget: usize) -> Vec<Json> {
    if budget == 0 || results.is_empty() {
        return results;
    }
    let mut out: Vec<Json> = Vec::new();
    let mut used = 0usize;
    for mut r in results {
        let mut cost = est_tokens(&r);
        if !out.is_empty() && used + cost > budget {
            break;
        }
        if out.is_empty() && cost > budget {
            let path_len = r["path"].as_str().unwrap_or("").chars().count();
            let room = (budget * 4).saturating_sub(path_len + 40).max(80);
            let mut kept: Vec<&str> = Vec::new();
            let mut size = 0usize;
            let text = r["text"].as_str().unwrap_or("").to_string();
            for ln in text.split('\n') {
                let l = ln.chars().count();
                if !kept.is_empty() && size + l + 1 > room {
                    break;
                }
                kept.push(ln);
                size += l + 1;
            }
            r["text"] = json!(kept.join("\n"));
            cost = est_tokens(&r);
        }
        out.push(r);
        used += cost;
    }
    out
}

/// Parameters for [`Store::search`].
pub struct SearchOpts<'a> {
    pub k: usize,
    pub mode: &'a str,
    pub path: Option<&'a str>,
    pub kinds: &'a [String],
    pub per_file: usize,
    pub lines: usize,
    pub budget: usize,
}

impl Default for SearchOpts<'_> {
    fn default() -> Self {
        SearchOpts {
            k: 10,
            mode: "hybrid",
            path: None,
            kinds: &[],
            per_file: 3,
            lines: 12,
            budget: 0,
        }
    }
}

struct VecCache {
    generation: Option<String>,
    ids: Vec<i64>,
    paths: Vec<String>,
    kinds: Vec<String>,
    mat: Vec<i8>,
}

pub struct Store {
    pub path: PathBuf,
    conn: Connection,
    cache: RefCell<Option<VecCache>>,
}

impl Store {
    pub fn open(path: &Path, create: bool) -> Result<Store> {
        if !path.exists() {
            if !create {
                bail!("no index found at {} (run `tinymem index` first)", path.display());
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
                let gi = parent.join(".gitignore");
                if !gi.exists() {
                    std::fs::write(gi, "*\n")?;
                }
            }
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(30))?;
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        let store = Store {
            path: path.to_path_buf(),
            conn,
            cache: RefCell::new(None),
        };
        match store.meta("embedder")? {
            None => {
                store.set_meta("embedder", EMBEDDER)?;
                store.set_meta("format", FORMAT)?;
                store.set_meta("generation", "0")?;
            }
            Some(e) if e != EMBEDDER => bail!("index uses embedder {:?}, this build supports {:?}", e, EMBEDDER),
            _ => {
                if store.meta("format")?.as_deref() != Some(FORMAT) {
                    bail!(
                        "index was written by an older tinymem; delete {} and re-run `tinymem index`",
                        path.parent().unwrap_or(Path::new(".")).display()
                    );
                }
            }
        }
        Ok(store)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn bump(&self) -> Result<()> {
        let g: i64 = self.meta("generation")?.and_then(|s| s.parse().ok()).unwrap_or(0);
        self.set_meta("generation", &(g + 1).to_string())
    }

    fn delete_path(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE path=?1)",
            [path],
        )?;
        self.conn.execute("DELETE FROM chunks WHERE path=?1", [path])?;
        Ok(())
    }

    fn insert_chunks(&self, path: &str, kind: &str, recs: &[ChunkRec]) -> Result<usize> {
        let base: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(id),0) FROM chunks", [], |r| r.get(0))?;
        let path_field = format!("{} {}", path, terms_of(path));
        let mut ins = self.conn.prepare_cached(
            "INSERT INTO chunks(id,path,kind,start_line,end_line,page,text,vec) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        )?;
        let mut fts = self
            .conn
            .prepare_cached("INSERT INTO chunks_fts(rowid,text,terms,path,symbols) VALUES(?1,?2,?3,?4,?5)")?;
        for (off, r) in recs.iter().enumerate() {
            let id = base + off as i64 + 1;
            ins.execute(params![
                id,
                path,
                kind,
                r.start as i64,
                r.end as i64,
                r.page,
                r.text,
                r.vec
            ])?;
            fts.execute(params![id, r.text, r.terms, path_field, r.symbols])?;
        }
        Ok(recs.len())
    }

    // ---- indexing -------------------------------------------------------
    pub fn index(&self, root: &Path, excludes: &[String], max_size: u64) -> Result<Json> {
        let root = root
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("not a directory: {}: {}", root.display(), e))?;
        if !root.is_dir() {
            bail!("not a directory: {}", root.display());
        }
        let root_s = root.to_string_lossy().into_owned();
        if let Some(prev) = self.meta("root")? {
            if prev != root_s {
                bail!("this index was built for {}; use a different --db for {}", prev, root_s);
            }
        }
        self.set_meta("root", &root_s)?;
        let t0 = Instant::now();

        let mut existing: HashMap<String, (i64, i64, String)> = HashMap::new();
        {
            let mut st = self.conn.prepare("SELECT path,size,mtime_ns,sha256 FROM files")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?;
            for row in rows {
                let (p, v) = row?;
                existing.insert(p, v);
            }
        }

        let (mut scanned, mut added, mut updated, mut unchanged, mut removed, mut skipped, mut nchunks) =
            (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
        let mut errors: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut todo: Vec<Todo> = Vec::new();

        for rel in discover(&root, excludes) {
            scanned += 1;
            if should_skip(&rel) {
                skipped += 1;
                continue;
            }
            let abs = root.join(&rel);
            let Ok(meta) = std::fs::symlink_metadata(&abs) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let kind = classify(&rel);
            let limit = if kind == "pdf" { MAX_PDF_BYTES } else { max_size };
            if meta.len() > limit {
                skipped += 1;
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            let old = existing.get(&rel);
            if let Some((osize, omtime, _)) = old {
                if *osize == meta.len() as i64 && *omtime == mtime {
                    seen.insert(rel);
                    unchanged += 1;
                    continue;
                }
            }
            todo.push(Todo {
                rel,
                abs,
                size: meta.len() as i64,
                mtime,
                kind,
                old_sha: old.map(|o| o.2.clone()),
            });
        }

        for batch in todo.chunks(128) {
            let built: Vec<Built> = batch.par_iter().map(build).collect();
            self.conn.execute_batch("BEGIN IMMEDIATE")?;
            let mut changed = false;
            let res: Result<()> = (|| {
                for (t, b) in batch.iter().zip(built) {
                    match b {
                        Built::Touch => {
                            self.conn.execute(
                                "UPDATE files SET size=?1, mtime_ns=?2 WHERE path=?3",
                                params![t.size, t.mtime, t.rel],
                            )?;
                            seen.insert(t.rel.clone());
                            unchanged += 1;
                        }
                        Built::Skip => skipped += 1,
                        Built::Err(m) => {
                            if t.old_sha.is_some() {
                                seen.insert(t.rel.clone());
                            }
                            if errors.len() < 20 {
                                errors.push(m);
                            }
                        }
                        Built::New(sha, recs) => {
                            self.delete_path(&t.rel)?;
                            let n = self.insert_chunks(&t.rel, t.kind, &recs)?;
                            self.conn.execute(
                                "INSERT INTO files(path,size,mtime_ns,sha256,kind,chunks) VALUES(?1,?2,?3,?4,?5,?6) \
                                 ON CONFLICT(path) DO UPDATE SET size=excluded.size, mtime_ns=excluded.mtime_ns, \
                                 sha256=excluded.sha256, kind=excluded.kind, chunks=excluded.chunks",
                                params![t.rel, t.size, t.mtime, sha, t.kind, n as i64],
                            )?;
                            seen.insert(t.rel.clone());
                            if t.old_sha.is_some() {
                                updated += 1;
                            } else {
                                added += 1;
                            }
                            nchunks += n;
                            changed = true;
                        }
                    }
                }
                if changed {
                    self.bump()?;
                }
                Ok(())
            })();
            match res {
                Ok(()) => self.conn.execute_batch("COMMIT")?,
                Err(e) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            }
        }

        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let gone: Vec<&String> = existing.keys().filter(|p| !seen.contains(*p)).collect();
        for rel in &gone {
            self.delete_path(rel)?;
            self.conn.execute("DELETE FROM files WHERE path=?1", [rel.as_str()])?;
            removed += 1;
        }
        if removed > 0 {
            self.bump()?;
        }
        self.set_meta("indexed_at", &utc_now())?;
        self.conn.execute_batch("COMMIT")?;

        Ok(json!({
            "scanned": scanned, "added": added, "updated": updated, "unchanged": unchanged,
            "removed": removed, "skipped": skipped, "chunks": nchunks, "errors": errors,
            "seconds": (t0.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
        }))
    }

    // ---- memory ---------------------------------------------------------
    pub fn remember(&self, text: &str, tags: &[String]) -> Result<String> {
        let text = text.trim();
        if text.is_empty() {
            bail!("nothing to remember");
        }
        let body = if tags.is_empty() {
            text.to_string()
        } else {
            format!("{}\ntags: {}", text, tags.join(", "))
        };
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seed = format!("{}:{}:{}", nanos, std::process::id(), body);
        let mid = hex(&Sha256::digest(seed.as_bytes()))[..8].to_string();
        let path = format!("memory://{}", mid);
        let n = body.split('\n').count();
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let res = self
            .insert_chunks(&path, "memory", &[make_rec("memory", 1, n, None, body)])
            .and_then(|_| self.bump());
        match res {
            Ok(()) => self.conn.execute_batch("COMMIT")?,
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
        Ok(mid)
    }

    pub fn forget(&self, id: &str) -> Result<bool> {
        let path = if id.starts_with("memory://") {
            id.to_string()
        } else {
            format!("memory://{}", id)
        };
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM chunks WHERE path=?1 AND kind='memory'",
            [&path],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Ok(false);
        }
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        self.delete_path(&path)?;
        self.bump()?;
        self.conn.execute_batch("COMMIT")?;
        Ok(true)
    }

    pub fn memories(&self) -> Result<Vec<Json>> {
        let mut st = self
            .conn
            .prepare("SELECT path, text FROM chunks WHERE kind='memory' ORDER BY id")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            let (p, t) = r?;
            out.push(json!({"id": p.trim_start_matches("memory://"), "text": t}));
        }
        Ok(out)
    }

    // ---- search ---------------------------------------------------------
    fn with_vectors<T>(&self, f: impl FnOnce(&VecCache) -> T) -> Result<T> {
        let generation = self.meta("generation")?;
        let stale = match &*self.cache.borrow() {
            Some(c) => c.generation != generation,
            None => true,
        };
        if stale {
            let mut ids = Vec::new();
            let mut paths = Vec::new();
            let mut kinds = Vec::new();
            let mut mat: Vec<i8> = Vec::new();
            let mut st = self
                .conn
                .prepare("SELECT id, path, kind, vec FROM chunks ORDER BY id")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                ids.push(r.get::<_, i64>(0)?);
                paths.push(r.get::<_, String>(1)?);
                kinds.push(r.get::<_, String>(2)?);
                let blob: Vec<u8> = r.get(3)?;
                if blob.len() != DIM {
                    bail!("corrupt vector in index");
                }
                mat.extend(blob.into_iter().map(|b| b as i8));
            }
            *self.cache.borrow_mut() = Some(VecCache {
                generation,
                ids,
                paths,
                kinds,
                mat,
            });
        }
        let guard = self.cache.borrow();
        Ok(f(guard.as_ref().unwrap()))
    }

    fn lexical(&self, query: &str, pool: usize, kinds: &[String], glob: Option<&str>) -> Result<Vec<i64>> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(vec![]);
        }
        let mut sql = String::from(
            "SELECT chunks_fts.rowid FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid WHERE chunks_fts MATCH ?",
        );
        let mut args: Vec<Value> = vec![Value::Text(q)];
        if !kinds.is_empty() {
            sql.push_str(&format!(" AND c.kind IN ({})", vec!["?"; kinds.len()].join(",")));
            args.extend(kinds.iter().map(|k| Value::Text(k.clone())));
        }
        if let Some(g) = glob {
            sql.push_str(" AND c.path GLOB ?");
            args.push(Value::Text(g.to_string()));
        }
        sql.push_str(&format!(" ORDER BY bm25(chunks_fts, {}) LIMIT ?", BM25_WEIGHTS));
        args.push(Value::Integer(pool as i64));
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args.iter()), |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn semantic(&self, query: &str, pool: usize, kinds: &[String], glob: Option<&str>) -> Result<Vec<i64>> {
        let qv = embed(query);
        if qv.iter().all(|&v| v == 0) {
            return Ok(vec![]);
        }
        self.with_vectors(|c| {
            let mut scored: Vec<(f32, usize)> = Vec::new();
            for (i, row) in c.mat.as_chunks::<DIM>().0.iter().enumerate() {
                if !kinds.is_empty() && !kinds.iter().any(|k| *k == c.kinds[i]) {
                    continue;
                }
                if let Some(g) = glob {
                    if !glob_match(g, &c.paths[i]) {
                        continue;
                    }
                }
                let mut dot: i32 = 0;
                for (a, b) in row.iter().zip(qv.iter()) {
                    dot += (*a as i32) * (*b as i32);
                }
                let s = dot as f32 / (127.0 * 127.0);
                if s > SEMANTIC_MIN_SCORE {
                    scored.push((s, i));
                }
            }
            scored.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.1.cmp(&b.1))
            });
            scored.into_iter().take(pool).map(|(_, i)| c.ids[i]).collect()
        })
    }

    pub fn search(&self, query: &str, opts: &SearchOpts) -> Result<Vec<Json>> {
        let SearchOpts {
            k,
            mode,
            path,
            kinds,
            per_file,
            lines,
            budget,
        } = *opts;
        if !matches!(mode, "hybrid" | "lexical" | "semantic") {
            bail!("mode must be hybrid, lexical or semantic");
        }
        let glob = glob_pattern(path);
        let pool = (k * 10).max(80);
        let lex = if mode != "semantic" {
            self.lexical(query, pool, kinds, glob.as_deref())?
        } else {
            vec![]
        };
        let sem = if mode != "lexical" {
            self.semantic(query, pool, kinds, glob.as_deref())?
        } else {
            vec![]
        };
        let mut fused: HashMap<i64, f64> = HashMap::new();
        let mut by: HashMap<i64, Vec<&str>> = HashMap::new();
        for (r, id) in lex.iter().enumerate() {
            *fused.entry(*id).or_insert(0.0) += W_LEXICAL / (RRF_K_LEX + (r + 1) as f64);
            by.entry(*id).or_default().push("lexical");
        }
        for (r, id) in sem.iter().enumerate() {
            *fused.entry(*id).or_insert(0.0) += W_SEMANTIC / (RRF_K + (r + 1) as f64);
            by.entry(*id).or_default().push("semantic");
        }
        if fused.is_empty() {
            return Ok(vec![]);
        }
        let ids: Vec<i64> = fused.keys().copied().collect();
        type Row = (String, String, i64, i64, Option<i64>, String);
        let mut meta: HashMap<i64, Row> = HashMap::new();
        for part in ids.chunks(500) {
            let sql = format!(
                "SELECT id,path,kind,start_line,end_line,page,text FROM chunks WHERE id IN ({})",
                vec!["?"; part.len()].join(",")
            );
            let mut st = self.conn.prepare(&sql)?;
            let rows = st.query_map(params_from_iter(part.iter()), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?),
                ))
            })?;
            for row in rows {
                let (id, v) = row?;
                meta.insert(id, v);
            }
        }
        let terms = crate::embed::query_terms(query);
        if !terms.iter().any(|t| LOW_PRIORITY_QUERY_WORDS.contains(&t.as_str())) {
            for (id, (path, kind, ..)) in &meta {
                if kind != "memory" && is_low_priority(path) {
                    if let Some(score) = fused.get_mut(id) {
                        *score *= LOW_PRIORITY_FACTOR;
                    }
                }
            }
        }
        let mut order: Vec<i64> = fused.keys().copied().collect();
        order.sort_by(|a, b| {
            fused[b]
                .partial_cmp(&fused[a])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(b))
        });
        let mut per: HashMap<String, usize> = HashMap::new();
        let mut results = Vec::new();
        for id in order {
            let Some((p, kind, s, e, page, text)) = meta.get(&id) else {
                continue;
            };
            if per_file > 0 && per.get(p).copied().unwrap_or(0) >= per_file {
                continue;
            }
            *per.entry(p.clone()).or_insert(0) += 1;
            let (snip_start, snip) = make_snippet(text, *s, &terms, lines);
            let mut item = serde_json::Map::new();
            item.insert("path".into(), json!(p));
            item.insert("kind".into(), json!(kind));
            item.insert("start_line".into(), json!(s));
            item.insert("end_line".into(), json!(e));
            if let Some(pg) = page {
                item.insert("page".into(), json!(pg));
            }
            item.insert("snippet_start_line".into(), json!(snip_start));
            item.insert("text".into(), json!(snip));
            item.insert("score".into(), json!((fused[&id] * 1e5).round() / 1e5));
            item.insert("matched_by".into(), json!(by[&id]));
            results.push(Json::Object(item));
            if results.len() >= k {
                break;
            }
        }
        Ok(apply_budget(results, budget))
    }

    // ---- status ---------------------------------------------------------
    pub fn status(&self) -> Result<Json> {
        let group = |sql: &str| -> Result<serde_json::Map<String, Json>> {
            let mut st = self.conn.prepare(sql)?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            let mut m = serde_json::Map::new();
            for r in rows {
                let (k, v) = r?;
                m.insert(k, json!(v));
            }
            Ok(m)
        };
        let chunks = group("SELECT kind, COUNT(*) FROM chunks GROUP BY kind")?;
        let files = group("SELECT kind, COUNT(*) FROM files GROUP BY kind")?;
        let memories = chunks.get("memory").cloned().unwrap_or(json!(0));
        Ok(json!({
            "version": VERSION,
            "db": self.path.to_string_lossy(),
            "db_bytes": std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
            "root": self.meta("root")?,
            "embedder": self.meta("embedder")?,
            "indexed_at": self.meta("indexed_at")?,
            "files": files,
            "chunks": chunks,
            "memories": memories,
        }))
    }
}

/// Pick the window of `max_lines` lines that covers the most distinct query terms.
pub fn make_snippet(text: &str, start_line: i64, terms: &[String], max_lines: usize) -> (i64, String) {
    let lines: Vec<&str> = text.split('\n').collect();
    if max_lines == 0 || lines.len() <= max_lines {
        return (start_line, text.to_string());
    }
    let hits: Vec<HashSet<usize>> = lines
        .iter()
        .map(|l| {
            let low = l.to_lowercase();
            terms
                .iter()
                .enumerate()
                .filter(|(_, t)| low.contains(t.as_str()))
                .map(|(i, _)| i)
                .collect()
        })
        .collect();
    let (mut best_s, mut best_score) = (0usize, -1i64);
    for s in 0..=(lines.len() - max_lines) {
        let mut u: HashSet<usize> = HashSet::new();
        for h in &hits[s..s + max_lines] {
            u.extend(h.iter().copied());
        }
        if (u.len() as i64) > best_score {
            best_s = s;
            best_score = u.len() as i64;
        }
    }
    (start_line + best_s as i64, lines[best_s..best_s + max_lines].join("\n"))
}

fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}
