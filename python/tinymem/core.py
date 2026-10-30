"""Core engine for tinymem: indexing, chunking, hybrid search and memory.

The on-disk format and the hashing embedder are specified in docs/FORMAT.md and
are shared byte-for-byte with the Go implementation, so either binary can read
an index written by the other.
"""
from __future__ import annotations

import fnmatch
import hashlib
import math
import os
import re
import shutil
import sqlite3
import subprocess
import time
import uuid
from array import array
from pathlib import Path
from typing import Iterable, Optional, Sequence

try:  # optional accelerator for the vector scan
    import numpy as np
except ImportError:  # pragma: no cover - exercised when numpy is absent
    np = None

__version__ = "0.1.0"

EMBEDDER = "hash1024-v1"
FORMAT = "4"
DIM = 1024
RRF_K = 60  # rank smoothing for the semantic list
RRF_K_LEX = 20  # rank smoothing for the keyword list (smaller = top ranks matter more)
W_LEXICAL = 1.0
W_SEMANTIC = 0.2
SEMANTIC_MIN_SCORE = 0.10

BM25_WEIGHTS = (1.0, 0.8, 4.0, 6.0)  # text, terms, path, symbols
LOW_PRIORITY_FACTOR = 0.55  # score multiplier for tests, examples, fixtures and vendored code
MAX_FILE_BYTES = 2 * 1024 * 1024
MAX_PDF_BYTES = 64 * 1024 * 1024
CHUNK_MAX_CHARS = 1600
CHUNK_MAX_LINES = 60
CHUNK_OVERLAP = 3
LINE_CLIP = 2000


class TinymemError(Exception):
    """User-facing error."""


# --------------------------------------------------------------------------
# Tokenisation and the hashing embedder
# --------------------------------------------------------------------------

_WORD = re.compile(r"[^\W_]+")
STOPWORDS = frozenset(
    "a an and are as at be by for from has have in is it its of on or that the this to was were will with".split()
)


def split_word(w: str) -> list[str]:
    """Split an identifier on camelCase / acronym boundaries, lower-cased."""
    if not any(c.isupper() for c in w):
        return [w.lower()]
    cls = ["U" if c.isupper() else ("L" if (c.islower() or c.isdecimal()) else "O") for c in w]
    n = len(w)
    parts: list[str] = []
    start = 0
    for i in range(1, n):
        if cls[i] == "U" and (cls[i - 1] == "L" or (cls[i - 1] == "U" and i + 1 < n and cls[i + 1] == "L")):
            parts.append(w[start:i].lower())
            start = i
    parts.append(w[start:].lower())
    return parts


_DIGITS = frozenset("0123456789")
_IDENT = frozenset("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_")
_DECL = frozenset(
    "func fn def class struct enum trait interface type mod module const static union namespace protocol impl "
    "macro_rules record object function".split()
)
_MODIFIERS = frozenset(
    "pub crate super in self async unsafe extern export default abstract final public private protected internal "
    "static override virtual sealed partial inline declare readonly open data C".split()
)
MAX_SYMBOLS = 64


def _ident_tokens(line: str):
    """ASCII identifier runs of *line* as (token, start, end)."""
    out = []
    i, n = 0, len(line)
    while i < n:
        if line[i] in _IDENT:
            j = i + 1
            while j < n and line[j] in _IDENT:
                j += 1
            out.append((line[i:j], i, j))
            i = j
        else:
            i += 1
    return out


def _skip_group(line: str, pos: int, open_c: str, close_c: str) -> int:
    """If line[pos:] (after spaces) starts with *open_c*, return the index after its matching close, else pos."""
    n = len(line)
    while pos < n and line[pos] == " ":
        pos += 1
    if pos >= n or line[pos] != open_c:
        return pos
    depth = 0
    while pos < n:
        c = line[pos]
        if c == open_c:
            depth += 1
        elif c == close_c:
            depth -= 1
            if depth == 0:
                return pos + 1
        pos += 1
    return n


_CONTROL = frozenset(
    "if else for while switch catch try finally return do new throw await yield typeof delete case default with elif "
    "match loop unsafe go defer select sizeof using lock synchronized fixed foreach when guard repeat assert in of "
    "constructor".split()
)
_HEAD_CHARS = _IDENT | frozenset(" <>[],.:&*")


def _strip_annotations(s: str) -> str:
    """Drop leading ``@Name`` / ``@Name(...)`` annotations (Java, C#-style attributes on the same line)."""
    while s.startswith("@"):
        i = 1
        while i < len(s) and (s[i] in _IDENT or s[i] == "."):
            i += 1
        if i == 1:
            return s
        s = s[_skip_group(s, i, "(", ")") :].lstrip()
    return s


def method_symbol(line: str, allow_bare: bool = False):
    """The name of a method/function whose signature is a single line ending in ``{``.

    Covers declarations without a leading keyword (JS/TS class methods, Java, C#, C, C++, PHP). Deliberately strict:
    the first parenthesis group must close on the line and be followed only by an optional return type, and
    control-flow keywords, calls on objects and callbacks never qualify.
    """
    s = _strip_annotations(line.strip())
    if not s.endswith("{") or s[0] not in _IDENT or s[0] in _DIGITS:
        return None
    p = s.find("(")
    if p <= 0:
        return None
    end = _skip_group(s, p, "(", ")")
    if end >= len(s):
        return None
    body = s[end:-1].strip()
    if body and not body.startswith((":", "->", "throws ", "const", "override", "final", "noexcept", "where ")):
        return None
    prefix = s[:p].rstrip()
    j = len(prefix)
    while j > 0 and prefix[j - 1] in _IDENT:
        j -= 1
    name = prefix[j:]
    if not name or name[0] in _DIGITS or (j > 0 and prefix[j - 1] == "."):
        return None
    head = prefix[:j]
    if any(c not in _HEAD_CHARS for c in head):
        return None
    head_toks = _ident_tokens(head)
    if len(head_toks) > 6:
        return None
    first = _ident_tokens(s)[0][0]
    if first in _CONTROL or name in _CONTROL or name in _DECL:
        return None
    if not allow_bare and not head_toks and line[:1] not in (" ", "\t") and s[p + 1 : end - 1].strip():
        return None  # unindented bare ``name(args) {`` is more likely a call than a declaration
    return name


def line_symbol(line: str):
    """The name declared by *line* (func/class/fn/struct/def/type/method/...), or None."""
    s = line.strip()
    if not s or s.startswith(("//", "/*", "*", "#", "--", ";")):
        return None
    return method_symbol(line) or _keyword_symbol(s)


def _keyword_symbol(s: str):
    toks = _ident_tokens(s)
    for idx, (tok, a, b) in enumerate(toks):
        if tok in _DECL:
            nxt = toks[idx + 1][0] if idx + 1 < len(toks) else ""
            if tok in ("const", "static") and nxt in _DECL:
                continue
            pos = b
            if tok == "func":  # Go method receiver: func (r *T) Name(
                pos = _skip_group(s, pos, "(", ")")
            elif tok == "impl":  # Rust: impl<T> Trait for Type
                pos = _skip_group(s, pos, "<", ">")
            rest = [t for t in toks if t[1] >= pos]
            if tok == "impl":
                names = [t[0] for t in rest]
                if "for" in names:
                    rest = rest[names.index("for") + 1 :]
            while rest and rest[0][0] in ("mut", "ref", "self", "dyn"):
                rest = rest[1:]
            if not rest or rest[0][0] in _DECL or rest[0][0] in _MODIFIERS:
                return None
            if tok in ("static", "const") and rest[0][0] in _C_TYPES:
                return None  # ``static int`` is the start of a C declaration, not a name
            return rest[0][0]
        if tok not in _MODIFIERS:
            return None
    return None


_C_TYPES = frozenset("int long short char void float double signed unsigned bool boolean byte auto".split())
_COMMENT_STARTS = ("//", "/*", "*", "#", "--", ";")
_MAX_SIG_LINES = 8


def _multiline_symbol(lines, i):
    """A method whose signature spans several lines, or whose ``{`` sits alone on the next line."""
    raw = lines[i]
    s = _strip_annotations(raw.strip())
    if not s or s[0] not in _IDENT or s[0] in _DIGITS or "(" not in s or s.endswith((";", "}")):
        return None
    joined, j = s, i
    depth = joined.count("(") - joined.count(")")
    while depth > 0 and j - i < _MAX_SIG_LINES and j + 1 < len(lines):
        j += 1
        t = lines[j].strip()
        if not t or t.startswith(_COMMENT_STARTS):
            continue
        joined += " " + t
        depth = joined.count("(") - joined.count(")")
    if depth != 0:
        return None
    indent = raw[:1] if raw[:1] in (" ", "\t") else ""
    if joined.endswith("{"):
        return method_symbol(indent + joined) if j > i else None
    k = j + 1
    while k < len(lines) and not lines[k].strip():
        k += 1
    if k < len(lines) and lines[k].strip() == "{":
        return method_symbol(indent + joined + " {", allow_bare=True)
    return None


def declared_names(text: str) -> list:
    """Distinct declared names of a code chunk, in order (at most MAX_SYMBOLS)."""
    names: list = []
    seen: set = set()
    lines = text.split("\n")
    for i, ln in enumerate(lines):
        name = line_symbol(ln)
        if name is None:
            t = ln.strip()
            if t and not t.startswith(_COMMENT_STARTS):
                name = _multiline_symbol(lines, i)
        if name and name.lower() not in seen:
            seen.add(name.lower())
            names.append(name)
        if len(names) >= MAX_SYMBOLS:
            break
    return names


def symbols_of(kind: str, text: str) -> str:
    """Declared names in a chunk plus their sub-words, space separated (FTS 'symbols' column)."""
    names: list[str] = []
    seen: set[str] = set()
    if kind == "doc":
        for ln in text.split("\n"):
            t = ln.lstrip()
            if t.startswith("#"):
                for tok, _, _ in _ident_tokens(t):
                    if tok.lower() not in seen:
                        seen.add(tok.lower())
                        names.append(tok)
    elif kind == "code":
        names = declared_names(text)
    if not names:
        return ""
    joined = " ".join(names)
    sub = terms_of(joined)
    return joined + (" " + sub if sub else "")


def terms_of(text: str) -> str:
    """Sub-words of every multi-part identifier, space separated (FTS 'terms' column)."""
    out: list[str] = []
    for w in _WORD.findall(text):
        parts = split_word(w)
        if len(parts) > 1:
            out.extend(parts)
    return " ".join(out)


def features(text: str) -> dict[str, int]:
    counts: dict[str, int] = {}
    seq: list[str] = []
    for w in _WORD.findall(text):
        parts = split_word(w)
        if len(parts) > 1:
            whole = w.lower()
            counts[whole] = counts.get(whole, 0) + 1
        for p in parts:
            if len(p) < 2 or p in STOPWORDS:
                continue
            counts[p] = counts.get(p, 0) + 1
            if len(p) >= 6:
                key = "~" + p[:5]
                counts[key] = counts.get(key, 0) + 1
            seq.append(p)
    for a, b in zip(seq, seq[1:]):
        key = a + " " + b
        counts[key] = counts.get(key, 0) + 1
    return counts


def _fnv1a64(data: bytes) -> int:
    h = 0xCBF29CE484222325
    for c in data:
        h ^= c
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def _weight(key: str) -> float:
    if key.startswith("~"):
        return 0.4
    if " " in key:
        return 0.5
    return 1.0


def embed(text: str) -> list[int]:
    """Return the DIM-wide int8 hashing embedding of *text*."""
    vec = [0.0] * DIM
    feats = features(text)
    for key in sorted(feats):  # fixed order: float addition is not associative
        count = feats[key]
        h = _fnv1a64(key.encode("utf-8"))
        sign = 1.0 if ((h >> 32) & 1) == 0 else -1.0
        vec[h % DIM] += sign * _weight(key) * (1.0 + math.log(count))
    norm2 = 0.0
    for v in vec:  # explicit left fold (builtin sum() is compensated on 3.12+)
        norm2 += v * v
    norm = math.sqrt(norm2)
    if norm == 0.0:
        return [0] * DIM
    out = []
    for v in vec:
        q = int(math.floor(v / norm * 127.0 + 0.5))
        out.append(max(-127, min(127, q)))
    return out


def embed_blob(text: str) -> bytes:
    return bytes(q & 0xFF for q in embed(text))


# --------------------------------------------------------------------------
# Chunking
# --------------------------------------------------------------------------

def chunk_lines(lines: Sequence[str]):
    """Yield (start_line, end_line, text) with 1-based inclusive line numbers."""
    n = len(lines)
    i = 0
    while i < n:
        j = i
        size = 0
        while j < n:
            ln = len(lines[j]) + 1
            if j > i and (j - i >= CHUNK_MAX_LINES or size + ln > CHUNK_MAX_CHARS):
                break
            size += ln
            j += 1
        end = j
        if end < n:  # a limit was hit: prefer to cut after a blank line
            lower = i + (end - i) * 2 // 5
            k = end - 1
            while k > lower:
                if lines[k].strip() == "":
                    end = k + 1
                    break
                k -= 1
        text = "\n".join(lines[i:end])
        if text.strip():
            yield i + 1, end, text
        if end >= n:
            break
        i = end - CHUNK_OVERLAP if end - CHUNK_OVERLAP > i else end


def split_lines(text: str) -> list[str]:
    return [ln[:LINE_CLIP] for ln in text.replace("\r\n", "\n").replace("\r", "\n").split("\n")]


# --------------------------------------------------------------------------
# File discovery
# --------------------------------------------------------------------------

DOC_EXTS = {".md", ".mdx", ".markdown", ".rst", ".txt", ".adoc", ".org", ".tex"}
SKIP_EXTS = set(
    ".png .jpg .jpeg .gif .bmp .ico .webp .tif .tiff .psd .zip .gz .tgz .bz2 .xz .7z .rar .tar .jar .war "
    ".class .o .a .so .dylib .dll .exe .bin .pyc .pyo .whl .woff .woff2 .ttf .otf .eot .mp3 .mp4 .mov .avi "
    ".mkv .wav .flac .ogg .webm .sqlite .sqlite3 .db .pdb .map .svg .iso .dmg .parquet .onnx .pt .pth".split()
)
SKIP_NAMES = {
    "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "Cargo.lock", "go.sum", "poetry.lock",
    "Pipfile.lock", "composer.lock", "Gemfile.lock", "uv.lock",
}
ALWAYS_IGNORE = {
    ".git", ".hg", ".svn", ".tinymem", "node_modules", "__pycache__", ".venv", "venv", ".tox",
    ".mypy_cache", ".pytest_cache", ".ruff_cache", ".idea",
}
WALK_IGNORE = ALWAYS_IGNORE | {"dist", "build", "target", ".next", ".cache", "coverage", "vendor", ".gradle", ".terraform"}


LOW_PRIORITY_DIRS = frozenset(
    "test tests testdata spec specs example examples benches bench benchmarks fixtures fixture __tests__ "
    "__mocks__ mocks vendor third_party thirdparty node_modules e2e".split()
)
LOW_PRIORITY_QUERY_WORDS = frozenset("test tests testing spec example examples sample fixture benchmark bench mock".split())


def is_low_priority(path: str) -> bool:
    """True for paths an agent rarely wants first: tests, examples, fixtures, vendored code."""
    parts = path.lower().split("/")
    if any(p in LOW_PRIORITY_DIRS for p in parts[:-1]):
        return True
    name = parts[-1]
    stem = name.rsplit(".", 1)[0]
    return (
        stem.endswith(("_test", ".test", "_spec", ".spec", "_tests"))
        or stem.startswith("test_")
        or name in ("conftest.py",)
        or ".test." in name
        or ".spec." in name
        or stem.startswith("example")
    )


def classify(rel: str) -> str:
    ext = os.path.splitext(rel)[1].lower()
    if ext == ".pdf":
        return "pdf"
    if ext in DOC_EXTS:
        return "doc"
    return "code"


def should_skip(rel: str) -> bool:
    name = rel.rsplit("/", 1)[-1]
    if name in SKIP_NAMES or name.endswith((".min.js", ".min.css")):
        return True
    return os.path.splitext(name)[1].lower() in SKIP_EXTS


def _git_files(root: Path) -> Optional[list[str]]:
    if shutil.which("git") is None:
        return None
    try:
        r = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-co", "--exclude-standard", "-z"],
            capture_output=True, timeout=120,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if r.returncode != 0:
        return None
    return [p for p in r.stdout.decode("utf-8", "surrogateescape").split("\0") if p]


def _walk(root: Path) -> list[str]:
    out: list[str] = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in WALK_IGNORE]
        rel_dir = os.path.relpath(dirpath, root)
        for f in filenames:
            rel = f if rel_dir == "." else (rel_dir + "/" + f).replace(os.sep, "/")
            out.append(rel)
    return out


def discover(root: Path, excludes: Iterable[str] = ()) -> list[str]:
    rels = _git_files(root)
    if rels is None:
        rels = _walk(root)
    excludes = list(excludes)
    out = []
    for rel in set(rels):
        parts = rel.split("/")
        if any(p in ALWAYS_IGNORE for p in parts[:-1]):
            continue
        if excludes and any(fnmatch.fnmatchcase(rel, g) or fnmatch.fnmatchcase(parts[-1], g) for g in excludes):
            continue
        out.append(rel)
    out.sort()
    return out


# --------------------------------------------------------------------------
# Extraction
# --------------------------------------------------------------------------

class PdfUnavailable(Exception):
    pass


def extract_pdf_pages(path: Path) -> list[str]:
    try:
        import logging

        import pypdf  # type: ignore

        logging.getLogger("pypdf").setLevel(logging.CRITICAL)  # corrupt PDFs are reported once, by us
        try:
            reader = pypdf.PdfReader(str(path))
            return [(p.extract_text() or "") for p in reader.pages]
        except Exception:
            pass  # fall through to pdftotext
    except ImportError:
        pass
    exe = shutil.which("pdftotext")
    if exe:
        r = subprocess.run([exe, "-layout", "-enc", "UTF-8", str(path), "-"], capture_output=True, timeout=180)
        if r.returncode == 0:
            pages = r.stdout.decode("utf-8", "replace").split("\f")
            if pages and pages[-1].strip() == "":
                pages.pop()
            return pages
        err = r.stderr.decode("utf-8", "replace").strip().splitlines()
        raise TinymemError("could not read PDF: " + (err[0][:160] if err else "pdftotext failed"))
    raise PdfUnavailable()


def looks_binary(data: bytes) -> bool:
    return b"\0" in data[:8192]


def looks_minified(text: str) -> bool:
    return len(text) > 200_000 and len(text) / (text.count("\n") + 1) > 300


# --------------------------------------------------------------------------
# Store
# --------------------------------------------------------------------------

SCHEMA = """
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
"""


def find_db(start: Optional[Path] = None) -> Optional[Path]:
    """Walk upwards from *start* looking for .tinymem/index.db."""
    cur = (start or Path.cwd()).resolve()
    for d in [cur, *cur.parents]:
        cand = d / ".tinymem" / "index.db"
        if cand.is_file():
            return cand
    return None


def glob_pattern(p: Optional[str]) -> Optional[str]:
    if not p:
        return None
    return p if any(c in p for c in "*?[") else "*" + p + "*"


def _alpha_digit_parts(w: str) -> list[str]:
    """'sha256' -> ['sha', '256']; words that are all letters or all digits yield nothing."""
    runs: list[str] = []
    cur = ""
    for c in w:
        if cur and ((c in _DIGITS) != (cur[-1] in _DIGITS)):
            runs.append(cur)
            cur = ""
        cur += c
    if cur:
        runs.append(cur)
    return runs if len(runs) > 1 else []


def fts_query(q: str) -> str:
    seen: set[str] = set()
    terms: list[str] = []

    def add(t: str):
        if len(t) >= 2 and t not in seen:
            seen.add(t)
            terms.append(t)

    words = _WORD.findall(q)
    for w in words:
        parts = split_word(w)
        add(w.lower())
        if len(parts) > 1:
            for t in parts:
                add(t)
        for t in _alpha_digit_parts(w.lower()):
            add(t)
    for a, b in zip(words, words[1:]):  # "sha 256" -> "sha256", "base 64" -> "base64"
        if a.isalpha() and b and all(c in _DIGITS for c in b):
            add(a.lower() + b)
    non_stop = [t for t in terms if t not in STOPWORDS]
    terms = non_stop or terms
    return " OR ".join('"%s"' % t for t in terms[:32])


def query_terms(q: str) -> list[str]:
    out = []
    for w in _WORD.findall(q.lower()):
        if len(w) >= 2 and w not in STOPWORDS and w not in out:
            out.append(w)
    return out


def est_tokens(item: dict) -> int:
    """Cheap token estimate for a result: about 4 characters per token plus fixed overhead for the metadata."""
    return (len(item["text"]) + len(item["path"]) + 40) // 4


def apply_budget(results: list[dict], budget: int) -> list[dict]:
    """Keep leading results whose estimated tokens fit in *budget*; always keep the first, clipped if it alone is too big."""
    if budget <= 0 or not results:
        return results
    out: list[dict] = []
    used = 0
    for r in results:
        cost = est_tokens(r)
        if out and used + cost > budget:
            break
        if not out and cost > budget:
            room = max(budget * 4 - len(r["path"]) - 40, 80)
            lines = r["text"].split("\n")
            kept, size = [], 0
            for ln in lines:
                if kept and size + len(ln) + 1 > room:
                    break
                kept.append(ln)
                size += len(ln) + 1
            r = dict(r, text="\n".join(kept))
            cost = est_tokens(r)
        out.append(r)
        used += cost
    return out


def make_snippet(text: str, start_line: int, terms: Sequence[str], max_lines: int):
    """Pick the window of *max_lines* lines that covers the most distinct query terms."""
    lines = text.split("\n")
    if max_lines <= 0 or len(lines) <= max_lines:
        return start_line, text
    hits = [{t for t in terms if t in ln.lower()} for ln in lines]
    best_s, best_score = 0, -1
    for s in range(0, len(lines) - max_lines + 1):
        score = len(set().union(*hits[s : s + max_lines]))
        if score > best_score:
            best_s, best_score = s, score
    return start_line + best_s, "\n".join(lines[best_s : best_s + max_lines])


class Store:
    def __init__(self, db_path, create: bool = False):
        self.path = Path(db_path)
        if not self.path.exists():
            if not create:
                raise TinymemError("no index found at %s (run `tinymem index` first)" % self.path)
            self.path.parent.mkdir(parents=True, exist_ok=True)
            gi = self.path.parent / ".gitignore"
            if not gi.exists():
                gi.write_text("*\n")
        self.db = sqlite3.connect(str(self.path), isolation_level=None, timeout=30)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=NORMAL")
        self.db.executescript(SCHEMA)
        emb = self.meta("embedder")
        if emb is None:
            self.set_meta("embedder", EMBEDDER)
            self.set_meta("format", FORMAT)
            self.set_meta("generation", "0")
        elif self.meta("format") != FORMAT:
            raise TinymemError("index was written by an older tinymem; delete %s and re-run `tinymem index`" % self.path.parent)
        elif emb != EMBEDDER:
            raise TinymemError("index uses embedder %r, this build supports %r" % (emb, EMBEDDER))
        self._cache = None

    # -- helpers ---------------------------------------------------------
    def close(self):
        self.db.close()

    def meta(self, key: str) -> Optional[str]:
        row = self.db.execute("SELECT value FROM meta WHERE key=?", (key,)).fetchone()
        return row[0] if row else None

    def set_meta(self, key: str, value: str):
        self.db.execute("INSERT INTO meta(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", (key, value))

    def _bump(self):
        self.set_meta("generation", str(int(self.meta("generation") or "0") + 1))

    def _delete_path(self, path: str):
        self.db.execute("DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE path=?)", (path,))
        self.db.execute("DELETE FROM chunks WHERE path=?", (path,))

    def _insert_chunks(self, path: str, kind: str, chunks) -> int:
        base = self.db.execute("SELECT COALESCE(MAX(id),0) FROM chunks").fetchone()[0]
        path_field = path + " " + terms_of(path)
        rows, fts = [], []
        for off, (start, end, page, text) in enumerate(chunks, 1):
            cid = base + off
            rows.append((cid, path, kind, start, end, page, text, embed_blob(text)))
            fts.append((cid, text, terms_of(text), path_field, symbols_of(kind, text)))
        self.db.executemany("INSERT INTO chunks(id,path,kind,start_line,end_line,page,text,vec) VALUES(?,?,?,?,?,?,?,?)", rows)
        self.db.executemany("INSERT INTO chunks_fts(rowid,text,terms,path,symbols) VALUES(?,?,?,?,?)", fts)
        return len(rows)

    # -- indexing --------------------------------------------------------
    def _build_chunks(self, abs_path: Path, rel: str, kind: str, data: bytes):
        if kind == "pdf":
            chunks = []
            for pno, page_text in enumerate(extract_pdf_pages(abs_path), 1):
                for s, e, t in chunk_lines(split_lines(page_text)):
                    chunks.append((s, e, pno, t))
            return chunks
        if looks_binary(data):
            return None
        text = data.decode("utf-8", "replace")
        if looks_minified(text):
            return None
        return [(s, e, None, t) for s, e, t in chunk_lines(split_lines(text))]

    def index(self, root, excludes: Iterable[str] = (), max_size: int = MAX_FILE_BYTES) -> dict:
        root = Path(root).resolve()
        if not root.is_dir():
            raise TinymemError("not a directory: %s" % root)
        prev = self.meta("root")
        if prev and prev != str(root):
            raise TinymemError("this index was built for %s; use a different --db for %s" % (prev, root))
        self.set_meta("root", str(root))
        t0 = time.time()
        existing = {r[0]: (r[1], r[2], r[3]) for r in self.db.execute("SELECT path,size,mtime_ns,sha256 FROM files")}
        st = dict(scanned=0, added=0, updated=0, unchanged=0, removed=0, skipped=0, chunks=0, errors=[])
        seen: set[str] = set()
        pdf_warned = False
        changed = False
        self.db.execute("BEGIN IMMEDIATE")
        try:
            batch = 0
            for rel in discover(root, excludes):
                st["scanned"] += 1
                if should_skip(rel):
                    st["skipped"] += 1
                    continue
                abs_path = root / rel
                try:
                    if abs_path.is_symlink() or not abs_path.is_file():
                        continue
                    s = abs_path.stat()
                    kind = classify(rel)
                    if s.st_size > (MAX_PDF_BYTES if kind == "pdf" else max_size):
                        st["skipped"] += 1
                        continue
                    old = existing.get(rel)
                    if old and old[0] == s.st_size and old[1] == s.st_mtime_ns:
                        seen.add(rel)
                        st["unchanged"] += 1
                        continue
                    data = abs_path.read_bytes()
                    sha = hashlib.sha256(data).hexdigest()
                    if old and old[2] == sha:
                        self.db.execute("UPDATE files SET size=?, mtime_ns=? WHERE path=?", (s.st_size, s.st_mtime_ns, rel))
                        seen.add(rel)
                        st["unchanged"] += 1
                        continue
                    try:
                        chunks = self._build_chunks(abs_path, rel, kind, data)
                    except PdfUnavailable:
                        if not pdf_warned:
                            st["errors"].append("PDF support needs `pypdf` (pip install pypdf) or the `pdftotext` tool; PDFs skipped")
                            pdf_warned = True
                        st["skipped"] += 1
                        continue
                    if chunks is None:
                        st["skipped"] += 1
                        continue
                    self._delete_path(rel)
                    n = self._insert_chunks(rel, kind, chunks)
                    self.db.execute(
                        "INSERT INTO files(path,size,mtime_ns,sha256,kind,chunks) VALUES(?,?,?,?,?,?) "
                        "ON CONFLICT(path) DO UPDATE SET size=excluded.size, mtime_ns=excluded.mtime_ns, "
                        "sha256=excluded.sha256, kind=excluded.kind, chunks=excluded.chunks",
                        (rel, s.st_size, s.st_mtime_ns, sha, kind, n),
                    )
                    seen.add(rel)
                    st["updated" if old else "added"] += 1
                    st["chunks"] += n
                    changed = True
                    batch += 1
                    if batch >= 200:
                        self.db.execute("COMMIT")
                        self.db.execute("BEGIN IMMEDIATE")
                        batch = 0
                except (OSError, TinymemError, sqlite3.Error) as exc:
                    if rel in existing:
                        seen.add(rel)  # keep the previous version rather than dropping it on a transient error
                    if len(st["errors"]) < 20:
                        st["errors"].append("%s: %s" % (rel, exc))
            for rel in set(existing) - seen:
                self._delete_path(rel)
                self.db.execute("DELETE FROM files WHERE path=?", (rel,))
                st["removed"] += 1
                changed = True
            if changed:
                self._bump()
            self.set_meta("indexed_at", time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))
            self.db.execute("COMMIT")
        except BaseException:
            self.db.execute("ROLLBACK")
            raise
        st["seconds"] = round(time.time() - t0, 3)
        return st

    # -- memory ----------------------------------------------------------
    def remember(self, text: str, tags: Sequence[str] = ()) -> str:
        text = text.strip()
        if not text:
            raise TinymemError("nothing to remember")
        body = text + ("\ntags: " + ", ".join(tags) if tags else "")
        mid = uuid.uuid4().hex[:8]
        path = "memory://" + mid
        lines = body.split("\n")
        self.db.execute("BEGIN IMMEDIATE")
        try:
            self._insert_chunks(path, "memory", [(1, len(lines), None, body)])
            self._bump()
            self.db.execute("COMMIT")
        except BaseException:
            self.db.execute("ROLLBACK")
            raise
        return mid

    def forget(self, mem_id: str) -> bool:
        path = mem_id if mem_id.startswith("memory://") else "memory://" + mem_id
        n = self.db.execute("SELECT COUNT(*) FROM chunks WHERE path=? AND kind='memory'", (path,)).fetchone()[0]
        if not n:
            return False
        self.db.execute("BEGIN IMMEDIATE")
        self._delete_path(path)
        self._bump()
        self.db.execute("COMMIT")
        return True

    def memories(self) -> list[dict]:
        rows = self.db.execute("SELECT path, text FROM chunks WHERE kind='memory' ORDER BY id").fetchall()
        return [{"id": p[len("memory://"):], "text": t} for p, t in rows]

    # -- search ----------------------------------------------------------
    def _vectors(self):
        gen = self.meta("generation")
        if self._cache and self._cache["gen"] == gen:
            return self._cache
        rows = self.db.execute("SELECT id, path, kind, vec FROM chunks ORDER BY id").fetchall()
        ids = [r[0] for r in rows]
        paths = [r[1] for r in rows]
        kinds = [r[2] for r in rows]
        blob = b"".join(r[3] for r in rows)
        if np is not None:
            mat = np.frombuffer(blob, dtype=np.int8).reshape(-1, DIM) if rows else np.zeros((0, DIM), dtype=np.int8)
        else:
            mat = array("b")
            mat.frombytes(blob)
        self._cache = {"gen": gen, "ids": ids, "paths": paths, "kinds": kinds, "mat": mat}
        return self._cache

    def _match(self, match: str, pool: int, kinds, glob) -> list[int]:
        sql = ("SELECT chunks_fts.rowid FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid "
               "WHERE chunks_fts MATCH ?")
        args: list = [match]
        if kinds:
            sql += " AND c.kind IN (%s)" % ",".join("?" * len(kinds))
            args += list(kinds)
        if glob:
            sql += " AND c.path GLOB ?"
            args.append(glob)
        sql += " ORDER BY bm25(chunks_fts, %s) LIMIT ?" % ", ".join(repr(float(w)) for w in BM25_WEIGHTS)
        args.append(pool)
        return [r[0] for r in self.db.execute(sql, args)]

    def _lexical(self, query: str, pool: int, kinds, glob) -> list[int]:
        q = fts_query(query)
        if not q:
            return []
        return self._match(q, pool, kinds, glob)

    def _semantic(self, query: str, pool: int, kinds, glob) -> list[int]:
        qv = embed(query)
        if not any(qv):
            return []
        c = self._vectors()
        n = len(c["ids"])
        if n == 0:
            return []
        scored: list[tuple[float, int]] = []
        keep = None
        if kinds or glob:
            kset = set(kinds) if kinds else None
            keep = [
                (kset is None or c["kinds"][i] in kset) and (glob is None or fnmatch.fnmatchcase(c["paths"][i], glob))
                for i in range(n)
            ]
        if np is not None:
            qf = np.asarray(qv, dtype=np.float32)
            mat = c["mat"]
            scores = np.empty(n, dtype=np.float32)
            for lo in range(0, n, 4096):
                scores[lo : lo + 4096] = mat[lo : lo + 4096].astype(np.float32) @ qf
            scores /= 127.0 * 127.0
            if keep is not None:
                scores = np.where(np.asarray(keep), scores, -1.0)
            top = np.argpartition(-scores, min(pool, n) - 1)[:pool]
            scored = [(float(scores[i]), int(i)) for i in top if scores[i] > SEMANTIC_MIN_SCORE]
        else:
            mat = c["mat"]
            for i in range(n):
                if keep is not None and not keep[i]:
                    continue
                row = mat[i * DIM : (i + 1) * DIM]
                s = sum(a * b for a, b in zip(row, qv)) / (127.0 * 127.0)
                if s > SEMANTIC_MIN_SCORE:
                    scored.append((s, i))
        scored.sort(key=lambda t: -t[0])
        return [c["ids"][i] for _, i in scored[:pool]]

    def search(self, query: str, k: int = 10, mode: str = "hybrid", path: Optional[str] = None,
               kinds: Optional[Sequence[str]] = None, per_file: int = 3, lines: int = 12, budget: int = 0) -> list[dict]:
        if mode not in ("hybrid", "lexical", "semantic"):
            raise TinymemError("mode must be hybrid, lexical or semantic")
        glob = glob_pattern(path)
        pool = max(k * 10, 80)
        lex = self._lexical(query, pool, kinds, glob) if mode in ("hybrid", "lexical") else []
        sem = self._semantic(query, pool, kinds, glob) if mode in ("hybrid", "semantic") else []
        fused: dict[int, float] = {}
        by: dict[int, list[str]] = {}
        for r, cid in enumerate(lex, 1):
            fused[cid] = fused.get(cid, 0.0) + W_LEXICAL / (RRF_K_LEX + r)
            by.setdefault(cid, []).append("lexical")
        for r, cid in enumerate(sem, 1):
            fused[cid] = fused.get(cid, 0.0) + W_SEMANTIC / (RRF_K + r)
            by.setdefault(cid, []).append("semantic")
        if not fused:
            return []
        meta = {}
        order = list(fused)
        for lo in range(0, len(order), 500):
            part = order[lo : lo + 500]
            q = "SELECT id,path,kind,start_line,end_line,page,text FROM chunks WHERE id IN (%s)" % ",".join("?" * len(part))
            for row in self.db.execute(q, part):
                meta[row[0]] = row
        if not (LOW_PRIORITY_QUERY_WORDS & set(query_terms(query))):
            for cid, row in meta.items():
                if row[2] != "memory" and is_low_priority(row[1]):
                    fused[cid] *= LOW_PRIORITY_FACTOR
        order = sorted(fused, key=lambda i: (-fused[i], i))
        terms = query_terms(query)
        per: dict[str, int] = {}
        results = []
        for cid in order:
            row = meta.get(cid)
            if row is None:
                continue
            _, p, kind, s, e, page, text = row
            if per_file and per.get(p, 0) >= per_file:
                continue
            per[p] = per.get(p, 0) + 1
            snip_start, snip = make_snippet(text, s, terms, lines)
            item = {"path": p, "kind": kind, "start_line": s, "end_line": e}
            if page is not None:
                item["page"] = page
            item.update(snippet_start_line=snip_start, text=snip, score=round(fused[cid], 5), matched_by=by[cid])
            results.append(item)
            if len(results) >= k:
                break
        return apply_budget(results, budget)

    # -- status ----------------------------------------------------------
    def status(self) -> dict:
        kinds = dict(self.db.execute("SELECT kind, COUNT(*) FROM chunks GROUP BY kind").fetchall())
        files = dict(self.db.execute("SELECT kind, COUNT(*) FROM files GROUP BY kind").fetchall())
        return {
            "version": __version__,
            "db": str(self.path),
            "db_bytes": self.path.stat().st_size,
            "root": self.meta("root"),
            "embedder": self.meta("embedder"),
            "indexed_at": self.meta("indexed_at"),
            "files": files,
            "chunks": kinds,
            "memories": kinds.get("memory", 0),
        }
