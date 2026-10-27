import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[3]
FIXTURES = REPO / "corpus" / "fixtures"
GENERATOR = REPO / "corpus" / "synthetic" / "gen_fixtures.py"


def valid_fixtures():
    return sorted(p for p in FIXTURES.iterdir() if p.suffix in (".docx", ".docm"))


def invalid_fixtures():
    return sorted((FIXTURES / "invalid").glob("*.docx"))


@pytest.fixture(scope="session")
def generated(tmp_path_factory):
    """A fresh generator run in a temp dir (the committed fixtures must match it byte for byte)."""
    out = tmp_path_factory.mktemp("fixtures")
    subprocess.run([sys.executable, str(GENERATOR), str(out)], check=True, capture_output=True)
    return out


def docxodus_available() -> bool:
    try:
        from quillwork_oracle.validate import docxodus_dir

        docxodus_dir()
        return True
    except FileNotFoundError:
        return False
