"""Regenerate testdata/golden.json (shared by the Python and Go test suites)."""
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from tinymem import core  # noqa: E402

TEXTS = [
    "def verifyPassword(self, user, password): return bcrypt.checkpw(password, user.password_hash)",
    "How do I roll back a deployment and revert the last database migration?",
    "HTTPServer parseURL XMLHttpRequest getUserName utf8Decode snake_case_name",
    "// ChargeCustomer bills a customer's saved card through the payment gateway.",
    "über naïve café 日本語 テスト привет мир",
    "the of and a",
    "",
]
WORDS = ["getUserName", "HTTPServer", "parseURL", "XMLHttpRequest", "utf8Decode", "plain", "ABC", "iOS", "A1B2", "snake"]

LINES = ["line %d" % i if i % 7 else "" for i in range(1, 200)]
LINES2 = ["x" * 700, "y" * 700, "z" * 700, "", "tail"]

METHOD_CASES = json.load(open(pathlib.Path(__file__).resolve().parents[2] / 'testdata' / 'method_cases.json'))

CODE = {
    "go": "func ReadAll(r Reader) ([]byte, error) {\n// func commented()\nfunc (m *Mutex) Lock() {\ntype ServeMux struct {\n\treturn func(x int) {",
    "rust": "pub fn from_str<'a, T>(s: &'a str) -> Result<T>\n    pub(crate) async fn run(&self)\nimpl<T: Clone> Iterator for Worker<T> {\npub struct ShardedLock<T> {\nmacro_rules! bitflags {\npub const fn new() -> Self {\nstatic mut COUNTER: u32 = 0;\nextern \"C\" fn cb()",
    "py": "def run(args, *, capture_output=False):\n    async def fetch(self):\nclass RotatingFileHandler(BaseRotatingHandler):\nx = 1  # def not_this\n@dataclass",
    "methods": "\n".join(l for l, _ in METHOD_CASES),
    "multiline": "\n\n".join(c[0] for c in json.load(open(pathlib.Path(__file__).resolve().parents[2] / "testdata" / "multiline_cases.json"))),
    "js": "export default class Foo extends Bar {\nexport async function loadData() {\nexport interface Props {\nconst handler = async () => {}",
}
DOC = "# Getting Started\n\ntext\n## Install the CLI\nnot # a heading"
QUERIES = TEXTS + ["compute SHA-256 checksum of data", "sha256", "base 64 encode", "utf 8 decoder", "ParseDuration", "NewDecoder json", "a of the"]
PATHS = ["io/io.go", "net/http/server.go", "src/foo_test.go", "tests/helpers.py", "pkg/example_test.go", "test_utils.py",
         "app/Button.spec.tsx", "vendor/x/y.go", "docs/examples/a.md", "src/contest.go", "src/latest/main.rs", "Conftest.py", "a/conftest.py"]
LPQ = ["how does the test runner work", "parse duration", "example usage of Foo"]

out = {
    "embed": [{"text": t, "vec": core.embed(t)} for t in TEXTS],
    "split_word": [{"word": w, "parts": core.split_word(w)} for w in WORDS],
    "terms_of": [{"text": t, "terms": core.terms_of(t)} for t in TEXTS],
    "fts_query": [{"query": t, "fts": core.fts_query(t)} for t in QUERIES],
    "symbols": [{"kind": "code", "text": t, "symbols": core.symbols_of("code", t)} for t in CODE.values()]
    + [{"kind": "doc", "text": DOC, "symbols": core.symbols_of("doc", DOC)}, {"kind": "pdf", "text": DOC, "symbols": core.symbols_of("pdf", DOC)}],
    "low_priority": [{"path": p, "low": core.is_low_priority(p)} for p in PATHS],
    "chunks": [
        {"lines": l, "chunks": [[s, e] for s, e, _ in core.chunk_lines(l)]} for l in (LINES, LINES2)
    ],
}
path = pathlib.Path(__file__).resolve().parents[2] / "testdata" / "golden.json"
path.write_text(json.dumps(out, ensure_ascii=False, indent=1) + "\n")
print("wrote", path)
