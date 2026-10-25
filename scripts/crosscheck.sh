#!/usr/bin/env bash
# Verifies that the Python and Rust implementations read each other's indexes and agree on results.
# usage: scripts/crosscheck.sh <path-to-rust-binary>
set -euo pipefail
RUST_BIN="${1:?usage: crosscheck.sh <rust-binary>}"
RUST_BIN="$(cd "$(dirname "$RUST_BIN")" && pwd)/$(basename "$RUST_BIN")"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
PY="python3 -m tinymem"
export PYTHONPATH="$HERE/python"
W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
mkdir -p "$W/proj/src" "$W/proj/docs"
cat > "$W/proj/src/auth.py" <<'PYEOF'
class LoginHandler:
    def verifyPassword(self, user, password):
        return bcrypt.checkpw(password, user.password_hash)
PYEOF
cat > "$W/proj/src/pay.go" <<'GOEOF'
package pay
// ChargeCustomer bills a customer's saved card through the payment gateway.
func ChargeCustomer(id string, cents int) error { return nil }
GOEOF
printf '# Ops\n\nTo roll back a release, redeploy the previous container image.\n\nMigrations: use migrate down to revert the last one.\n' > "$W/proj/docs/ops.md"

paths() { python3 -c 'import json,sys; print(",".join(r["path"]+":"+str(r["start_line"]) for r in json.load(sys.stdin)["results"]))'; }
fail=0
check() { # name, python-result, rust-result
  if [ "$2" = "$3" ]; then echo "ok   $1 -> $2"; else echo "FAIL $1: python=[$2] rust=[$3]"; fail=1; fi
}

# 1. Python writes, both read
$PY --db "$W/py.db" index "$W/proj" >/dev/null
for q in "verify a password" "chargecustomer" "revert last migration" "roll back release"; do
  a=$($PY --db "$W/py.db" search "$q" --json | paths)
  b=$("$RUST_BIN" --db "$W/py.db" search "$q" --json | paths)
  check "py-index  '$q'" "$a" "$b"
done
# 2. Rust writes, both read
"$RUST_BIN" --db "$W/rs.db" index "$W/proj" >/dev/null
for q in "verify a password" "chargecustomer" "revert last migration" "roll back release"; do
  a=$($PY --db "$W/rs.db" search "$q" --json | paths)
  b=$("$RUST_BIN" --db "$W/rs.db" search "$q" --json | paths)
  check "rust-index '$q'" "$a" "$b"
done
# 3. Memory written by one, found by the other
id=$($PY --db "$W/py.db" remember "We chose Postgres over MySQL for billing" --tag decision)
r=$("$RUST_BIN" --db "$W/py.db" search "which database for billing" --json | paths)
check "memory py->rust" "memory://$id:1" "${r%%,*}"
"$RUST_BIN" --db "$W/py.db" forget "$id"
test -z "$($PY --db "$W/py.db" search postgres --mode lexical --json | paths)" && echo "ok   forget rust->py" || { echo "FAIL forget"; fail=1; }
# 4. Incremental re-index is a no-op across implementations
n=$("$RUST_BIN" --db "$W/py.db" index "$W/proj" --json | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["added"]+d["updated"]+d["removed"])')
[ "$n" = "0" ] && echo "ok   rust re-index of python index is a no-op" || { echo "FAIL re-index changed $n files"; fail=1; }
# 5. `init` writes byte-identical files in both implementations, merging into existing configs
for impl in py rs; do
  d="$W/init-$impl"; mkdir -p "$d/.cursor"
  printf '# Notes\n\nKeep this.\n' > "$d/CLAUDE.md"
  echo '{"mcpServers":{"other":{"command":"x"}},"keep":[1,2]}' > "$d/.mcp.json"
  echo '{}' > "$d/.cursor/mcp.json"
done
(cd "$W/init-py" && $PY init --agent all --no-index >/dev/null)
(cd "$W/init-rs" && "$RUST_BIN" init --agent all --no-index >/dev/null)
if diff -r "$W/init-py" "$W/init-rs" >/dev/null; then echo "ok   init output identical"; else echo "FAIL init output differs"; fail=1; fi
# 6. --budget trims identically
a=$($PY --db "$W/py.db" search "verify a password" --budget 20 --json | paths)
b=$("$RUST_BIN" --db "$W/py.db" search "verify a password" --budget 20 --json | paths)
check "budget" "$a" "$b"
exit $fail
