"""Check a fixture against its hand-written `.expect.json` sidecar using the text oracle (and, for invalid
fixtures, the validator's error codes). Returns a list of human-readable mismatches; empty means it matches."""
from __future__ import annotations

import json
from pathlib import Path

from . import text
from .ooxml import NotADocx, Package


def sidecar(docx: str | Path) -> Path:
    p = Path(docx)
    return p.with_name(p.name.rsplit(".", 1)[0] + ".expect.json")


def check(docx: str | Path, expect: dict | None = None) -> list[str]:
    expect = expect if expect is not None else json.loads(sidecar(docx).read_text(encoding="utf-8"))
    problems: list[str] = []

    def cmp(key, got, want):
        if got != want:
            problems.append("%s: expected %s, got %s" % (key, json.dumps(want, ensure_ascii=False),
                                                         json.dumps(got, ensure_ascii=False)))

    if expect.get("body") is None:  # unreadable on purpose; nothing to compare
        return problems
    try:
        pkg = Package.open(str(docx))
    except NotADocx as e:
        return ["cannot open: %s" % e]
    main = pkg.main_part()
    views = {m: text.extract(pkg, m) for m in text.MODES}
    for m in text.MODES:
        cmp("body[%s]" % m, [p.text for p in views[m][main]], expect["body"][m])
    paras = views["markup"][main]
    if "paragraph_count" in expect:
        cmp("paragraph_count", len(paras), expect["paragraph_count"])
    for key, story in (expect.get("stories") or {}).items():
        cmp("stories[%s]" % key, [p.text for p in views["markup"].get(key, [])], story)
    if "styles" in expect:
        cmp("styles", [p.style for p in paras], expect["styles"])
    if "outline_levels" in expect:
        cmp("outline_levels", [p.outline for p in paras], expect["outline_levels"])
    if "numbering" in expect:
        cmp("numbering", [p.num for p in paras], expect["numbering"])
    if "bidi" in expect:
        cmp("bidi", [p.bidi for p in paras], expect["bidi"])
    got_hidden = [{"paragraph": p.index, **h} for p in paras for h in p.hidden]
    cmp("hidden", got_hidden, expect.get("hidden", []))

    f = text.facts(pkg)
    rev = expect.get("revisions", {"authors": []})
    cmp("revisions", f["revisions"], rev)
    cmp("comments", f["comments"], expect.get("comments", []))
    cmp("fields", f["fields"], expect.get("fields", []))
    cmp("bookmarks", f["bookmarks"], expect.get("bookmarks", []))
    cmp("sdt", f["sdt"], expect.get("sdt", 0))
    cmp("images", f["images"], expect.get("images", {"inline": 0, "anchor": 0, "pictures": 0}))
    cmp("tables", f["tables"], expect.get("tables", 0))
    cmp("footnotes", f["footnotes"], expect.get("footnotes", 0))
    cmp("endnotes", f["endnotes"], expect.get("endnotes", 0))
    cmp("alternate_content", f["alternate_content"], expect.get("alternate_content", 0))
    cmp("macros", f["macros"], expect.get("macros", False))
    z = expect.get("zip")
    if z:
        stored = [n for n in pkg.names if pkg.infos[n].compress_type == 0]
        desc = [n for n in pkg.names if pkg.infos[n].flag_bits & 0x08]
        cmp("zip.stored", stored, z["stored"])
        cmp("zip.data_descriptor", desc, z["data_descriptor"])
    if expect.get("bom"):
        no_bom = [n for n in pkg.names if n.endswith((".xml", ".rels")) and not pkg.data[n].startswith(b"\xef\xbb\xbf")]
        cmp("parts without BOM", no_bom, [])
    return problems
