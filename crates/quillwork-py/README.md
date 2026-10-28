# quillwork

Lossless, AI-friendly editing of Word documents and existing PowerPoint decks, for Python
3.12 and newer.

An agent reads a compact text view of the document, with a short anchor on every paragraph,
table row and comment. It sends small JSON edits against those anchors. Quillwork writes each
edit into the original file as a real Word tracked change and leaves every byte it did not need
to touch exactly as it was. Letterheads, numbering, styles, fields and existing tracked changes
survive, and a person reviews the agent's work in Word's Review pane.

```python
from pathlib import Path

import quillwork

doc = quillwork.Document.open(Path("contract.docx").read_bytes())
doc.set_agent("AI")
print(doc.read())        # [b103 #1.] The Consultant provides advice ... for 16 hours per week.

doc.apply({
    "base_version": doc.version,
    "ops": [{"op": "replace_text", "anchor": "b103", "find": "16 hours", "replace": "20 hours"}],
})
Path("contract.edited.docx").write_bytes(doc.save())
```

`doc.help()` lists every op. This is an alpha (0.0.x): the op language and the sidecar format
may change between releases.

`quillwork.Presentation` edits existing `.pptx` text, tables and speaker notes and fills
literal `{{name}}` template slots across formatted runs. It preserves the branded deck and
returns exact source/candidate hashes and a text review report. PowerPoint candidates need
application-owned approval and a visual fit check. Slide creation and image/chart editing
are outside this initial API. See the
[PowerPoint guide](https://github.com/gardan4/quillwork/blob/main/docs/POWERPOINT.md).

- Source, docs and benchmarks: https://github.com/gardan4/quillwork
- Binding reference: https://github.com/gardan4/quillwork/blob/main/docs/BINDING.md
- MCP server and agent tools: the `quillwork-server` package

Licensed under Apache-2.0.
