"""mypy --strict passes on the example (and on the package with its stubs)."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path
from types import ModuleType

import pytest

from conftest import corpus_params

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent


@pytest.mark.skipif(importlib.util.find_spec("mypy") is None, reason="mypy not installed")
def test_mypy_strict_on_example() -> None:
    r = subprocess.run(
        [sys.executable, "-m", "mypy", "--strict", "--no-incremental", "examples/replace_text.py", "python/quillwork"],
        cwd=CRATE,
        capture_output=True,
        text=True,
        check=False,
    )
    assert r.returncode == 0, r.stdout + r.stderr


def test_example_runs(tmp_path: Path) -> None:
    src = CRATE.parents[1] / "corpus" / "synthetic" / "doc50.docx"
    out = tmp_path / "out.docx"
    r = subprocess.run(
        [sys.executable, str(CRATE / "examples" / "replace_text.py"), str(src), str(out), "Nieuw"],
        capture_output=True,
        text=True,
        check=False,
    )
    assert r.returncode == 0, r.stdout + r.stderr
    assert out.stat().st_size > 0 and "byte-identical" in r.stdout


def _example() -> ModuleType:
    spec = importlib.util.spec_from_file_location("replace_text", CRATE / "examples" / "replace_text.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CORPUS = corpus_params()


@pytest.mark.parametrize("tracked", [True, False], ids=["tracked", "direct"])
@pytest.mark.parametrize("path", [p for _, p in CORPUS], ids=[i for i, _ in CORPUS])
def test_example_on_corpus(path: Path, tracked: bool) -> None:
    """The example's edit + checks, in memory (no output file), on every corpus document."""
    rt = _example()
    data = path.read_bytes()
    after, part, old = rt.replace_first_text(data, " Quillwork ", tracked=tracked)
    # check() asserts; failures name entries and lengths only, never document text.
    assert rt.check(data, after, part, old, " Quillwork ", tracked=tracked) > 0
