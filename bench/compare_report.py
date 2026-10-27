"""Render results-compare.json into the per-file tables of results-compare.md (stdout).

python bench/compare_report.py [bench/results-compare.json]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

E = ["quillwork", "adeu", "docxodus", "python-docx"]
LABEL = {
    "quillwork": "Quillwork",
    "adeu": "Adeu",
    "docxodus": "Docxodus",
    "python-docx": "python-docx",
}


def name(r):
    return r["file"].split("/")[-1]


def noop_cell(x):
    n, c = x["noop"], x["noop_canonical"]
    tot = c["parts_total"]
    f = "identical" if n["file_identical"] else "changed"
    return f"{f} / {n['entries_identical']}/{tot} / {n['parts_identical']}/{tot} / {c['parts_canonically_identical']}/{tot}"


def edit_cell(x):
    e, c = x["edit"], x["edit_canonical"]
    pc = c["paragraphs_canonically_changed"]
    return (
        f"{len(e['other_entries_changed'])} ({len(c['other_entries_canonically_changed'])}) / "
        f"{len(e['paragraphs_changed'])}/{e['paragraphs']} ({len(pc) if pc is not None else '?'})"
    )


def target_cell(x):
    e = x["edit"]
    return f"{'yes' if e['target_ok'] else 'NO'}, {e['kind']}"


def sdk_cell(x, key):
    o = x.get(key, {}).get("ooxml", {})
    if o.get("ok") is None:
        return "n/a"
    return "0" if o.get("ok") else str(o.get("new_error_count"))


def main():
    path = Path(
        sys.argv[1]
        if len(sys.argv) > 1
        else Path(__file__).with_name("results-compare.json")
    )
    d = json.loads(path.read_text())
    rows = d["rows"]
    out = []
    w = out.append
    w("### No-op (open, save)\n")
    w(
        "Each cell: file / zip entries with identical stored bytes / parts with identical content / "
        "parts canonically identical.\n"
    )
    w("| file | " + " | ".join(LABEL[e] for e in E) + " |")
    w("|---|" + "---|" * len(E))
    for r in rows:
        w(f"| {name(r)} | " + " | ".join(noop_cell(r["engines"][e]) for e in E) + " |")
    w("\n### One-word edit\n")
    w(
        "Each cell: zip entries changed besides `word/document.xml` (canonically changed) / outermost "
        "paragraphs changed of total (canonically changed).\n"
    )
    w("| file | word | " + " | ".join(LABEL[e] for e in E) + " |")
    w("|---|---|" + "---|" * len(E))
    for r in rows:
        w(
            f"| {name(r)} | {r['word']} | "
            + " | ".join(edit_cell(r["engines"][e]) for e in E)
            + " |"
        )
    w("\n### Edit correctness and validity\n")
    w(
        "Each cell: target edited correctly, edit kind / new Open XML SDK errors after no-op / after edit. "
        "Every output of every engine opened as a zip, parsed as XML and loaded in python-docx.\n"
    )
    w("| file | " + " | ".join(LABEL[e] for e in E) + " |")
    w("|---|" + "---|" * len(E))
    for r in rows:
        cells = []
        for e in E:
            x = r["engines"][e]
            ok = all(
                x[k]["zip_ok"]
                and not x[k]["xml_parse_errors"]
                and x[k]["python_docx_loads"]
                for k in ("noop_valid", "edit_valid")
            )
            cells.append(
                f"{target_cell(x)} / {sdk_cell(x, 'noop_valid')} / {sdk_cell(x, 'edit_valid')}"
                + ("" if ok else " (STRUCTURAL FAIL)")
            )
        w(f"| {name(r)} | " + " | ".join(cells) + " |")
    w("\n### Tokens (o200k_base) of each engine's full read view\n")
    w(
        "| file | raw document.xml | Quillwork `read()` | Adeu CriticMarkup | Docxodus markdown | python-docx text |"
    )
    w("|---|---|---|---|---|---|")
    for r in rows:
        t = r["tokens"]
        w(
            f"| {name(r)} | {t['document_xml']} | {t['quillwork']} | {t['adeu']} | {t['docxodus']} | {t['python-docx']} |"
        )
    print("\n".join(out))


if __name__ == "__main__":
    main()
