"""Fill the public business template and verify a reviewable, local candidate.

python corpus/synthetic/gen_presentation.py /tmp/business-template.pptx
python examples/fill_presentation.py /tmp/business-template.pptx /tmp/proposal.pptx
"""

from __future__ import annotations

import hashlib
import io
import json
import sys
import zipfile
from pathlib import Path

from quillwork import Presentation


def entries(data: bytes) -> dict[str, bytes]:
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        return {
            i.filename: data[
                i.header_offset : i.header_offset
                + 30
                + int.from_bytes(
                    data[i.header_offset + 26 : i.header_offset + 28], "little"
                )
                + int.from_bytes(
                    data[i.header_offset + 28 : i.header_offset + 30], "little"
                )
                + i.compress_size
            ]
            for i in archive.infolist()
        }


def main(source: Path, destination: Path) -> None:
    if source.resolve() == destination.resolve():
        raise ValueError(
            "write the candidate to a different path so the source remains available for review"
        )
    original = source.read_bytes()
    deck = Presentation.open(original)
    assert deck.save() == original
    print(deck.read())
    deck.fill(
        {"client": "Example client", "period": "November 2026", "fee": "EUR 4,500"},
        expected_counts={"client": 2, "period": 1, "fee": 1},
    )
    candidate = deck.save()
    report = deck.review()
    assert report["candidate_sha256"] == hashlib.sha256(candidate).hexdigest()
    assert report["source_sha256"] == hashlib.sha256(original).hexdigest()
    allowed = {change["part"] for change in report["changes"]}
    before, after = entries(original), entries(candidate)
    assert before.keys() == after.keys()
    assert all(before[name] == after[name] for name in before if name not in allowed)
    destination.write_bytes(candidate)
    destination.with_suffix(".review.json").write_text(
        json.dumps(report, indent=2) + "\n"
    )
    print(
        f"Wrote {destination}; {len(report['changes'])} changed paragraphs in {len(allowed)} parts."
    )


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    main(Path(sys.argv[1]), Path(sys.argv[2]))
