import json

import pytest

from conftest import docxodus_available, invalid_fixtures, valid_fixtures
from quillwork_oracle import expect
from quillwork_oracle.validate import validate

pytestmark = pytest.mark.skipif(not docxodus_available(), reason="Docxodus not available")


def test_valid_fixtures_pass():
    res = validate([str(p) for p in valid_fixtures()])
    bad = {r["path"]: [e["code"] for e in r["errors"]] for r in res if not r["ok"]}
    assert bad == {}


def test_invalid_fixtures_fail_with_expected_codes():
    files = invalid_fixtures()
    res = validate([str(p) for p in files])
    for f, r in zip(files, res):
        want = json.loads(expect.sidecar(f).read_text(encoding="utf-8"))["error_codes"]
        got = {e["code"] for e in r["errors"]}
        assert not r["ok"], f.name
        assert set(want) <= got, (f.name, got)
