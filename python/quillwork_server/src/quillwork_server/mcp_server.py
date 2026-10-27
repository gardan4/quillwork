"""``quillwork-mcp``: the 8 spec tools over MCP, plus open / list / export and document resources.

Built on the official ``mcp`` SDK's low-level :class:`mcp.server.Server`. Transports: stdio (the
default; for desktop clients) and streamable HTTP (``--transport http``).

``open_document`` sets a current document; the spec tools act on it, or on the ``document_id``
a call names (the one schema change: an optional ``document_id`` property on each spec tool).
The current document is kept per MCP session id when the transport has one; MCP 2026-07-28
connections are stateless and have none, so there it is one per server process - right for
stdio (one client), and the reason shared HTTP deployments should pass ``document_id``.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import copy
import json
from typing import Any, Final

import anyio
import mcp_types as types
from mcp.server import Server, ServerRequestContext
from quillwork.errors import QuillworkError

from . import __version__
from .config import Settings, add_arguments, from_args
from .core import DocumentInfo, DocumentService, ExportVariant, envelope, make_error
from .core.types import JSON
from .tools import (
    DATA_NOTICE,
    ORDER,
    ToolRunner,
    fence,
    load_tools,
    render_error,
    render_read,
)

URI_PREFIX: Final = "quillwork://documents/"
DOCX: Final = "application/vnd.openxmlformats-officedocument.wordprocessingml.document"

INSTRUCTIONS: Final = (
    "Quillwork edits Word documents without losing their formatting. Call open_document first "
    "(upload a .docx as base64, or open an id from list_documents), then read_document; every "
    "edit takes base_version, the version of your last read or edit. Errors say whether to "
    "retry and show the current text. " + DATA_NOTICE
)

_DOCUMENT_ID_PROP: Final[JSON] = {
    "type": ["string", "null"],
    "description": "Document to act on; null for the one open_document opened in this session.",
}

_EXTRA_TOOLS: Final[list[JSON]] = [
    {
        "name": "open_document",
        "description": (
            "Open a document for the tools in this session. Either upload a .docx "
            "(content_base64, with a file name) or open an existing document_id. Returns the "
            "document id and its version; then call read_document."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "document_id": {
                    "type": ["string", "null"],
                    "description": "An existing document (see list_documents).",
                },
                "content_base64": {
                    "type": ["string", "null"],
                    "description": "A .docx file, base64-encoded, to upload.",
                },
                "name": {"type": ["string", "null"], "description": "File name for an upload."},
            },
            "additionalProperties": False,
        },
    },
    {
        "name": "list_documents",
        "description": "List the documents on this server: id, name, current version.",
        "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
    },
    {
        "name": "export_document",
        "description": (
            "Export the document as a .docx: 'redline' keeps tracked changes (default), "
            "'clean' accepts them all, 'original' is the file as uploaded."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "document_id": _DOCUMENT_ID_PROP,
                "variant": {
                    "type": ["string", "null"],
                    "enum": ["redline", "clean", "original", None],
                },
                "version": {"type": ["integer", "null"], "minimum": 1},
            },
            "additionalProperties": False,
        },
    },
]

_READ_ONLY: Final = frozenset({"read_document", "find", "list_documents", "export_document"})


class QuillworkMCP:
    """The tool and resource handlers; :meth:`server` wires them into an SDK ``Server``."""

    def __init__(self, service: DocumentService, *, author: str | None = None) -> None:
        self.service = service
        self.runner = ToolRunner(service, author=author)
        self._current: dict[str, str] = {}
        self._tools = self._tool_list()

    # -- wiring ------------------------------------------------------------------------------

    def server(self, name: str = "quillwork") -> Server[Any]:
        return Server(
            name,
            version=__version__,
            instructions=INSTRUCTIONS,
            on_list_tools=self._list_tools,
            on_call_tool=self._call_tool,
            on_list_resources=self._list_resources,
            on_list_resource_templates=self._list_templates,
            on_read_resource=self._read_resource,
        )

    def _tool_list(self) -> list[types.Tool]:
        tools: list[types.Tool] = []
        for spec in load_tools():
            schema = copy.deepcopy(spec["parameters"])
            schema["properties"]["document_id"] = dict(_DOCUMENT_ID_PROP)
            tools.append(
                types.Tool(
                    name=spec["name"],
                    description=spec["description"],
                    input_schema=schema,
                    annotations=types.ToolAnnotations(read_only_hint=spec["name"] in _READ_ONLY),
                )
            )
        for extra in _EXTRA_TOOLS:
            tools.append(
                types.Tool(
                    name=extra["name"],
                    description=extra["description"],
                    input_schema=extra["inputSchema"],
                    annotations=types.ToolAnnotations(read_only_hint=extra["name"] in _READ_ONLY),
                )
            )
        return tools

    # -- tools -------------------------------------------------------------------------------

    async def _list_tools(
        self, ctx: ServerRequestContext[Any], params: types.PaginatedRequestParams | None
    ) -> types.ListToolsResult:
        return types.ListToolsResult(tools=self._tools)

    async def _call_tool(
        self, ctx: ServerRequestContext[Any], params: types.CallToolRequestParams
    ) -> types.CallToolResult:
        args = dict(params.arguments or {})
        try:
            if params.name in ORDER:
                document_id = self._document_id(ctx, args.pop("document_id", None))
                result = await anyio.to_thread.run_sync(
                    lambda: self.runner.call(document_id, params.name, args)
                )
                return types.CallToolResult(
                    content=[types.TextContent(type="text", text=result.text)],
                    structured_content=result.structured,
                    is_error=result.is_error,
                )
            if params.name == "open_document":
                return await self._open(ctx, args)
            if params.name == "list_documents":
                docs = await anyio.to_thread.run_sync(self.service.list_documents)
                lines = [_info_line(d) for d in docs] or ["§ no documents"]
                return _ok("\n".join(lines), {"documents": [d.to_dict() for d in docs]})
            if params.name == "export_document":
                return await self._export(ctx, args)
            raise make_error(
                "INVALID_OP",
                f"Unknown tool {params.name!r}.",
                version=1,
                path="/name",
                usage="one of: " + ", ".join(t.name for t in self._tools),
            )
        except QuillworkError as exc:
            return types.CallToolResult(
                content=[types.TextContent(type="text", text=render_error(exc))],
                structured_content=envelope(exc),
                is_error=True,
            )

    def _document_id(self, ctx: ServerRequestContext[Any], given: Any) -> str:
        if isinstance(given, str) and given:
            return given
        current = self._current.get(_session_key(ctx))
        if current is None:
            raise make_error(
                "INVALID_OP",
                "No document is open in this session; call open_document first.",
                version=1,
                path="/document_id",
                usage="open_document {content_base64, name} | {document_id}",
            )
        return current

    async def _open(self, ctx: ServerRequestContext[Any], args: JSON) -> types.CallToolResult:
        content = args.get("content_base64")
        document_id = args.get("document_id")
        if content:
            try:
                data = base64.b64decode(content, validate=True)
            except (binascii.Error, ValueError):
                raise make_error(
                    "INVALID_OP",
                    "content_base64 is not valid base64.",
                    version=1,
                    path="/content_base64",
                    usage="open_document {content_base64, name}",
                ) from None
            name = args.get("name") or "document.docx"
            info = await anyio.to_thread.run_sync(
                lambda: self.service.upload(data, name=name, document_id=document_id or None)
            )
        elif isinstance(document_id, str) and document_id:
            info = await anyio.to_thread.run_sync(lambda: self.service.info(document_id))
        else:
            raise make_error(
                "INVALID_OP",
                "Give content_base64 (to upload) or document_id (to open).",
                version=1,
                path="",
                usage="open_document {content_base64, name} | {document_id}",
            )
        self._current[_session_key(ctx)] = info.id
        text = _info_line(info) + "\n§ opened; call read_document to see it"
        return _ok(text, info.to_dict())

    async def _export(self, ctx: ServerRequestContext[Any], args: JSON) -> types.CallToolResult:
        document_id = self._document_id(ctx, args.get("document_id"))
        variant: ExportVariant = args.get("variant") or "redline"
        if variant not in ("redline", "clean", "original"):
            raise make_error(
                "INVALID_OP",
                f"Unknown export variant {variant!r}.",
                version=1,
                path="/variant",
                usage="variant: redline | clean | original",
            )
        version = args.get("version")
        data = await anyio.to_thread.run_sync(
            lambda: self.service.export(document_id, version=version, variant=variant)
        )
        uri = f"{URI_PREFIX}{document_id}/export/{variant}"
        blob = types.BlobResourceContents(
            uri=uri, mime_type=DOCX, blob=base64.b64encode(data).decode("ascii")
        )
        return types.CallToolResult(
            content=[
                types.TextContent(
                    type="text", text=f"§ exported {document_id} · {variant} · {len(data)} bytes"
                ),
                types.EmbeddedResource(type="resource", resource=blob),
            ],
            structured_content={
                "document_id": document_id,
                "variant": variant,
                "bytes": len(data),
                "uri": uri,
            },
        )

    # -- resources ---------------------------------------------------------------------------

    async def _list_resources(
        self, ctx: ServerRequestContext[Any], params: types.PaginatedRequestParams | None
    ) -> types.ListResourcesResult:
        docs = await anyio.to_thread.run_sync(self.service.list_documents)
        return types.ListResourcesResult(
            resources=[
                types.Resource(
                    name=d.name,
                    uri=f"{URI_PREFIX}{d.id}",
                    mime_type="text/plain",
                    description=f"{d.name} at v{d.head}: the text projection (first window)",
                )
                for d in docs
            ]
        )

    async def _list_templates(
        self, ctx: ServerRequestContext[Any], params: types.PaginatedRequestParams | None
    ) -> types.ListResourceTemplatesResult:
        t = types.ResourceTemplate
        return types.ListResourceTemplatesResult(
            resource_templates=[
                t(
                    name="projection",
                    uri_template=URI_PREFIX + "{id}",
                    mime_type="text/plain",
                    description="The document as the agent reads it (spec/projection.md)",
                ),
                t(
                    name="view-model",
                    uri_template=URI_PREFIX + "{id}/view-model",
                    mime_type="application/json",
                    description="The view model (for a viewer)",
                ),
                t(
                    name="versions",
                    uri_template=URI_PREFIX + "{id}/versions",
                    mime_type="application/json",
                    description="Version history",
                ),
                t(
                    name="export",
                    uri_template=URI_PREFIX + "{id}/export/{variant}",
                    mime_type=DOCX,
                    description="The .docx: redline, clean or original",
                ),
            ]
        )

    async def _read_resource(
        self, ctx: ServerRequestContext[Any], params: types.ReadResourceRequestParams
    ) -> types.ReadResourceResult:
        uri = str(params.uri)
        if not uri.startswith(URI_PREFIX):
            raise ValueError(f"unknown resource {uri}")
        parts = uri.removeprefix(URI_PREFIX).split("/")
        document_id, rest = parts[0], parts[1:]
        contents: types.TextResourceContents | types.BlobResourceContents
        if not rest:
            result = await anyio.to_thread.run_sync(lambda: self.service.read(document_id))
            text = fence(render_read(result), document=document_id, version=result.get("version"))
            contents = types.TextResourceContents(uri=uri, mime_type="text/plain", text=text)
        elif rest == ["view-model"]:
            vm = await anyio.to_thread.run_sync(lambda: self.service.view_model(document_id))
            contents = _json_contents(uri, vm)
        elif rest == ["versions"]:
            records = await anyio.to_thread.run_sync(lambda: self.service.versions(document_id))
            contents = _json_contents(uri, {"versions": [r.summary() for r in records]})
        elif len(rest) == 2 and rest[0] == "export" and rest[1] in ("redline", "clean", "original"):
            variant: Any = rest[1]
            data = await anyio.to_thread.run_sync(
                lambda: self.service.export(document_id, variant=variant)
            )
            contents = types.BlobResourceContents(
                uri=uri, mime_type=DOCX, blob=base64.b64encode(data).decode("ascii")
            )
        else:
            raise ValueError(f"unknown resource {uri}")
        return types.ReadResourceResult(contents=[contents])


def _session_key(ctx: ServerRequestContext[Any]) -> str:
    connection = getattr(ctx.session, "_connection", None)
    session_id = getattr(connection, "session_id", None)
    return session_id if isinstance(session_id, str) else ""


def _ok(text: str, structured: JSON) -> types.CallToolResult:
    return types.CallToolResult(
        content=[types.TextContent(type="text", text=text)], structured_content=structured
    )


def _info_line(info: DocumentInfo) -> str:
    return f'§ document {info.id} "{info.name}" · v{info.head}'


def _json_contents(uri: str, value: Any) -> types.TextResourceContents:
    return types.TextResourceContents(
        uri=uri, mime_type="application/json", text=json.dumps(value, ensure_ascii=False)
    )


def build_server(
    service: DocumentService, *, author: str | None = None, name: str = "quillwork"
) -> Server[Any]:
    """An SDK ``Server`` exposing ``service``; run it on any MCP transport."""
    return QuillworkMCP(service, author=author).server(name)


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="quillwork-mcp", description=__doc__)
    parser.add_argument("--transport", choices=("stdio", "http"), default="stdio")
    add_arguments(parser)
    args = parser.parse_args(argv)
    settings: Settings = from_args(args)
    server = build_server(settings.service(), author=settings.author)
    if args.transport == "stdio":
        anyio.run(_run_stdio, server)
        return
    if settings.host not in ("127.0.0.1", "localhost", "::1"):
        # The MCP HTTP transport here has no auth hook; put it behind one (or use the HTTP API).
        parser.error("refusing to serve MCP beyond localhost; put an authenticating proxy in front")
    import uvicorn

    app = server.streamable_http_app(host=settings.host)
    uvicorn.run(app, host=settings.host, port=settings.port)


async def _run_stdio(server: Server[Any]) -> None:
    from mcp.server.stdio import stdio_server

    async with stdio_server() as (read, write):
        await server.run(read, write, server.create_initialization_options())


if __name__ == "__main__":  # pragma: no cover
    main()
