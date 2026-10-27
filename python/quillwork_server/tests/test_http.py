"""The HTTP API through Starlette's test client."""

from __future__ import annotations

import json
from typing import Any

import pytest
from conftest import PARAGRAPHS
from starlette.testclient import TestClient

from quillwork_server.core import DocumentService, ServiceConfig
from quillwork_server.core.fake import FakeEngine, fake_bytes
from quillwork_server.core.store import MemoryStore
from quillwork_server.http_app import DOCX, api_key_auth, create_app

PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 32
KEY = "s3cret-key"


@pytest.fixture
def client(service: DocumentService, doc: str) -> TestClient:
    return TestClient(create_app(service, agent_author="Agent", sse_keepalive=0.05))


def tx(base: int, anchor: str, find: str, new: str) -> dict[str, Any]:
    return {
        "base_version": base,
        "ops": [{"op": "replace_text", "anchor": anchor, "find": find, "replace": new}],
    }


def sse(text: str) -> list[tuple[str, dict[str, Any]]]:
    """``(event, data)`` per SSE event; comments are skipped."""
    out = []
    for chunk in text.strip().split("\n\n"):
        fields = dict(
            line.split(": ", 1) for line in chunk.splitlines() if not line.startswith(":")
        )
        if fields:
            out.append((fields["event"], json.loads(fields["data"])))
    return out


def test_health_needs_no_auth(service: DocumentService) -> None:
    c = TestClient(create_app(service, authenticate=api_key_auth(KEY)))
    assert c.get("/health").json() == {"ok": True, "version": "0.0.3", "engine": "fake"}


def test_api_key_auth(service: DocumentService, doc: str) -> None:
    c = TestClient(create_app(service, authenticate=api_key_auth(KEY)))
    denied = c.get("/documents")
    assert denied.status_code == 401 and denied.headers["www-authenticate"] == "Bearer"
    assert c.get("/documents", headers={"Authorization": "Bearer wrong"}).status_code == 401
    assert c.get("/documents", headers={"Authorization": f"Bearer {KEY}"}).status_code == 200
    assert c.get("/documents", headers={"X-API-Key": KEY}).status_code == 200
    headers = {"X-API-Key": KEY, "X-Quillwork-Author": "Carol"}
    out = c.post(f"/documents/{doc}/ops", json=tx(1, "b101", "world", "there"), headers=headers)
    assert out.status_code == 200
    assert service.versions(doc)[-1].author == "Carol"


def test_upload_info_list(client: TestClient) -> None:
    up = client.post("/documents?name=new.docx&id=n1", content=fake_bytes("Hi"))
    assert up.status_code == 201
    assert up.json() == {"id": "n1", "name": "new.docx", "version": 1}
    assert up.headers["location"] == "/documents/n1"
    assert client.get("/documents/n1").json()["head"] == 1
    assert {d["id"] for d in client.get("/documents").json()["documents"]} == {"doc1", "n1"}
    missing = client.get("/documents/nope")
    assert missing.status_code == 404
    assert missing.json()["error"]["code"] == "NOT_FOUND"


def test_upload_limit_is_413() -> None:
    small = DocumentService(FakeEngine(), MemoryStore(), config=ServiceConfig(max_upload_bytes=10))
    c = TestClient(create_app(small))
    out = c.post("/documents", content=b"x" * 11)
    assert out.status_code == 413
    err = out.json()["error"]
    assert err["code"] == "LIMIT"
    assert err["limit"] == {"name": "uncompressed_bytes", "max": 10, "actual": 11}


def test_projection_text_and_json(client: TestClient, doc: str) -> None:
    text = client.get(f"/documents/{doc}/projection")
    assert text.status_code == 200 and text.headers["x-quillwork-version"] == "1"
    assert "[b101] Hello world" in text.text
    data = client.get(f"/documents/{doc}/projection?format=json").json()
    assert data["version"] == 1 and "Hello world" in data["text"]
    bad = client.get(f"/documents/{doc}/projection?version=x")
    assert bad.status_code == 400 and bad.json()["error"]["path"] == "/version"


def test_ops_then_stale(client: TestClient, doc: str) -> None:
    ok = client.post(f"/documents/{doc}/ops", json=tx(1, "b101", "world", "there"))
    assert ok.status_code == 200 and ok.json()["version"] == 2
    stale = client.post(f"/documents/{doc}/ops", json=tx(1, "b101", "Hello", "Hi"))
    assert stale.status_code == 409
    body = stale.json()
    assert body["ok"] is False and body["error"]["code"] == "STALE_ANCHOR"
    assert body["spec_version"]
    not_json = client.post(f"/documents/{doc}/ops", content=b"{", headers={"content-type": "x"})
    assert not_json.status_code == 400 and not_json.json()["error"]["code"] == "INVALID_OP"


def test_tool_endpoint(client: TestClient, doc: str, service: DocumentService) -> None:
    read = client.post(
        f"/documents/{doc}/tools/read_document", json={"range": None, "mode": None, "budget": None}
    )
    assert read.status_code == 200
    assert read.json()["text"].startswith('<document_data document="doc1" version="1">')
    out = client.post(
        f"/documents/{doc}/tools/find", json={"query": "Second", "regex": None, "mode": None}
    )
    assert out.json()["is_error"] is False and "b102" in out.json()["text"]
    unknown = client.post(f"/documents/{doc}/tools/nope", json={})
    assert unknown.status_code == 400 and unknown.json()["error"]["path"] == "/tool"


def test_accept_and_reject(client: TestClient, doc: str) -> None:
    client.post(f"/documents/{doc}/ops", json=tx(1, "b101", "world", "there"))
    client.post(f"/documents/{doc}/ops", json=tx(2, "b102", "Second", "2nd"))
    rejected = client.post(f"/documents/{doc}/changes/cs_2/reject", json={})
    assert rejected.status_code == 200 and rejected.json()["version"] == 4
    accepted = client.post(f"/documents/{doc}/changes/cs_1/accept", json={"label": "ok"})
    assert accepted.json()["version"] == 5
    clean = client.get(f"/documents/{doc}/projection?mode=accepted").text
    assert "Hello there" in clean and "Second para" in clean
    assert client.post(f"/documents/{doc}/changes/cs_1/frob", json={}).status_code == 404


def test_versions_and_restore(client: TestClient, doc: str) -> None:
    client.post(f"/documents/{doc}/ops", json=tx(1, "b101", "world", "there"))
    versions = client.get(f"/documents/{doc}/versions").json()["versions"]
    assert [v["version"] for v in versions] == [1, 2]
    assert "patch" not in versions[0]
    assert [
        v["version"] for v in client.get(f"/documents/{doc}/versions?after=1").json()["versions"]
    ] == [2]
    restored = client.post(f"/documents/{doc}/versions/1/restore")
    assert restored.json()["version"] == 3 and restored.json()["kind"] == "restore"


def test_export(client: TestClient, doc: str) -> None:
    out = client.get(f"/documents/{doc}/export?variant=original")
    assert out.status_code == 200
    assert out.headers["content-type"] == DOCX
    assert out.headers["content-disposition"] == 'attachment; filename="doc1-original.docx"'
    assert out.content == fake_bytes(*PARAGRAPHS)
    pdf = client.get(f"/documents/{doc}/export?format=pdf")
    assert pdf.status_code == 422 and pdf.json()["error"]["code"] == "UNSUPPORTED"


def test_import_file(client: TestClient, doc: str) -> None:
    out = client.put(f"/documents/{doc}/file?label=edited%20in%20Word", content=fake_bytes("New"))
    assert out.status_code == 200
    assert out.json()["version"] == 2 and out.json()["label"] == "edited in Word"


def test_images_and_assets(client: TestClient, doc: str) -> None:
    up = client.post(f"/documents/{doc}/images", content=PNG)
    assert up.status_code == 201
    aid = up.json()["asset"]
    got = client.get(f"/documents/{doc}/assets/{aid}")
    assert got.content == PNG and got.headers["content-type"] == "image/png"
    assert got.headers["x-content-type-options"] == "nosniff"
    assert "sandbox" in got.headers["content-security-policy"]
    assert client.get(f"/documents/{doc}/assets/nope").status_code == 404


def test_view(client: TestClient, doc: str) -> None:
    vm = client.get(f"/documents/{doc}/view").json()
    assert vm["version"] == 1


def test_events_stream_patches_then_reload(client: TestClient, doc: str) -> None:
    client.post(f"/documents/{doc}/ops", json=tx(1, "b101", "world", "there"))
    client.post(f"/documents/{doc}/ops", json=tx(2, "b102", "Second", "2nd"))
    with client.stream("GET", f"/documents/{doc}/events?since=1&until=3") as r:
        assert r.headers["content-type"].startswith("text/event-stream")
        events = sse(r.read().decode())
    assert [(e, d.get("version")) for e, d in events] == [("hello", 1), ("patch", 2), ("patch", 3)]

    client.put(f"/documents/{doc}/file", content=fake_bytes("Replaced"))
    with client.stream(
        "GET", f"/documents/{doc}/events?until=4", headers={"Last-Event-ID": "3"}
    ) as r:
        events = sse(r.read().decode())
    assert events == [("hello", {"version": 3}), ("reload", {"version": 4})]

    assert client.get("/documents/nope/events").status_code == 404
