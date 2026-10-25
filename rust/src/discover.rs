//! File discovery: `git ls-files` when possible (honours .gitignore), else a directory walk.

use crate::glob::glob_match;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

pub const DOC_EXTS: &[&str] = &[".md", ".mdx", ".markdown", ".rst", ".txt", ".adoc", ".org", ".tex"];
const SKIP_EXTS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".ico", ".webp", ".tif", ".tiff", ".psd", ".zip", ".gz", ".tgz", ".bz2",
    ".xz", ".7z", ".rar", ".tar", ".jar", ".war", ".class", ".o", ".a", ".so", ".dylib", ".dll", ".exe", ".bin",
    ".pyc", ".pyo", ".whl", ".woff", ".woff2", ".ttf", ".otf", ".eot", ".mp3", ".mp4", ".mov", ".avi", ".mkv", ".wav",
    ".flac", ".ogg", ".webm", ".sqlite", ".sqlite3", ".db", ".pdb", ".map", ".svg", ".iso", ".dmg", ".parquet",
    ".onnx", ".pt", ".pth",
];
const SKIP_NAMES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Cargo.lock",
    "go.sum",
    "poetry.lock",
    "Pipfile.lock",
    "composer.lock",
    "Gemfile.lock",
    "uv.lock",
];
const ALWAYS_IGNORE: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".tinymem",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".idea",
];
const WALK_EXTRA_IGNORE: &[&str] = &[
    "dist",
    "build",
    "target",
    ".next",
    ".cache",
    "coverage",
    "vendor",
    ".gradle",
    ".terraform",
];

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i..].to_lowercase(),
        _ => String::new(),
    }
}

pub fn classify(rel: &str) -> &'static str {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let ext = ext_of(name);
    if ext == ".pdf" {
        "pdf"
    } else if DOC_EXTS.contains(&ext.as_str()) {
        "doc"
    } else {
        "code"
    }
}

pub fn should_skip(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if SKIP_NAMES.contains(&name) || name.ends_with(".min.js") || name.ends_with(".min.css") {
        return true;
    }
    SKIP_EXTS.contains(&ext_of(name).as_str())
}

fn git_files(root: &Path) -> Option<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        out.stdout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect(),
    )
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(ft) = entry.file_type() else { continue };
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", prefix, name)
        };
        if ft.is_dir() {
            if ALWAYS_IGNORE.contains(&name.as_str()) || WALK_EXTRA_IGNORE.contains(&name.as_str()) {
                continue;
            }
            walk(&entry.path(), &rel, out);
        } else {
            out.push(rel);
        }
    }
}

pub fn discover(root: &Path, excludes: &[String]) -> Vec<String> {
    let rels = git_files(root).unwrap_or_else(|| {
        let mut v = Vec::new();
        walk(root, "", &mut v);
        v
    });
    let mut out = BTreeSet::new();
    for rel in rels {
        let parts: Vec<&str> = rel.split('/').collect();
        if parts[..parts.len() - 1].iter().any(|p| ALWAYS_IGNORE.contains(p)) {
            continue;
        }
        let base = parts[parts.len() - 1];
        if excludes.iter().any(|g| glob_match(g, &rel) || glob_match(g, base)) {
            continue;
        }
        out.insert(rel);
    }
    out.into_iter().collect()
}
