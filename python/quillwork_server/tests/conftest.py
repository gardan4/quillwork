"""Shared fixtures: a service on the FakeEngine, and paths into the repository's spec/corpus."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from quillwork_server.core import DocumentService, FakeEngine, MemoryStore, ServiceConfig
from quillwork_server.core.fake import fake_bytes

REPO = Path(__file__).resolve().parents[3]
SPEC = REPO / "spec"
EXAMPLES = SPEC / "examples"
SYNTHETIC = REPO / "corpus" / "synthetic"

PARAGRAPHS = ("# Contract", "Hello world", "Second para", "Third para")


def load(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


@pytest.fixture
def service() -> DocumentService:
    return DocumentService(FakeEngine(), MemoryStore(), config=ServiceConfig(lock_timeout=5.0))


@pytest.fixture
def doc(service: DocumentService) -> str:
    return service.upload(fake_bytes(*PARAGRAPHS), name="contract.docx", document_id="doc1").id
