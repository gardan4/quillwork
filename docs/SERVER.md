# Quillwork server

The reference service around the engine (DESIGN §11.1). One Python package,
`quillwork-server` (`python/quillwork_server/`), in three layers:

| Layer | Module | What it is |
|---|---|---|
| Library | `quillwork_server.core` | `DocumentService`: versions, one writer per document, undo, export, live patches. Storage, locks and the engine are interfaces. |
| Agent tools | `quillwork_server.tools` | The 8 spec tools (`spec/tools/`): compile a call into a Transaction or ReadOp, run it, render the result as §9 text inside the untrusted-data fence. |
| Front ends | `quillwork_server.mcp_server`, `quillwork_server.http_app` | `quillwork-mcp` (MCP, stdio or streamable HTTP) and `quillwork-server` (HTTP API, Starlette). |

The library needs only `quillwork`. The front ends are extras (`[mcp]`, `[http]`, `[all]`), so a
host that embeds the library does not pull in Starlette or the MCP SDK.

## Run it

```bash
pip install 'quillwork-server[all]'

quillwork-mcp                                 # MCP over stdio (desktop clients)
quillwork-mcp --transport http --port 8766    # MCP over streamable HTTP, localhost only
quillwork-server --store ./docs --api-key "$KEY"   # HTTP API
quillwork-server --engine fake                # plain-text fake engine, for demos
```

Every setting is an environment variable and most are also a flag (the flag wins):

| Variable | Flag | Default | Meaning |
|---|---|---|---|
| `QUILLWORK_STORE` | `--store` | memory | Directory for `FileStore`; empty keeps documents in memory. |
| `QUILLWORK_ENGINE` | `--engine` | `quillwork` | `quillwork` (the binding) or `fake`. |
| `QUILLWORK_AUTHOR` | `--author` | `AI` | Author of agent edits (tracked-change author). |
| `QUILLWORK_API_KEY` | `--api-key` | none | HTTP API key (`Authorization: Bearer` or `X-API-Key`). |
| `QUILLWORK_MAX_UPLOAD_BYTES` | | 50 MiB | Largest upload or import. |
| `QUILLWORK_MAX_OPS` | | 200 | Ops per transaction. |
| `QUILLWORK_HOST` / `QUILLWORK_PORT` | `--host` / `--port` | `127.0.0.1` / `8765` | Listen address. |

Both servers refuse to listen beyond localhost without protection: `quillwork-server` needs an
API key, and `quillwork-mcp --transport http` has no auth hook at all, so it only binds to
localhost (put a proxy that authenticates in front of it if you need more).

## Embed it

```python
from quillwork_server.core import DocumentService, FileStore, QuillworkEngine
from quillwork_server.tools import DATA_NOTICE, ToolRunner, export

service = DocumentService(QuillworkEngine(), FileStore("/var/lib/quillwork"))
info = service.upload(docx_bytes, name="contract.docx", author="marc")

# The agent loop: give the model the tools, run its calls.
tools = export("anthropic")              # or "openai", "openai-responses", "mcp"
runner = ToolRunner(service, author="AI")
result = runner.call(info.id, "edit", tool_input, turn=turn_id)
result.text          # §9 text, fenced: what goes back to the model
result.structured    # the spec JSON (OpResult / ReadResult / error envelope)
result.is_error
# Put DATA_NOTICE in the system prompt: it says what the fence means.
```

Or mount the front ends inside a host app:

```python
from quillwork_server.http_app import api_key_auth, create_app
from quillwork_server.mcp_server import build_server

app = create_app(service, authenticate=my_auth, agent_author="AI")   # Starlette ASGI app
mcp = build_server(service, author="AI")                             # mcp.server.Server
```

`authenticate(request)` returns a `Principal(id, author)` or `None` (401); it may be async.
`author` signs that caller's transactions.

### `DocumentService`

```python
DocumentService(engine, store=None, *, locks=None, notifier=None, config=None, id_factory=None)
```

| Method | Does |
|---|---|
| `upload(data, *, name, document_id=None, author=None)` | New document at version 1. |
| `import_version(id, data, *, author, label)` | A new file for an existing document (re-upload from Word). The engine reopens it with the previous sidecar so anchors carry over where it can. |
| `restore(id, version, *, author)` | A new version with an old version's bytes. History is never rewritten. |
| `apply(id, transaction, *, author, turn)` | Apply a Transaction, store the next version, return the OpResult. The host's `author`/`turn` win over the transaction's. |
| `undo(id, change_set, *, author, turn)` | Reject the change set, as a new version (`kind: "undo"`). |
| `read(id, range, *, mode, budget, version)`, `find(...)`, `read_op(id, request)` | Projection reads. |
| `view_model(id, *, version)` | The viewer's JSON. |
| `export(id, *, version, variant)` | `.docx` bytes; `variant` is `redline`, `clean` or `original`. |
| `patches_since(id, since)` → `PatchFeed` | The patches a live viewer needs, or `reload`. |
| `wait(id, after, timeout)` | Block until the head passes `after` (SSE, long polling). |
| `put_asset(id, data)`, `get_asset(id, asset, *, version)` | Images (PNG/JPEG/GIF/WebP/SVG sniffed; ids are content hashes). |
| `info`, `list_documents`, `versions(id, after)`, `head(id)` | Metadata. |
| `session(id)` → `Session` | The same calls bound to one document, tracking the version the caller last saw. |

Every method is synchronous and thread-safe; async hosts call it from a worker thread.

How a write goes:

1. Take the document's writer lock (`LockProvider`). A timeout answers `CONFLICT`.
2. Get the live engine document at the head (cached, LRU of `max_live_documents`; reopened from
   the store when cold or behind).
3. `engine.apply(tx)`. The engine checks `base_version` and every guard and is atomic.
4. `store.put_version(expected_head=head, ...)`: compare-and-swap on the head. If another
   writer slipped past an advisory lock, the service retries once against the new head, then
   answers `CONFLICT`.
5. Announce the new head on the `Notifier`.

The interfaces a host implements:

| Interface | Reference implementations | A typical production host |
|---|---|---|
| `Store` (versions: bytes + sidecar + record; assets; CAS on the head) | `MemoryStore`, `FileStore` (a directory per document, `flock` for cross-process CAS) | Postgres rows + S3 blobs |
| `LockProvider` | `LocalLocks` (in process) | Postgres advisory lock |
| `Notifier` | `LocalNotifier` (in process) | Postgres `LISTEN/NOTIFY` |
| `Engine` / `EngineDocument` | `QuillworkEngine` (the binding), `FakeEngine` (plain text, for tests and demos) | `QuillworkEngine` |

The reference stores a full snapshot per version, not DESIGN's op log plus a snapshot every N:
until the binding exposes a replayable op log, a snapshot is the only thing it can reopen.
`Store` is shaped so a host can do either.

### `FakeEngine`

A stand-in with the spec's shapes, so everything above it is testable before the real engine
lands. `fake_bytes(*paragraphs)` makes a "file" (UTF-8 lines; `# ` makes a heading). It has
block anchors (`b100`, `b101`, ...), guards, tracked and direct edits, change sets,
accept/reject, the §2–§4 projection, a view model and patches. It replaces whole blocks (no
word-level diff), and ops it does not model answer `UNSUPPORTED`.

## Agent tools

`quillwork_server.tools` turns the 8 tool definitions in `spec/tools/` into engine calls.

- **Exports**: `export(shape)` gives the tools as Anthropic, OpenAI (Chat Completions or
  Responses) or MCP definitions. The output equals `spec/tools/export.py` (a test checks it).
- **Compile**: `compile_call(tool, args, version=head)` gives a `ReadCall` (ReadOp) or an
  `ApplyCall` (Transaction). Every tool-call example in the spec compiles to a document that
  validates against the schemas. Bad arguments are `INVALID_OP` with a `path` into the call.
- **Errors**: `translate_error` maps the engine's error back onto the call (`/ops/2/align` in
  the compiled transaction becomes `/ops/1/align` in the `format` call that produced it).
- **Text**: `render_op_result`, `render_error` and `render_read` produce §9 exactly (tests
  compare against the blocks in `spec/projection.md`).
- **Fence**: `fence()` wraps every result in `<document_data document=".." version="..">`.
  Any `<document_data` or `</document_data` inside is broken with a zero-width space, so the
  document can't close the fence. `DATA_NOTICE` is the matching system-prompt text; the MCP
  server sends it as its `instructions`.

## MCP server

`quillwork-mcp`, built on the official `mcp` SDK (low-level `Server`).

**Tools**: the 8 spec tools, each with one extra optional property `document_id`, plus:

| Tool | Does |
|---|---|
| `open_document {document_id? , content_base64?, name?}` | Open an existing document or upload a new one; it becomes the current document. |
| `list_documents {}` | The documents in the store. |
| `export_document {document_id?, variant?, version?}` | The `.docx` as an embedded blob resource, plus a one-line summary. |

`read_document` and `find` carry `readOnlyHint`. A failed call answers `isError: true` with the
§9 error text and the envelope as `structuredContent`.

**Current document**: kept per MCP session id. MCP 2026-07-28 connections are stateless and
have no session id, so there it is one per server process. That is right for stdio (one
client); a shared HTTP deployment should pass `document_id` on every call.

**Resources**:

| URI | Content |
|---|---|
| `quillwork://documents/{id}` | The projection, fenced (`text/plain`). Listed per document. |
| `quillwork://documents/{id}/view-model` | The view model (JSON). |
| `quillwork://documents/{id}/versions` | The version list (JSON). |
| `quillwork://documents/{id}/export/{redline,clean,original}` | The `.docx` (blob). |

## HTTP API

`quillwork-server` (DESIGN §11.1). Files go up as raw bodies (no multipart); everything else is
JSON. Every error answers with the spec envelope `{spec_version, ok: false, error}`.

| Route | Does |
|---|---|
| `GET /health` | `{ok, version, engine}`. No auth. |
| `POST /documents` `?name=&id=` (or `X-Filename`) | Upload the body. 201 `{id, name, version}` + `Location`. |
| `GET /documents` | `{documents: [...]}` |
| `GET /documents/{id}` | `{id, name, head, created_at}` |
| `PUT /documents/{id}/file` `?label=` | A new file for the document (re-upload). |
| `GET /documents/{id}/view` `?version=` | View model. |
| `GET /documents/{id}/projection` `?mode=&range=&budget=&version=&format=json` | The projection as `text/plain` (headers `X-Quillwork-Version`, `X-Quillwork-Next`), or the ReadResult JSON. |
| `POST /documents/{id}/read` | A ReadOp; answers the ReadResult. |
| `POST /documents/{id}/ops` | A Transaction; answers the OpResult. The caller's principal is the author. |
| `POST /documents/{id}/tools/{tool}` | Run an agent tool (header `X-Quillwork-Turn`): `{text, result, is_error}`. |
| `POST /documents/{id}/changes/{cs}/accept` and `/reject` | Body `{base_version?, label?}`. |
| `GET /documents/{id}/versions` `?after=` | `{versions: [...]}` (records without patches). |
| `POST /documents/{id}/versions/{v}/restore` | A new version with version `v`'s bytes. |
| `GET /documents/{id}/export` `?format=docx&variant=&version=` | The file. `format=pdf` is `UNSUPPORTED` until a converter is wired. |
| `GET /documents/{id}/assets/{asset}` | An image. `nosniff`, immutable cache, and a sandboxing CSP so an SVG opened directly runs nothing. |
| `POST /documents/{id}/images` | Upload an image (raw body). 201 `{asset}`. |
| `GET /documents/{id}/events` `?since=&until=` | Server-sent events (below). |

**Status codes**:

| Status | Codes |
|---|---|
| 400 | `INVALID_OP` |
| 403 | `PROTECTED` |
| 404 | `NOT_FOUND` |
| 409 | `STALE_ANCHOR`, `CONFLICT`, `BAD_VERSION`, `CONFIRM_REQUIRED` |
| 413 | `LIMIT` |
| 422 | `AMBIGUOUS`, `IN_FIELD`, `BAD_PACKAGE`, `UNSUPPORTED` |
| 500 | `INVARIANT`, `INTERNAL` |
| 401 | Not a spec error: `{ok: false, error: "unauthorized"}` with `WWW-Authenticate: Bearer`. |

Request bodies are capped while they stream: uploads at `max_upload_bytes`, images at
`max_asset_bytes`, JSON at 4 MiB. Over the cap is `LIMIT` (413).

**Events** (`text/event-stream`):

```
event: hello            data: {"version": 3}          # where the stream starts
id: 4  event: patch     data: <the OpResult patch>    # one per new version
id: 5  event: reload    data: {"version": 5}          # no patch chain (re-import, restore): refetch the view
: keep-alive                                          # every 15 s while idle
```

The start is `?since=`, else `Last-Event-ID`, else the head. Each event's `id` is the version
it leads to, so a reconnect resumes where it stopped. `?until=` ends the stream once that
version is sent.

DESIGN's other event types (`change_set`, `comment`, `lint`, `export_ready`) are not sent yet:
the first three are inside the patch, and nothing exports asynchronously.

## Tests

`python/quillwork_server/tests/`, 108 tests. Run from the repo root:

```bash
.venv/bin/python -m pytest python/quillwork_server/tests/
cd python/quillwork_server && ruff check . && ruff format --check . && mypy --strict src tests
```

| File | Covers |
|---|---|
| `test_store.py` | Both stores: CAS, 8 threads racing for one head, FileStore persistence, bad ids. |
| `test_service.py` | Versions, author, guards, limits, undo, restore, import, patches, 8 concurrent writers, two services on one FileStore, export variants, assets, sessions. |
| `test_tools.py` | Vendored tools equal `spec/tools`; exports equal `spec/tools/export.py`; every example compiles to a schema-valid document; §9 text matches `spec/projection.md` exactly; the fence; the runner. |
| `test_mcp.py` | The MCP server through the SDK's in-memory client: tool list, open/read/edit/stale/export, resources. |
| `test_http.py` | The HTTP API through Starlette's test client: auth, limits, status codes, every route, SSE. |
| `test_real_engine.py` | The real binding: upload then export (`original` and `redline`) of `corpus/synthetic/*.docx` is byte-identical. Skips without the binding. |

## What waits on the real engine

Through `QuillworkEngine` the binding opens, applies, saves and reopens documents with their
sidecars, so upload, apply, undo, import, restore, export (all three variants), versions and
assets run on the real engine (`tests/test_real_engine.py`). Every read op in the catalogue runs
there too: `read`, `find`, `outline`, `lint` and `help` answer in text, `get_block`,
`list_changes`, `list_comments`, `list_images`, `get_image` and `list_fields` in data (shapes in
spec/projection.md §8). `advanced_op` reaches all of them, so an agent lists the tracked changes
and comments before it calls `review` or `comment`; `test_real_engine.py` runs that loop on
`corpus/fixtures/07`, `08`, `09` and `11`. `FakeEngine` answers the same ops with the same data
shapes on its plain-text documents (no images, no content controls).

Also waiting on the binding:

- **PDF export** needs the converter (DESIGN §9).
- **Assets from the package.** `QuillworkEngine.asset` looks images up in `word/media/` by
  content hash; the view model's asset ids must use the same `a_` + first 16 hex of sha256 (spec/anchors.md).

## Proposed spec and binding changes

None of these are made; they are what building the service turned up.

**Errors (`spec/errors.md`, `error.schema.json`)**

1. `NOT_FOUND` with `what: "document"` (and `"version"`) for an unknown document id or
   version. Today `what` only names things inside a document.
2. A `LIMIT` name for the package size: `package_bytes`. The service uses
   `uncompressed_bytes` meanwhile, which is not quite right for a zipped upload.
3. An `UNSUPPORTED` reason `not_implemented`, for a feature the build does not have (PDF
   export without a converter) as opposed to content the engine must preserve.
4. A lock timeout is `CONFLICT` (retryable). Say so in the `CONFLICT` row.
5. Document-level errors (upload, bad body) have no meaningful version. The schema requires
   `version >= 1`; the service sends 1. Allow `null`, or document the convention.

**Text (`spec/projection.md`)**

6. §9 shows candidates as `<anchor> #<occurrence>: <context>`. A `NOT_FOUND` near-match has
   `occurrence: null`; the service prints `<anchor>: <context>`. Say so.
7. A read result's text should end with `§ next: <range>` when there is more. Reading by range
   gives no window line, so an agent can't tell how to continue.

**Ops (`ops.schema.json`)**

8. `accept`/`reject` allow exactly one selector per op, so the `review` tool compiles "Bob's
   changes plus revisions 3 and 4" into several ops. Fine, but say that ops apply in order
   and a later one sees the result of the earlier.
9. Several `insert_row` ops after the same row: state the order they land in (the `table`
   tool emits one per row and assumes in order).

**Tools (`spec/tools/`)**

10. MCP: an optional `document_id` on each tool, for servers that hold several documents.
    The definitions are otherwise unchanged; the spec could carry it as an MCP-only addition.
11. Undo in direct mode (no change set to reject) is `restore(version)`. DESIGN §11.1 says
    so; the tools have no way to ask for it. A `restore` action on `review`, or leave it to
    the host.

**Binding**

12. Done: `Document.open(data, limits=None, *, sidecar=None, version=None)` and
    `Document.sidecar()`; a reopened document keeps its version numbers and anchors, and
    `QuillworkEngine` passes them straight through (no more version offset).
13. Done: `read`, `read_op` and `view_model` run every catalogue read op and the view model.
