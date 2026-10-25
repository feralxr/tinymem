use crate::chunk::chunk_lines;
use crate::embed::{embed, fts_query, is_low_priority, split_word, symbols_of, terms_of};
use crate::store::{SearchOpts, Store};
use serde_json::Value;
use std::path::PathBuf;

fn golden() -> Value {
    serde_json::from_str(include_str!("../../testdata/golden.json")).unwrap()
}

#[test]
fn golden_embeddings() {
    for g in golden()["embed"].as_array().unwrap() {
        let text = g["text"].as_str().unwrap();
        let want: Vec<i8> = g["vec"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap() as i8)
            .collect();
        assert_eq!(embed(text), want, "embedding mismatch for {:?}", text);
    }
}

#[test]
fn golden_tokenisation() {
    let g = golden();
    for c in g["split_word"].as_array().unwrap() {
        let want: Vec<String> = c["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().into())
            .collect();
        assert_eq!(split_word(c["word"].as_str().unwrap()), want);
    }
    for c in g["terms_of"].as_array().unwrap() {
        assert_eq!(terms_of(c["text"].as_str().unwrap()), c["terms"].as_str().unwrap());
    }
    for c in g["fts_query"].as_array().unwrap() {
        assert_eq!(fts_query(c["query"].as_str().unwrap()), c["fts"].as_str().unwrap());
    }
}

#[test]
fn golden_symbols_and_priority() {
    let g = golden();
    for c in g["symbols"].as_array().unwrap() {
        let got = symbols_of(c["kind"].as_str().unwrap(), c["text"].as_str().unwrap());
        assert_eq!(
            got,
            c["symbols"].as_str().unwrap(),
            "symbols mismatch for {:?}",
            c["text"]
        );
    }
    for c in g["low_priority"].as_array().unwrap() {
        assert_eq!(
            is_low_priority(c["path"].as_str().unwrap()),
            c["low"].as_bool().unwrap(),
            "{}",
            c["path"]
        );
    }
}

#[test]
fn golden_chunks() {
    for c in golden()["chunks"].as_array().unwrap() {
        let lines: Vec<String> = c["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().into())
            .collect();
        let got: Vec<(usize, usize)> = chunk_lines(&lines).iter().map(|(s, e, _)| (*s, *e)).collect();
        let want: Vec<(usize, usize)> = c["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_u64().unwrap() as usize, p[1].as_u64().unwrap() as usize))
            .collect();
        assert_eq!(got, want);
    }
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tinymem-test-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn engine_index_search_memory() {
    let root = tmpdir("engine").join("proj");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
    std::fs::write(
        root.join("src/auth.py"),
        "class LoginHandler:\n    def verifyPassword(self, user, password):\n        return bcrypt.checkpw(password, user.hash)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/pay.go"),
        "package pay\n\nfunc ChargeCustomer(id string) error { return nil }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("docs/ops.md"),
        "# Ops\n\nTo roll back a release redeploy the previous image.\n",
    )
    .unwrap();
    std::fs::write(root.join("node_modules/x/junk.js"), "function verifyPassword() {}\n").unwrap();
    std::fs::write(root.join("blob.bin"), b"\0\x01\x02verifyPassword").unwrap();

    let store = Store::open(&root.join(".tinymem/index.db"), true).unwrap();
    let st = store.index(&root, &[], 2 * 1024 * 1024).unwrap();
    assert_eq!(st["added"], 3);
    let top = |q: &str| -> String {
        store.search(q, &SearchOpts::default()).unwrap()[0]["path"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(top("verify password"), "src/auth.py");
    assert_eq!(top("chargecustomer"), "src/pay.go");
    assert_eq!(top("rolling back releases"), "docs/ops.md");

    let again = store.index(&root, &[], 2 * 1024 * 1024).unwrap();
    assert_eq!(
        (
            again["added"].as_u64(),
            again["updated"].as_u64(),
            again["unchanged"].as_u64()
        ),
        (Some(0), Some(0), Some(3))
    );

    std::fs::write(root.join("src/pay.go"), "package pay\n\nfunc RefundOrder() {}\n").unwrap();
    std::fs::remove_file(root.join("docs/ops.md")).unwrap();
    let st = store.index(&root, &[], 2 * 1024 * 1024).unwrap();
    assert_eq!((st["updated"].as_u64(), st["removed"].as_u64()), (Some(1), Some(1)));
    assert_eq!(top("refundorder"), "src/pay.go");
    assert!(store
        .search(
            "chargecustomer",
            &SearchOpts {
                mode: "lexical",
                ..Default::default()
            }
        )
        .unwrap()
        .is_empty());

    let id = store
        .remember(
            "We chose Postgres over MySQL for the billing service",
            &["decision".into()],
        )
        .unwrap();
    let hit = &store
        .search("which database for billing", &SearchOpts::default())
        .unwrap()[0];
    assert_eq!(hit["path"].as_str().unwrap(), format!("memory://{}", id));
    store.index(&root, &[], 2 * 1024 * 1024).unwrap();
    assert_eq!(store.memories().unwrap().len(), 1);
    assert!(store.forget(&id).unwrap());
    assert!(!store.forget(&id).unwrap());
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

const MINIMAL_PDF: &[u8] = b"%PDF-1.4
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj
3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 200]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj
4 0 obj<</Length 63>>stream
BT /F1 14 Tf 20 100 Td (Quarterly refund policy for enterprise customers) Tj ET
endstream
endobj
5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj
trailer<</Root 1 0 R/Size 6>>
%%EOF
";

#[test]
fn pdf_pages_are_indexed_and_corrupt_pdfs_are_reported() {
    let root = tmpdir("pdf").join("proj");
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/policy.pdf"), MINIMAL_PDF).unwrap();
    std::fs::write(root.join("docs/broken.pdf"), b"not really a pdf\n".repeat(20)).unwrap();
    std::fs::write(root.join("docs/ok.md"), "# Notes\n\nplain text\n").unwrap();

    let store = Store::open(&root.join(".tinymem/index.db"), true).unwrap();
    let st = store.index(&root, &[], 2 * 1024 * 1024).unwrap();
    assert_eq!(st["added"], 2, "policy.pdf and ok.md should index: {}", st);
    let errors = st["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "only broken.pdf should be reported: {:?}", errors);
    assert!(errors[0].as_str().unwrap().contains("broken.pdf"));

    let hit = &store
        .search("refund policy enterprise", &SearchOpts::default())
        .unwrap()[0];
    assert_eq!(hit["path"], "docs/policy.pdf");
    assert_eq!(hit["kind"], "pdf");
    assert_eq!(hit["page"], 1);
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn concurrent_writers_do_not_hit_database_locked() {
    let root = tmpdir("concurrent").join("proj");
    std::fs::create_dir_all(root.join("src")).unwrap();
    for i in 0..30 {
        std::fs::write(
            root.join(format!("src/m{}.py", i)),
            format!("def func_{}(): return {}\n", i, i),
        )
        .unwrap();
    }
    let db = root.join(".tinymem/index.db");
    Store::open(&db, true)
        .unwrap()
        .index(&root, &[], 2 * 1024 * 1024)
        .unwrap();

    let mut handles = Vec::new();
    for t in 0..2 {
        let (db, root) = (db.clone(), root.clone());
        handles.push(std::thread::spawn(move || {
            let store = Store::open(&db, false).unwrap();
            for n in 0..6 {
                std::fs::write(
                    root.join(format!("src/m{}.py", n)),
                    format!("def func_{}(): return {} # {}{}\n", n, n, t, n),
                )
                .unwrap();
                store.index(&root, &[], 2 * 1024 * 1024).unwrap();
            }
        }));
    }
    for t in 0..2 {
        let db = db.clone();
        handles.push(std::thread::spawn(move || {
            let store = Store::open(&db, false).unwrap();
            for n in 0..15 {
                store
                    .remember(&format!("note {} from {} about widgets", n, t), &[])
                    .unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(Store::open(&db, false).unwrap().memories().unwrap().len(), 30);
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

fn proj(name: &str) -> (PathBuf, Store) {
    let root = tmpdir(name).join("proj");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    let store = Store::open(&root.join(".tinymem/index.db"), true).unwrap();
    (root, store)
}

fn top(store: &Store, q: &str, mode: &str) -> String {
    let opts = SearchOpts {
        mode,
        ..Default::default()
    };
    store.search(q, &opts).unwrap()[0]["path"].as_str().unwrap().to_string()
}

#[test]
fn tests_and_examples_rank_below_real_code() {
    let (root, store) = proj("prior");
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(root.join("src/retry.py"), "def retry_backoff(fn):\n    return fn()\n").unwrap();
    std::fs::write(
        root.join("tests/test_retry.py"),
        "def test_retry_backoff():\n    retry_backoff(lambda: 1)\n    retry_backoff(lambda: 2)\n",
    )
    .unwrap();
    store.index(&root, &[], 2 * 1024 * 1024).unwrap();
    assert_eq!(top(&store, "retry backoff", "hybrid"), "src/retry.py");
    assert_eq!(top(&store, "retry backoff test", "hybrid"), "tests/test_retry.py");
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn definitions_outrank_mentions_and_headings_are_symbols() {
    let (root, store) = proj("defs");
    std::fs::write(root.join("src/defs.py"), "def parse_duration(text):\n    return text\n").unwrap();
    std::fs::write(
        root.join("src/uses.py"),
        "# parse_duration parse_duration parse_duration\nvalue = 1  # parse_duration\n",
    )
    .unwrap();
    std::fs::write(
        root.join("docs/guide.md"),
        "# Rate limiting\n\nSome unrelated prose about queues.\n",
    )
    .unwrap();
    store.index(&root, &[], 2 * 1024 * 1024).unwrap();
    assert_eq!(top(&store, "parse_duration", "hybrid"), "src/defs.py");
    assert_eq!(top(&store, "rate limiting", "hybrid"), "docs/guide.md");
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn digit_splitting_in_queries() {
    let (root, store) = proj("digits");
    std::fs::write(
        root.join("src/hashing.go"),
        "package h\n\n// Sum256 returns the SHA-256 checksum of the data.\nfunc Sum256(b []byte) {}\n",
    )
    .unwrap();
    store.index(&root, &[], 2 * 1024 * 1024).unwrap();
    for q in ["sha256 checksum", "sha 256 checksum", "SHA-256 checksum"] {
        assert_eq!(top(&store, q, "lexical"), "src/hashing.go", "{}", q);
    }
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn old_format_index_is_rejected_with_a_clear_message() {
    let (root, store) = proj("format");
    drop(store);
    let db = root.join(".tinymem/index.db");
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute("DELETE FROM meta WHERE key='format'", []).unwrap();
    }
    let err = Store::open(&db, false).err().expect("old index must be rejected");
    assert!(err.to_string().contains("older tinymem"), "{}", err);
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn budget_caps_results_and_keeps_the_first() {
    let (root, store) = proj("budget");
    for i in 0..12 {
        let body = format!(
            "def widget_{i}():\n    return 'widget gadget {i}'\n{}",
            "# padding line\n".repeat(20)
        );
        std::fs::write(root.join(format!("src/w{}.py", i)), body).unwrap();
    }
    store.index(&root, &[], 2 * 1024 * 1024).unwrap();
    let run = |budget: usize, per_file: usize| {
        store
            .search(
                "widget gadget",
                &SearchOpts {
                    budget,
                    per_file,
                    ..Default::default()
                },
            )
            .unwrap()
    };
    let (full, small, tiny) = (run(0, 1), run(150, 1), run(5, 3));
    assert!(full.len() > small.len() && !small.is_empty());
    assert_eq!(small[0]["path"], full[0]["path"]);
    assert_eq!(tiny.len(), 1);
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

#[test]
fn init_merges_configs_and_is_idempotent() {
    use crate::init::init_project;
    let root = tmpdir("init");
    std::fs::write(
        root.join(".mcp.json"),
        r#"{"mcpServers": {"other": {"command": "x"}}, "keep": 1}"#,
    )
    .unwrap();
    std::fs::write(root.join("CLAUDE.md"), "# Mine\n\nKeep me.\n").unwrap();
    let agents = vec!["claude".to_string(), "cursor".to_string()];
    let first: std::collections::HashMap<_, _> = init_project(&root, &agents, false).unwrap().into_iter().collect();
    assert_eq!(first[".mcp.json"], "updated");
    assert_eq!(first[".cursor/mcp.json"], "created");
    assert_eq!(first["AGENTS.md"], "created");
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(cfg["keep"], 1);
    assert!(cfg["mcpServers"]["other"].is_object());
    assert_eq!(cfg["mcpServers"]["tinymem"]["command"], "tinymem");
    let md = std::fs::read_to_string(root.join("CLAUDE.md")).unwrap();
    assert!(md.starts_with("# Mine\n\nKeep me."));
    assert_eq!(md.matches("tinymem:start").count(), 1);
    assert!(init_project(&root, &agents, false)
        .unwrap()
        .iter()
        .all(|(_, s)| *s == "unchanged"));
    std::fs::write(root.join("CLAUDE.md"), md.replace("Use it instead", "OLD TEXT instead")).unwrap();
    init_project(&root, &agents[..1], false).unwrap();
    let md = std::fs::read_to_string(root.join("CLAUDE.md")).unwrap();
    assert_eq!(md.matches("tinymem:start").count(), 1);
    assert!(!md.contains("OLD TEXT"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn init_refuses_to_clobber_invalid_json_and_dry_run_writes_nothing() {
    use crate::init::init_project;
    let root = tmpdir("init2");
    std::fs::write(root.join(".mcp.json"), "{not json").unwrap();
    assert!(init_project(&root, &["claude".to_string()], false).is_err());
    assert_eq!(std::fs::read_to_string(root.join(".mcp.json")).unwrap(), "{not json");
    std::fs::remove_file(root.join(".mcp.json")).unwrap();
    init_project(&root, &["all".to_string()], true).unwrap();
    assert!(!root.join(".mcp.json").exists() && !root.join("AGENTS.md").exists());
    assert!(init_project(&root, &["nope".to_string()], false).is_err());
    let _ = std::fs::remove_dir_all(&root);
}
