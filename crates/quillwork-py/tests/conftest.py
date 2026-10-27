"""Shared fixtures. Run from the repo venv after `maturin develop`:

    uv venv --python 3.12 .venv && uv pip install pytest jsonschema mypy
    VIRTUAL_ENV=$PWD/.venv uvx maturin develop --uv -m crates/quillwork-py/Cargo.toml
    .venv/bin/pytest crates/quillwork-py/tests

The private corpus (real client documents) is used when present and is only ever referred to by
index: no file names, no content in test ids, assertion messages or output.
"""

from __future__ import annotations

import io
import os
import struct
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
SPEC = REPO / "spec"
SYNTHETIC = sorted((REPO / "corpus" / "synthetic").glob("*.docx"))


def private_root() -> Path | None:
    """`$QUILLWORK_PRIVATE_CORPUS`, else a `quillwork-private-corpus` folder next to an ancestor of
    the repo (same rule as the Rust tests)."""
    env = os.environ.get("QUILLWORK_PRIVATE_CORPUS")
    if env:
        d = Path(env)
        return d if d.is_dir() else None
    for a in REPO.parents:
        d = a / "quillwork-private-corpus"
        if d.is_dir():
            return d
    return None


def private_dir() -> Path | None:
    """The folder of private `.docx` files: the corpus root when it holds any, else its first
    subfolder (in name order) that does."""
    root = private_root()
    if root is None:
        return None
    if any(root.glob("*.docx")):
        return root
    return next((d for d in sorted(root.iterdir()) if d.is_dir() and any(d.glob("*.docx"))), None)


def private_docx() -> list[Path]:
    d = private_dir()
    return sorted(d.glob("*.docx")) if d else []


PRIVATE = private_docx()


def raw_entries(data: bytes) -> dict[str, bytes]:
    """Each zip entry's local header + compressed data, exactly as stored."""
    out: dict[str, bytes] = {}
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        for info in z.infolist():
            start = info.header_offset
            name_len, extra_len = struct.unpack_from("<HH", data, start + 26)
            out[info.filename] = data[start : start + 30 + name_len + extra_len + info.compress_size]
    return out


def corpus_params() -> list[tuple[str, Path]]:
    """(id, path) pairs: synthetic files by name, private files by index only."""
    out = [(f"synthetic-{p.stem}", p) for p in SYNTHETIC]
    out += [(f"private-{i}", p) for i, p in enumerate(PRIVATE)]
    return out
