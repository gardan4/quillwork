# quillwork-server

The service layer for [Quillwork](https://github.com/gardan4/quillwork), which does lossless,
AI-friendly editing of Word documents. It has three layers:

- **`quillwork_server.core`**: `DocumentService`, which handles document versions, one writer
  per document, undo, export and live patches. Storage, locks and the engine are interfaces.
- **`quillwork_server.tools`**: the eight agent tools (`read_document`, `find`, `edit`,
  `format`, `table`, `comment`, `review`, `advanced_op`). Each one compiles a call into an
  engine transaction and renders the result as text inside an untrusted-data fence.
- **Front ends**: `quillwork-mcp`, an MCP server over stdio or streamable HTTP, and
  `quillwork-server`, an HTTP API.

```sh
pip install 'quillwork-server[mcp]'     # or [http], or [all]
quillwork-mcp --store ./docs            # stdio, for MCP clients such as Claude Desktop
```

The library alone (`pip install quillwork-server`) depends only on `quillwork`. Configuration,
the HTTP API and how to embed it in your own service are covered in
[docs/SERVER.md](https://github.com/gardan4/quillwork/blob/main/docs/SERVER.md).

This is an alpha (0.0.x). Licensed under Apache-2.0.
