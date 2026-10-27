"""The MCP server through the SDK's in-memory client."""

from __future__ import annotations

import base64
import json
from collections.abc import AsyncIterator
from typing import Any

import mcp_types as types
import pytest
from conftest import SPEC, load
from mcp import Client

from quillwork_server.core import DocumentService
from quillwork_server.core.fake import fake_bytes
from quillwork_server.mcp_server import URI_PREFIX, build_server
from quillwork_server.tools import DATA_NOTICE, ORDER

pytestmark = pytest.mark.anyio


@pytest.fixture
def anyio_backend() -> str:
    return "asyncio"


@pytest.fixture
async def client(service: DocumentService) -> AsyncIterator[Client]:
    async with Client(build_server(service, author="Agent")) as c:
        yield c


def text_of(result: types.CallToolResult) -> str:
    first = result.content[0]
    assert isinstance(first, types.TextContent)
    return first.text


async def open_doc(client: Client, *paragraphs: str) -> str:
    data = base64.b64encode(fake_bytes(*paragraphs)).decode()
    result = await client.call_tool(
        "open_document", {"content_base64": data, "name": "c.docx", "document_id": "m1"}
    )
    assert not result.is_error, text_of(result)
    return "m1"


def edit(base: int, **op: Any) -> dict[str, Any]:
    full = {
        "anchor": None,
        "find": None,
        "occurrence": None,
        "text": None,
        "style": None,
        "target": None,
        "expect": None,
    } | op
    return {"base_version": base, "label": None, "ops": [full]}


async def test_tools_are_the_spec_tools_plus_document_tools(client: Client) -> None:
    tools = (await client.list_tools()).tools
    names = [t.name for t in tools]
    assert names == [*ORDER, "open_document", "list_documents", "export_document"]
    for tool in tools[: len(ORDER)]:
        spec = load(SPEC / "tools" / f"{tool.name}.json")
        assert tool.description == spec["description"]
        schema = dict(tool.input_schema)
        assert "document_id" in schema["properties"]
        schema["properties"] = {k: v for k, v in schema["properties"].items() if k != "document_id"}
        assert schema == spec["parameters"]
        assert tool.annotations is not None
        assert tool.annotations.read_only_hint == (tool.name in ("read_document", "find"))


async def test_instructions_carry_the_data_notice(service: DocumentService) -> None:
    async with Client(build_server(service)) as c:
        assert DATA_NOTICE in (c.instructions or "")


async def test_a_spec_tool_without_a_document_is_an_error(client: Client) -> None:
    result = await client.call_tool("read_document", {"range": None, "mode": None, "budget": None})
    assert result.is_error
    assert text_of(result).startswith("§ error INVALID_OP")
    assert result.structured_content is not None
    assert result.structured_content["error"]["path"] == "/document_id"


async def test_open_read_edit_export(client: Client, service: DocumentService) -> None:
    await open_doc(client, "# Contract", "Hello world")
    read = await client.call_tool("read_document", {"range": None, "mode": None, "budget": None})
    assert not read.is_error
    assert text_of(read).startswith('<document_data document="m1" version="1">')
    assert "[b101] Hello world" in text_of(read)

    out = await client.call_tool(
        "edit", edit(1, op="replace", anchor="b101", find="world", text="there")
    )
    assert not out.is_error, text_of(out)
    assert "§ v2 · cs_1 · 1 changed" in text_of(out)
    assert out.structured_content is not None and out.structured_content["version"] == 2
    assert service.versions("m1")[-1].author == "Agent"

    stale = await client.call_tool(
        "edit", edit(1, op="replace", anchor="b101", find="Hello", text="Hi")
    )
    assert stale.is_error
    assert "§ error STALE_ANCHOR · v2 · ops[0] replace_text · b101 · retryable" in text_of(stale)

    exported = await client.call_tool("export_document", {"variant": "clean"})
    assert not exported.is_error
    blob = exported.content[1]
    assert isinstance(blob, types.EmbeddedResource)
    assert isinstance(blob.resource, types.BlobResourceContents)
    assert base64.b64decode(blob.resource.blob) == service.export("m1", variant="clean")


async def test_document_id_argument_overrides_the_current_document(
    client: Client, service: DocumentService
) -> None:
    await open_doc(client, "first")
    service.upload(fake_bytes("other doc"), document_id="other")
    out = await client.call_tool(
        "find", {"query": "other", "regex": None, "mode": None, "document_id": "other"}
    )
    assert not out.is_error
    assert 'document="other"' in text_of(out)


async def test_list_documents_and_open_existing(client: Client, service: DocumentService) -> None:
    service.upload(fake_bytes("x"), name="x.docx", document_id="x1")
    listed = await client.call_tool("list_documents", {})
    assert "x1" in text_of(listed)
    opened = await client.call_tool("open_document", {"document_id": "x1"})
    assert not opened.is_error
    missing = await client.call_tool("open_document", {"document_id": "nope"})
    assert missing.is_error and text_of(missing).startswith("§ error NOT_FOUND")
    bad = await client.call_tool("open_document", {"content_base64": "%%%"})
    assert bad.is_error and "base64" in text_of(bad)


async def test_resources(client: Client, service: DocumentService) -> None:
    await open_doc(client, "# Contract", "Hello world")
    resources = (await client.list_resources()).resources
    assert [str(r.uri) for r in resources] == [f"{URI_PREFIX}m1"]
    templates = (await client.list_resource_templates()).resource_templates
    assert {t.name for t in templates} == {"projection", "view-model", "versions", "export"}

    projection = (await client.read_resource(f"{URI_PREFIX}m1")).contents[0]
    assert isinstance(projection, types.TextResourceContents)
    assert projection.text.startswith('<document_data document="m1" version="1">')

    vm = (await client.read_resource(f"{URI_PREFIX}m1/view-model")).contents[0]
    assert isinstance(vm, types.TextResourceContents)
    assert json.loads(vm.text)["version"] == 1

    versions = (await client.read_resource(f"{URI_PREFIX}m1/versions")).contents[0]
    assert isinstance(versions, types.TextResourceContents)
    assert [v["kind"] for v in json.loads(versions.text)["versions"]] == ["upload"]

    original = (await client.read_resource(f"{URI_PREFIX}m1/export/original")).contents[0]
    assert isinstance(original, types.BlobResourceContents)
    assert base64.b64decode(original.blob) == fake_bytes("# Contract", "Hello world")


async def test_export_rejects_an_unknown_variant(client: Client) -> None:
    await open_doc(client, "x")
    out = await client.call_tool("export_document", {"variant": "pdf"})
    assert out.is_error
    assert out.structured_content is not None
    assert out.structured_content["error"]["path"] == "/variant"
