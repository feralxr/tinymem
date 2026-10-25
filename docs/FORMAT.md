# Index format (format `2`, embedder `hash1024-v1`)

Both implementations produce and consume this format. `testdata/golden.json` pins the behaviour below.

## Location

`<root>/.tinymem/index.db`, a SQLite 3 database in WAL mode with FTS5 enabled. A `.gitignore` containing `*` is written next to it.

## Tables

```sql
meta(key TEXT PRIMARY KEY, value TEXT)        -- root, embedder, generation, indexed_at
files(path PK, size, mtime_ns, sha256, kind, chunks)
chunks(id PK, path, kind, start_line, end_line, page, text, vec BLOB)
chunks_fts USING fts5(text, terms, path, symbols, tokenize='porter unicode61 remove_diacritics 2')
```

`chunks_fts.rowid = chunks.id`. `kind` is one of `code`, `doc`, `pdf`, `memory`. Memories use `path = "memory://<8 hex>"`.
`meta.generation` is incremented on every change; readers use it to invalidate in-memory vector caches.
`meta.embedder` must equal `hash1024-v1` and `meta.format` must equal `2`; other values are rejected with a message asking the user to delete `.tinymem/` and re-index.

## Tokenisation

A *word* is a maximal run of Unicode letters or digits (categories L\*, Nd, Nl, No). `_` and punctuation separate words.

`split_word(w)`: if `w` has no uppercase letter, return `[lower(w)]`. Otherwise classify each character as upper (U), lower-or-decimal (L) or other (O), and cut before index `i` when `cls[i] = U` and either `cls[i-1] = L`, or `cls[i-1] = U` and `cls[i+1] = L`. Parts are lower-cased.

Stopwords: `a an and are as at be by for from has have in is it its of on or that the this to was were will with`.

## Embedding

For each word: if it splits into several parts, add the whole lower-cased word as a feature. For each part with at least 2 characters that is not a stopword, add the part, add `~` + first 5 characters when the part has 6 or more characters, and append it to a sequence. Add `a b` bigram features for adjacent parts in the sequence. Feature weight: `~` stems 0.4, bigrams 0.5, others 1.0.

Each feature is hashed with FNV-1a 64 over its UTF-8 bytes. The bucket is `h mod 1024`, the sign is `-` when bit 32 is set. Features are accumulated in byte-sorted order as `sign * weight * (1 + ln(count))` in float64, L2-normalised, then quantised to `floor(v * 127 + 0.5)` clamped to `[-127, 127]` and stored as 1024 signed bytes. Similarity is the integer dot product divided by `127²`.

## Chunking

Lines are split on `\n` (CRLF and CR normalised), each clipped to 2,000 characters. A window starts at the current line and grows until it has 60 lines or would exceed 1,600 characters (always at least one line). If the window stopped early, the end moves back to just after the last blank line past the first 40% of the window. The next window starts 3 lines before the previous end. Windows that are only whitespace are dropped. PDFs are chunked per page; `page` is 1-based.

## Symbols

`chunks_fts.symbols` holds the names a chunk declares, followed by their camelCase sub-words (as in `terms`).

*Code chunks.* For each line (after trimming, skipping lines that start with `//`, `/*`, `*`, `#`, `--` or `;`) take the ASCII identifier runs `[A-Za-z0-9_]+`. Scan them left to right: a run in the *modifier* set (`pub crate super in self async unsafe extern export default abstract final public private protected internal static override virtual sealed partial inline declare readonly open data C`) is skipped; a run in the *declaration* set (`func fn def class struct enum trait interface type mod module const static union namespace protocol impl macro_rules record object function`) starts a declaration; any other run ends the scan with no symbol. `const` or `static` directly followed by another declaration keyword is skipped (`const fn`). After `func` a parenthesised receiver is skipped; after `impl` a `<...>` group is skipped and, if a later run is `for`, the name is the run after it. Leading `mut ref self dyn` are dropped. The symbol is the next run unless it is itself a declaration or modifier keyword. At most 64 distinct symbols (case-insensitive) are kept per chunk.

*Doc chunks.* Every identifier run on lines that start with `#` (after leading whitespace) is a symbol.

*Other kinds* (`pdf`, `memory`) have an empty `symbols` column.

## Search

1. Lexical: FTS5 `MATCH` on the OR of quoted query terms (at most 32), ordered by `bm25(chunks_fts, 1.0, 0.8, 4.0, 6.0)`. Query terms are, per word: the lower-cased word, its camelCase parts, and its letter/digit runs (`sha256` gives `sha` and `256`); plus the concatenation of every adjacent *letters-then-digits* pair (`sha 256` gives `sha256`). Stopwords are removed unless nothing else remains.
2. Semantic: cosine against all vectors; scores of 0.10 or less are discarded.
3. Each list is truncated to `max(10k, 80)` and fused: `score = 1.0/(20+rank_lex) + 0.2/(60+rank_sem)`.
4. Path prior: unless the query contains one of `test tests testing spec example examples sample fixture benchmark bench mock`, the fused score of every non-memory chunk on a *low-priority path* is multiplied by 0.55. A path is low priority if a directory component (case-insensitive) is one of `test tests testdata spec specs example examples benches bench benchmarks fixtures fixture __tests__ __mocks__ mocks vendor third_party thirdparty node_modules e2e`, or the file stem ends with `_test`, `.test`, `_spec`, `.spec` or `_tests`, or starts with `test_` or `example`, or the name is `conftest.py` or contains `.test.` or `.spec.`.
5. Results are ordered by score then chunk id, grouped to at most `per_file` per path, and each snippet is the window of `lines` lines covering the most distinct query terms.
