#!/usr/bin/env python3
"""Fail if the Python package, Rust crate and (optionally) a git tag disagree on the version."""
import pathlib
import re
import sys

root = pathlib.Path(__file__).resolve().parents[1]


def find(path, pattern):
    m = re.search(pattern, (root / path).read_text(), re.M)
    if not m:
        sys.exit("could not find a version in %s" % path)
    return m.group(1)


versions = {
    "python/pyproject.toml": find("python/pyproject.toml", r'^version = "([^"]+)"'),
    "python/tinymem/core.py": find("python/tinymem/core.py", r'^__version__ = "([^"]+)"'),
    "rust/Cargo.toml": find("rust/Cargo.toml", r'^version = "([^"]+)"'),
}
if len(sys.argv) > 1:
    versions["git tag"] = sys.argv[1].lstrip("v")
for name, v in versions.items():
    print("%-26s %s" % (name, v))
if len(set(versions.values())) != 1:
    sys.exit("version mismatch")
