import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from tinymem import core  # noqa: E402
from tinymem.core import Store  # noqa: E402
from tinymem.init import init_project  # noqa: E402
from tinymem.mcp import Server  # noqa: E402

GOLDEN = json.loads((pathlib.Path(__file__).resolve().parents[2] / "testdata" / "golden.json").read_text())

MINIMAL_PDF = b"""%PDF-1.4
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
"""


def have_pdf_extractor():
    try:
        import pypdf  # noqa: F401
        return True
    except ImportError:
        return shutil.which("pdftotext") is not None


class Golden(unittest.TestCase):
    def test_embed(self):
        for g in GOLDEN["embed"]:
            self.assertEqual(core.embed(g["text"]), g["vec"], g["text"])

    def test_split_word(self):
        for g in GOLDEN["split_word"]:
            self.assertEqual(core.split_word(g["word"]), g["parts"], g["word"])

    def test_terms_and_fts(self):
        for g in GOLDEN["terms_of"]:
            self.assertEqual(core.terms_of(g["text"]), g["terms"])
        for g in GOLDEN["fts_query"]:
            self.assertEqual(core.fts_query(g["query"]), g["fts"])

    def test_symbols_and_priority(self):
        for g in GOLDEN["symbols"]:
            self.assertEqual(core.symbols_of(g["kind"], g["text"]), g["symbols"])
        for g in GOLDEN["low_priority"]:
            self.assertEqual(core.is_low_priority(g["path"]), g["low"], g["path"])

    def test_chunks(self):
        for g in GOLDEN["chunks"]:
            got = [[s, e] for s, e, _ in core.chunk_lines(g["lines"])]
            self.assertEqual(got, g["chunks"])


class Engine(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.root = pathlib.Path(self.tmp) / "proj"
        (self.root / "src").mkdir(parents=True)
        (self.root / "docs").mkdir()
        (self.root / "node_modules" / "x").mkdir(parents=True)
        (self.root / "src" / "auth.py").write_text(
            "class LoginHandler:\n    def verifyPassword(self, user, password):\n        return bcrypt.checkpw(password, user.hash)\n")
        (self.root / "src" / "pay.go").write_text("package pay\n\nfunc ChargeCustomer(id string) error { return nil }\n")
        (self.root / "docs" / "ops.md").write_text("# Ops\n\nTo roll back a release redeploy the previous image.\n")
        (self.root / "node_modules" / "x" / "junk.js").write_text("function verifyPassword() {}\n")
        (self.root / "blob.bin").write_bytes(b"\0\1\2verifyPassword")
        self.store = Store(self.root / ".tinymem" / "index.db", create=True)

    def tearDown(self):
        self.store.close()
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_index_and_search(self):
        st = self.store.index(self.root)
        self.assertEqual(st["added"], 3)
        self.assertEqual(self.store.search("verify password")[0]["path"], "src/auth.py")
        self.assertEqual(self.store.search("chargecustomer")[0]["path"], "src/pay.go")  # camelCase subword
        self.assertEqual(self.store.search("rolling back releases")[0]["path"], "docs/ops.md")  # stemming
        for mode in ("lexical", "semantic"):
            self.assertTrue(self.store.search("verifyPassword", mode=mode))
        paths = {r["path"] for r in self.store.search("verifyPassword", k=20)}
        self.assertNotIn("node_modules/x/junk.js", paths)
        self.assertNotIn("blob.bin", paths)

    def test_awkward_inputs_do_not_crash(self):
        src = self.root / "src"
        (src / "crlf.py").write_bytes(b'def crlf_marker():\r\n    return 1\r\n')
        (src / "uni.rs").write_text("fn gr\u00f6\u00dfe() { /* \u65e5\u672c\u8a9e */ }\n", encoding="utf-8")
        (src / "empty.py").write_text("")
        (src / "min.js").write_text("var a=" + "1," * 50000 + "0;\n")
        if hasattr(os, "symlink"):
            try:
                os.symlink(".", src / "loop")
                os.symlink("/nonexistent", src / "dangling.py")
            except OSError:
                pass
        self.store.index(self.root)
        self.assertEqual(self.store.search("crlf_marker")[0]["path"], "src/crlf.py")
        self.assertTrue(self.store.search("gr\u00f6\u00dfe"))
        for q in ["", "   ", "!!!", '"', "*", "AND OR NOT", "NEAR(a b)", "a" * 5000]:
            self.assertIsInstance(self.store.search(q), list)
        self.assertEqual(self.store.search("!!!"), [])

    def test_filters(self):
        self.store.index(self.root)
        self.assertEqual({r["path"] for r in self.store.search("release image", path="docs")}, {"docs/ops.md"})
        self.assertEqual(self.store.search("verifyPassword", kinds=["doc"]), [])

    def test_incremental(self):
        self.store.index(self.root)
        again = self.store.index(self.root)
        self.assertEqual((again["added"], again["updated"], again["unchanged"]), (0, 0, 3))
        (self.root / "src" / "pay.go").write_text("package pay\n\nfunc RefundOrder() {}\n")
        (self.root / "docs" / "ops.md").unlink()
        st = self.store.index(self.root)
        self.assertEqual((st["updated"], st["removed"]), (1, 1))
        self.assertEqual(self.store.search("refundorder")[0]["path"], "src/pay.go")
        self.assertEqual(self.store.search("chargecustomer", mode="lexical"), [])
        self.assertEqual(self.store.search("roll back release", mode="lexical"), [])

    def test_touch_without_change_does_not_reindex(self):
        self.store.index(self.root)
        p = self.root / "src" / "pay.go"
        p.write_text(p.read_text())
        os.utime(p, None)
        self.assertEqual(self.store.index(self.root)["updated"], 0)

    def test_wrong_root_rejected(self):
        self.store.index(self.root)
        other = pathlib.Path(self.tmp) / "other"
        other.mkdir()
        with self.assertRaises(core.TinymemError):
            self.store.index(other)

    def test_memory(self):
        self.store.index(self.root)
        mid = self.store.remember("We chose Postgres over MySQL for the billing service", ["decision"])
        top = self.store.search("which database for billing")[0]
        self.assertEqual(top["path"], "memory://" + mid)
        self.assertEqual(top["kind"], "memory")
        self.store.index(self.root)  # re-index must not delete memories
        self.assertEqual(len(self.store.memories()), 1)
        self.assertTrue(self.store.forget(mid))
        self.assertFalse(self.store.forget(mid))
        self.assertEqual(self.store.search("postgres", mode="lexical"), [])

    def test_git_respects_gitignore(self):
        if shutil.which("git") is None:
            self.skipTest("git not installed")
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        (self.root / ".gitignore").write_text("secret.py\n")
        (self.root / "secret.py").write_text("def verifyPassword_secret(): pass\n")
        self.store.index(self.root)
        paths = {r["path"] for r in self.store.search("verifyPassword_secret", k=20)}
        self.assertNotIn("secret.py", paths)
        self.assertIn("src/auth.py", paths)

    def test_pdf(self):
        if not have_pdf_extractor():
            self.skipTest("no PDF extractor available")
        (self.root / "docs" / "policy.pdf").write_bytes(MINIMAL_PDF)
        st = self.store.index(self.root)
        self.assertEqual(st["errors"], [])
        r = self.store.search("refund policy enterprise")[0]
        self.assertEqual((r["path"], r["kind"], r["page"]), ("docs/policy.pdf", "pdf", 1))

    def test_corrupt_pdf_is_reported_cleanly(self):
        if not have_pdf_extractor():
            self.skipTest("no PDF extractor available")
        (self.root / "docs" / "broken.pdf").write_bytes(b"not really a pdf\n" * 20)
        st = self.store.index(self.root)
        self.assertEqual(st["added"], 3)  # the good files still indexed
        self.assertEqual(len(st["errors"]), 1)
        self.assertIn("broken.pdf", st["errors"][0])
        self.assertNotIn("\n", st["errors"][0])

    def test_concurrent_writers_do_not_hit_database_locked(self):
        import threading

        for i in range(30):
            (self.root / "src" / ("m%d.py" % i)).write_text("def func_%d(): return %d\n" % (i, i))
        self.store.index(self.root)
        errors = []

        def indexer():
            st = Store(self.root / ".tinymem" / "index.db")
            try:
                for n in range(6):
                    (self.root / "src" / ("m%d.py" % n)).write_text("def func_%d(): return %d  # %s\n" % (n, n, threading.get_ident()))
                    st.index(self.root)
            except Exception as exc:  # noqa: BLE001
                errors.append(repr(exc))
            finally:
                st.close()

        def noter():
            st = Store(self.root / ".tinymem" / "index.db")
            try:
                for n in range(15):
                    st.remember("note %d about widgets" % n)
            except Exception as exc:  # noqa: BLE001
                errors.append(repr(exc))
            finally:
                st.close()

        threads = [threading.Thread(target=f) for f in (indexer, indexer, noter, noter)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(errors, [])
        self.assertEqual(len(self.store.memories()), 30)

    def test_tests_and_examples_rank_below_real_code(self):
        (self.root / "src" / "retry.py").write_text("def retry_backoff(fn):\n    return fn()\n")
        (self.root / "tests").mkdir()
        (self.root / "tests" / "test_retry.py").write_text("def test_retry_backoff():\n    retry_backoff(lambda: 1)\n    retry_backoff(lambda: 2)\n")
        self.store.index(self.root)
        self.assertEqual(self.store.search("retry backoff")[0]["path"], "src/retry.py")
        # asking about tests lifts the penalty
        self.assertEqual(self.store.search("retry backoff test")[0]["path"], "tests/test_retry.py")

    def test_definitions_outrank_mentions(self):
        (self.root / "src" / "defs.py").write_text("def parse_duration(text):\n    return text\n")
        (self.root / "src" / "uses.py").write_text("# parse_duration parse_duration parse_duration\nvalue = 1  # parse_duration\n")
        self.store.index(self.root)
        self.assertEqual(self.store.search("parse_duration")[0]["path"], "src/defs.py")
        (self.root / "docs" / "guide.md").write_text("# Rate limiting\n\nSome unrelated prose about queues.\n")
        self.store.index(self.root)
        self.assertEqual(self.store.search("rate limiting")[0]["path"], "docs/guide.md")  # headings are symbols

    def test_digit_splitting_in_queries(self):
        (self.root / "src" / "hashing.go").write_text("package h\n\n// Sum256 returns the SHA-256 checksum of the data.\nfunc Sum256(b []byte) {}\n")
        self.store.index(self.root)
        for q in ("sha256 checksum", "sha 256 checksum", "SHA-256 checksum"):
            self.assertEqual(self.store.search(q, mode="lexical")[0]["path"], "src/hashing.go", q)

    def test_old_format_index_is_rejected_with_a_clear_message(self):
        self.store.close()
        import sqlite3

        db = sqlite3.connect(str(self.root / ".tinymem" / "index.db"))
        db.execute("DELETE FROM meta WHERE key='format'")
        db.commit()
        db.close()
        with self.assertRaises(core.TinymemError) as cm:
            Store(self.root / ".tinymem" / "index.db")
        self.assertIn("older tinymem", str(cm.exception))

    def test_budget_caps_results_and_keeps_the_first(self):
        for i in range(12):
            (self.root / "src" / ("w%d.py" % i)).write_text("def widget_%d():\n    return 'widget gadget %d'\n" % (i, i) + "# padding line\n" * 20)
        self.store.index(self.root)
        full = self.store.search("widget gadget", k=10, per_file=1)
        small = self.store.search("widget gadget", k=10, per_file=1, budget=150)
        self.assertGreater(len(full), len(small))
        self.assertGreaterEqual(len(small), 1)
        self.assertEqual(small[0]["path"], full[0]["path"])
        tiny = self.store.search("widget gadget", k=10, budget=5)
        self.assertEqual(len(tiny), 1)  # the best hit is never dropped, only clipped

    def test_init_merges_configs_and_is_idempotent(self):
        (self.root / ".mcp.json").write_text('{"mcpServers": {"other": {"command": "x"}}, "keep": 1}')
        (self.root / "CLAUDE.md").write_text("# Mine\n\nKeep me.\n")
        first = dict(init_project(self.root, ["claude", "cursor"]))
        self.assertEqual(first[".mcp.json"], "updated")
        self.assertEqual(first[".cursor/mcp.json"], "created")
        self.assertEqual(first["AGENTS.md"], "created")
        cfg = json.loads((self.root / ".mcp.json").read_text())
        self.assertEqual(cfg["keep"], 1)
        self.assertIn("other", cfg["mcpServers"])
        self.assertEqual(cfg["mcpServers"]["tinymem"], {"command": "tinymem", "args": ["serve"]})
        md = (self.root / "CLAUDE.md").read_text()
        self.assertTrue(md.startswith("# Mine\n\nKeep me."))
        self.assertEqual(md.count("tinymem:start"), 1)
        again = dict(init_project(self.root, ["claude", "cursor"]))
        self.assertEqual(set(again.values()), {"unchanged"})
        # a stale block is replaced in place, not duplicated
        (self.root / "CLAUDE.md").write_text(md.replace("Use it instead", "OLD TEXT instead"))
        dict(init_project(self.root, ["claude"]))
        self.assertEqual((self.root / "CLAUDE.md").read_text().count("tinymem:start"), 1)
        self.assertNotIn("OLD TEXT", (self.root / "CLAUDE.md").read_text())

    def test_init_refuses_to_clobber_invalid_json_and_dry_run_writes_nothing(self):
        (self.root / ".mcp.json").write_text("{not json")
        with self.assertRaises(core.TinymemError):
            init_project(self.root, ["claude"])
        self.assertEqual((self.root / ".mcp.json").read_text(), "{not json")
        (self.root / ".mcp.json").unlink()
        init_project(self.root, ["all"], dry_run=True)
        self.assertFalse((self.root / ".mcp.json").exists())
        self.assertFalse((self.root / "AGENTS.md").exists())
        with self.assertRaises(core.TinymemError):
            init_project(self.root, ["nope"])

    def test_mcp(self):
        self.store.index(self.root)
        srv = Server(self.store, refresh_interval=0)
        init = srv.handle({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})
        self.assertEqual(init["result"]["serverInfo"]["name"], "tinymem")
        tools = srv.handle({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})["result"]["tools"]
        self.assertEqual({t["name"] for t in tools}, {"search", "remember", "forget", "reindex", "status"})
        out = srv.handle({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                          "params": {"name": "search", "arguments": {"query": "verify password", "k": 2}}})
        body = json.loads(out["result"]["content"][0]["text"])
        self.assertEqual(body["results"][0]["path"], "src/auth.py")
        bad = srv.handle({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "nope"}})
        self.assertTrue(bad["result"]["isError"])
        self.assertIsNone(srv.handle({"jsonrpc": "2.0", "method": "notifications/initialized"}))


if __name__ == "__main__":
    unittest.main()
