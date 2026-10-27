"""``quillwork-server``: the HTTP API of DESIGN §11.1 (Starlette).

Every error answers with the spec's error envelope ``{spec_version, ok: false, error}`` and an
HTTP status from :data:`STATUS`. Bodies are raw bytes for files (``.docx`` uploads, images) and
JSON for everything else; there are no multipart forms.
"""

from __future__ import annotations

import argparse
import json
from collections.abc import AsyncIterator, Awaitable, Callable
from dataclasses import dataclass
from functools import partial
from typing import Any, Final

import anyio
from quillwork.errors import QuillworkError
from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import JSONResponse, PlainTextResponse, Response, StreamingResponse
from starlette.routing import Route

from . import __version__
from .config import Settings, add_arguments, from_args
from .core import DocumentService, envelope, make_error
from .core.types import JSON
from .tools import ORDER, ToolRunner, render_read

DOCX: Final = "application/vnd.openxmlformats-officedocument.wordprocessingml.document"

#: HTTP status per spec error code. The body always carries the full error.
STATUS: Final[dict[str, int]] = {
    "STALE_ANCHOR": 409,
    "CONFLICT": 409,
    "BAD_VERSION": 409,
    "CONFIRM_REQUIRED": 409,
    "AMBIGUOUS": 422,
    "IN_FIELD": 422,
    "BAD_PACKAGE": 422,
    "UNSUPPORTED": 422,
    "NOT_FOUND": 404,
    "INVALID_OP": 400,
    "PROTECTED": 403,
    "LIMIT": 413,
    "INVARIANT": 500,
    "INTERNAL": 500,
}


@dataclass(frozen=True, slots=True)
class Principal:
    """Who is calling. ``author`` signs their transactions (tracked-change author)."""

    id: str
    author: str


#: The auth hook: return the caller, or ``None`` to answer 401. May be sync or async.
Authenticate = Callable[[Request], "Principal | Awaitable[Principal | None] | None"]


def api_key_auth(key: str, *, author: str = "user") -> Authenticate:
    """Accept ``Authorization: Bearer <key>`` or ``X-API-Key: <key>``.

    A shared key can't tell users apart, so the author comes from ``X-Quillwork-Author`` when
    the caller sends it (a trusted host) and ``author`` otherwise.
    """
    import hmac

    def authenticate(request: Request) -> Principal | None:
        given = request.headers.get("x-api-key") or ""
        bearer = request.headers.get("authorization", "")
        if bearer.lower().startswith("bearer "):
            given = bearer[7:].strip()
        if not given or not hmac.compare_digest(given.encode(), key.encode()):
            return None
        name = request.headers.get("x-quillwork-author") or author
        return Principal("api-key", name[:200])

    return authenticate


def open_access(author: str = "user") -> Authenticate:
    """No authentication (local use only)."""

    def authenticate(request: Request) -> Principal | None:
        return Principal("anonymous", request.headers.get("x-quillwork-author") or author)

    return authenticate


Handler = Callable[[Request, Principal], Awaitable[Response]]


class _HttpError(Exception):
    def __init__(self, status: int, body: JSON) -> None:
        self.status = status
        self.body = body


class QuillworkHTTP:
    def __init__(
        self,
        service: DocumentService,
        *,
        authenticate: Authenticate | None = None,
        agent_author: str = "AI",
        sse_keepalive: float = 15.0,
    ) -> None:
        self.service = service
        self.authenticate = authenticate or open_access()
        self.runner = ToolRunner(service, author=agent_author)
        self.sse_keepalive = sse_keepalive

    def app(self) -> Starlette:
        r = self._route
        routes = [
            Route("/health", self.health, methods=["GET"]),
            Route("/documents", r(self.upload), methods=["POST"]),
            Route("/documents", r(self.list_documents), methods=["GET"]),
            Route("/documents/{id}", r(self.info), methods=["GET"]),
            Route("/documents/{id}/file", r(self.import_file), methods=["PUT"]),
            Route("/documents/{id}/view", r(self.view), methods=["GET"]),
            Route("/documents/{id}/projection", r(self.projection), methods=["GET"]),
            Route("/documents/{id}/read", r(self.read_op), methods=["POST"]),
            Route("/documents/{id}/ops", r(self.ops), methods=["POST"]),
            Route("/documents/{id}/tools/{tool}", r(self.tool), methods=["POST"]),
            Route("/documents/{id}/events", r(self.events), methods=["GET"]),
            Route("/documents/{id}/changes/{cs}/{action}", r(self.review), methods=["POST"]),
            Route("/documents/{id}/versions", r(self.versions), methods=["GET"]),
            Route("/documents/{id}/versions/{v:int}/restore", r(self.restore), methods=["POST"]),
            Route("/documents/{id}/export", r(self.export), methods=["GET"]),
            Route("/documents/{id}/assets/{asset}", r(self.asset), methods=["GET"]),
            Route("/documents/{id}/images", r(self.image), methods=["POST"]),
        ]
        return Starlette(routes=routes)

    # -- plumbing ----------------------------------------------------------------------------

    def _route(self, handler: Handler) -> Callable[[Request], Awaitable[Response]]:
        async def endpoint(request: Request) -> Response:
            try:
                found = self.authenticate(request)
                principal = await found if isinstance(found, Awaitable) else found
                if principal is None:
                    return JSONResponse(
                        {"ok": False, "error": "unauthorized"},
                        status_code=401,
                        headers={"WWW-Authenticate": "Bearer"},
                    )
                return await handler(request, principal)
            except QuillworkError as exc:
                body = envelope(exc)
                return JSONResponse(body, status_code=STATUS.get(body["error"]["code"], 400))
            except _HttpError as exc:
                return JSONResponse(exc.body, status_code=exc.status)

        return endpoint

    @staticmethod
    async def _call(fn: Callable[[], Any]) -> Any:
        return await anyio.to_thread.run_sync(fn)

    async def _body(self, request: Request, limit: int, name: str) -> bytes:
        """The raw body, refused with LIMIT (413) as soon as it passes ``limit`` bytes."""
        declared = request.headers.get("content-length")
        if declared and declared.isdigit() and int(declared) > limit:
            raise _limit(name, limit, int(declared))
        chunks: list[bytes] = []
        size = 0
        async for chunk in request.stream():
            size += len(chunk)
            if size > limit:
                raise _limit(name, limit, size)
            chunks.append(chunk)
        return b"".join(chunks)

    async def _json(self, request: Request) -> JSON:
        raw = await self._body(request, 4 * 1024 * 1024, "text_chars")
        try:
            value = json.loads(raw or b"{}")
        except json.JSONDecodeError as exc:
            raise make_error(
                "INVALID_OP",
                f"The body is not JSON: {exc.msg}.",
                version=1,
                path="",
                usage="Content-Type: application/json",
            ) from None
        if not isinstance(value, dict):
            raise make_error(
                "INVALID_OP",
                "The body must be a JSON object.",
                version=1,
                path="",
                usage="Content-Type: application/json",
            )
        return value

    @staticmethod
    def _version(request: Request) -> int | None:
        raw = request.query_params.get("version")
        if raw is None or raw == "":
            return None
        if not raw.isdigit() or int(raw) < 1:
            raise make_error(
                "INVALID_OP",
                "version must be a positive integer.",
                version=1,
                path="/version",
                usage="?version=<n>",
            )
        return int(raw)

    # -- routes ------------------------------------------------------------------------------

    async def health(self, request: Request) -> Response:
        return JSONResponse(
            {"ok": True, "version": __version__, "engine": self.service.engine.name}
        )

    async def upload(self, request: Request, who: Principal) -> Response:
        data = await self._body(request, self.service.config.max_upload_bytes, "uncompressed_bytes")
        name = (
            request.query_params.get("name") or request.headers.get("x-filename") or "document.docx"
        )
        doc_id = request.query_params.get("id") or None
        info = await self._call(
            lambda: self.service.upload(data, name=name, document_id=doc_id, author=who.author)
        )
        return JSONResponse(
            {"id": info.id, "name": info.name, "version": info.head},
            status_code=201,
            headers={"Location": f"/documents/{info.id}"},
        )

    async def list_documents(self, request: Request, who: Principal) -> Response:
        docs = await self._call(self.service.list_documents)
        return JSONResponse({"documents": [d.to_dict() for d in docs]})

    async def info(self, request: Request, who: Principal) -> Response:
        info = await self._call(lambda: self.service.info(request.path_params["id"]))
        return JSONResponse(info.to_dict())

    async def import_file(self, request: Request, who: Principal) -> Response:
        doc_id = request.path_params["id"]
        data = await self._body(request, self.service.config.max_upload_bytes, "uncompressed_bytes")
        label = request.query_params.get("label")
        record = await self._call(
            lambda: self.service.import_version(doc_id, data, author=who.author, label=label)
        )
        return JSONResponse(record.summary())

    async def view(self, request: Request, who: Principal) -> Response:
        doc_id, version = request.path_params["id"], self._version(request)
        vm = await self._call(lambda: self.service.view_model(doc_id, version=version))
        return JSONResponse(vm)

    async def projection(self, request: Request, who: Principal) -> Response:
        doc_id, version = request.path_params["id"], self._version(request)
        q = request.query_params
        budget = int(q["budget"]) if q.get("budget", "").isdigit() else None
        result = await self._call(
            lambda: self.service.read(
                doc_id,
                q.get("range") or None,
                mode=q.get("mode") or "accepted",
                budget=budget,
                version=version,
            )
        )
        if q.get("format") == "json":
            return JSONResponse(result)
        headers = {"X-Quillwork-Version": str(result.get("version"))}
        if result.get("next"):
            headers["X-Quillwork-Next"] = str(result["next"])
        return PlainTextResponse(render_read(result), headers=headers)

    async def read_op(self, request: Request, who: Principal) -> Response:
        doc_id, body = request.path_params["id"], await self._json(request)
        version = self._version(request)
        return JSONResponse(
            await self._call(lambda: self.service.read_op(doc_id, body, version=version))
        )

    async def ops(self, request: Request, who: Principal) -> Response:
        doc_id, tx = request.path_params["id"], await self._json(request)
        result = await self._call(lambda: self.service.apply(doc_id, tx, author=who.author))
        return JSONResponse(result)

    async def tool(self, request: Request, who: Principal) -> Response:
        """Run one of the 8 agent tools; the answer has the rendered text and the spec JSON."""
        doc_id, tool = request.path_params["id"], request.path_params["tool"]
        if tool not in ORDER:
            raise make_error(
                "INVALID_OP",
                f"Unknown tool {tool!r}.",
                version=1,
                path="/tool",
                usage="one of: " + ", ".join(ORDER),
            )
        args = await self._json(request)
        turn = request.headers.get("x-quillwork-turn")
        result = await self._call(lambda: self.runner.call(doc_id, tool, args, turn=turn))
        return JSONResponse(
            {"text": result.text, "result": result.structured, "is_error": result.is_error}
        )

    async def review(self, request: Request, who: Principal) -> Response:
        doc_id, cs, action = (request.path_params[k] for k in ("id", "cs", "action"))
        if action not in ("accept", "reject"):
            raise _HttpError(404, {"ok": False, "error": "not found"})
        body = await self._json(request)

        def run() -> JSON:
            base = body.get("base_version") or self.service.head(doc_id)
            tx: JSON = {"base_version": base, "ops": [{"op": action, "change_set": cs}]}
            if body.get("label"):
                tx["label"] = body["label"]
            return self.service.apply(doc_id, tx, author=who.author)

        return JSONResponse(await self._call(run))

    async def versions(self, request: Request, who: Principal) -> Response:
        doc_id = request.path_params["id"]
        after = request.query_params.get("after", "0")
        records = await self._call(
            lambda: self.service.versions(doc_id, int(after) if after.isdigit() else 0)
        )
        return JSONResponse({"versions": [r.summary() for r in records]})

    async def restore(self, request: Request, who: Principal) -> Response:
        doc_id, v = request.path_params["id"], request.path_params["v"]
        record = await self._call(lambda: self.service.restore(doc_id, v, author=who.author))
        return JSONResponse(record.summary())

    async def export(self, request: Request, who: Principal) -> Response:
        doc_id, version = request.path_params["id"], self._version(request)
        fmt = request.query_params.get("format", "docx")
        variant: Any = request.query_params.get("variant", "redline")
        if fmt != "docx":
            raise make_error(
                "UNSUPPORTED",
                f"Export to {fmt} needs the converter, which this server does not run.",
                version=version or 1,
                reason="not_implemented",
            )
        data = await self._call(
            lambda: self.service.export(doc_id, version=version, variant=variant)
        )
        name = f"{doc_id}-{variant}.docx"
        return Response(
            data,
            media_type=DOCX,
            headers={"Content-Disposition": f'attachment; filename="{name}"'},
        )

    async def asset(self, request: Request, who: Principal) -> Response:
        doc_id, aid = request.path_params["id"], request.path_params["asset"]
        version = self._version(request)
        found = await self._call(lambda: self.service.get_asset(doc_id, aid, version=version))
        headers = {
            "X-Content-Type-Options": "nosniff",
            "Cache-Control": "private, max-age=31536000, immutable",  # ids are content hashes
            # An SVG opened directly must not run script or load anything.
            "Content-Security-Policy": "default-src 'none'; style-src 'unsafe-inline'; sandbox",
        }
        return Response(found.data, media_type=found.mime, headers=headers)

    async def image(self, request: Request, who: Principal) -> Response:
        doc_id = request.path_params["id"]
        data = await self._body(request, self.service.config.max_asset_bytes, "image_bytes")
        aid = await self._call(lambda: self.service.put_asset(doc_id, data))
        return JSONResponse({"asset": aid}, status_code=201)

    async def events(self, request: Request, who: Principal) -> Response:
        """SSE: one ``patch`` event per new version, or ``reload`` when there is no patch.

        ``?since=<version>`` (or ``Last-Event-ID``) is what the viewer has; events carry the
        version they lead to as their id, so a reconnect resumes. ``?until=<version>`` ends the
        stream once that version is sent.
        """
        doc_id = request.path_params["id"]
        raw = request.query_params.get("since") or request.headers.get("last-event-id") or ""
        since = int(raw) if raw.isdigit() else await self._call(lambda: self.service.head(doc_id))
        raw_until = request.query_params.get("until", "")
        until = int(raw_until) if raw_until.isdigit() else None
        await self._call(lambda: self.service.head(doc_id))  # 404 before the stream starts

        async def stream() -> AsyncIterator[str]:
            seen = since
            yield f"event: hello\ndata: {json.dumps({'version': seen})}\n\n"
            while until is None or seen < until:
                if await request.is_disconnected():
                    return
                feed = await self._call(partial(self.service.patches_since, doc_id, seen))
                if feed.reload:
                    data = json.dumps({"version": feed.version})
                    yield f"id: {feed.version}\nevent: reload\ndata: {data}\n\n"
                else:
                    for patch in feed.patches:
                        v = patch.get("version")
                        yield f"id: {v}\nevent: patch\ndata: {json.dumps(patch)}\n\n"
                seen = max(seen, feed.version)
                if until is not None and seen >= until:
                    return
                new = await self._call(partial(self.service.wait, doc_id, seen, self.sse_keepalive))
                if new is None:
                    yield ": keep-alive\n\n"

        return StreamingResponse(
            stream(),
            media_type="text/event-stream",
            headers={"Cache-Control": "no-store", "X-Accel-Buffering": "no"},
        )


def _limit(name: str, limit: int, actual: int) -> QuillworkError:
    return make_error(
        "LIMIT",
        f"The body is {actual} bytes; the limit is {limit}.",
        version=1,
        limit={"name": name, "max": limit, "actual": actual},
    )


def create_app(
    service: DocumentService,
    *,
    authenticate: Authenticate | None = None,
    agent_author: str = "AI",
    sse_keepalive: float = 15.0,
) -> Starlette:
    """The ASGI app. ``authenticate`` is the auth hook (default: none; see :func:`api_key_auth`)."""
    return QuillworkHTTP(
        service, authenticate=authenticate, agent_author=agent_author, sse_keepalive=sse_keepalive
    ).app()


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="quillwork-server", description=__doc__)
    add_arguments(parser)
    parser.add_argument("--api-key", help="require this key (default: $QUILLWORK_API_KEY)")
    args = parser.parse_args(argv)
    settings: Settings = from_args(args)
    key = args.api_key or settings.api_key
    if not key and settings.host not in ("127.0.0.1", "localhost", "::1"):
        parser.error("refusing to listen beyond localhost without --api-key / QUILLWORK_API_KEY")
    app = create_app(
        settings.service(),
        authenticate=api_key_auth(key) if key else None,
        agent_author=settings.author,
    )
    import uvicorn

    uvicorn.run(app, host=settings.host, port=settings.port)


if __name__ == "__main__":  # pragma: no cover
    main()
