import json

import pytest

from conftest import FIXTURES, invalid_fixtures, valid_fixtures
from quillwork_oracle import expect


def test_generator_is_deterministic_and_matches_committed(generated):
    committed = sorted(p.relative_to(FIXTURES) for p in FIXTURES.rglob("*") if p.is_file())
    fresh = sorted(p.relative_to(generated) for p in generated.rglob("*") if p.is_file())
    assert committed == fresh
    stale = [str(r) for r in committed if (FIXTURES / r).read_bytes() != (generated / r).read_bytes()]
    assert stale == [], "re-run corpus/synthetic/gen_fixtures.py and commit"


def test_fixture_set():
    assert len(valid_fixtures()) == 18
    assert len(invalid_fixtures()) == 6
    for f in valid_fixtures() + invalid_fixtures():
        assert expect.sidecar(f).exists(), f


@pytest.mark.parametrize("docx", valid_fixtures() + invalid_fixtures(), ids=lambda p: p.name)
def test_text_oracle_matches_expectations(docx):
    assert expect.check(docx) == []


def test_check_reports_mismatch():
    f = FIXTURES / "08-tracked-changes.docx"
    exp = json.loads(expect.sidecar(f).read_text(encoding="utf-8"))
    exp["body"]["accept"][0] += " (tampered)"
    exp["revisions"]["authors"] = ["Nobody"]
    problems = expect.check(f, exp)
    assert any(p.startswith("body[accept]") for p in problems)
    assert any(p.startswith("revisions") for p in problems)
