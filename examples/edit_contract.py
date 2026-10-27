"""Edit a .docx the way an AI agent would, as tracked changes, and check what else moved.

    python examples/make_sample.py sample-contract.docx
    python examples/edit_contract.py sample-contract.docx [OUT.docx]

1. A no-op open and save gives back the input bytes exactly.
2. One transaction: a word-level replace, a new numbered clause and a comment, all tracked.
3. Every zip entry the edit did not need to touch is byte-identical to the input; the script prints
   which ones changed (the document body, plus the comment parts the comment needs).
4. The markup projection (what an agent reads back) shows the redlines.

Works on any .docx; the anchors and text below match the sample from ``make_sample.py``.
"""

from __future__ import annotations

import io
import struct
import sys
import zipfile
from pathlib import Path

import quillwork


def raw_entries(data: bytes) -> dict[str, bytes]:
    """Each zip entry's local header plus compressed data, exactly as stored."""
    out: dict[str, bytes] = {}
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        for info in z.infolist():
            start = info.header_offset
            name_len, extra_len = struct.unpack_from("<HH", data, start + 26)
            out[info.filename] = data[
                start : start + 30 + name_len + extra_len + info.compress_size
            ]
    return out


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    src = Path(argv[1])
    dst = Path(argv[2]) if len(argv) > 2 else src.with_name(src.stem + ".edited.docx")
    before = src.read_bytes()

    # 1. No-op round trip.
    assert quillwork.Document.open(before).save() == before
    print("no-op save: byte-identical")

    # 2. What the agent sees, then one atomic transaction. `expect` guards against a stale read:
    #    if the paragraph changed since, the whole transaction is rejected and nothing is written.
    doc = quillwork.Document.open(before)
    doc.set_agent("AI assistant")
    print(doc.read())
    result = doc.apply(
        {
            "base_version": doc.version,
            "label": "Raise hours, add a travel clause",
            "ops": [
                {
                    "op": "replace_text",
                    "anchor": "b103",
                    "find": "16 hours",
                    "replace": "20 hours",
                    "expect": {
                        "text": "The Consultant provides advice on employment policy for 16 hours per week."
                    },
                },
                {
                    "op": "insert_paragraph",
                    "after": "b105",
                    "text": "Travel time is billed at half the hourly rate.",
                },
                {
                    "op": "add_comment",
                    "anchor": "b104",
                    "find": "EUR 95",
                    "text": "Check against the 2027 rate card.",
                },
            ],
        }
    )
    print("changed blocks:", *result["changed"], sep="\n  ")

    # 3. What moved in the file.
    after = doc.save()
    a, b = raw_entries(before), raw_entries(after)
    changed = sorted(name for name in b if a.get(name) != b[name])
    kept = sum(1 for name in a if a[name] == b.get(name))
    print(
        f"zip entries byte-identical: {kept} of {len(a)}; changed or added: {', '.join(changed)}"
    )

    # 4. What the agent reads back.
    print(doc.read(mode="markup"))
    dst.write_bytes(after)
    print(f"wrote {dst}: open it in Word to accept or reject the changes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
