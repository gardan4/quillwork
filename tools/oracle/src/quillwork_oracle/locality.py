"""Byte-locality oracle: did an edit touch only what it meant to?

Given the original .docx, the edited .docx and the set of paragraphs the edit was meant to change, report:

* zip level: entries added / removed / content-changed, entries whose stored bytes changed while the content did
  not (recompressed), entry order changes, and whether the whole file is byte-identical;
* paragraph level, for every changed XML part that holds paragraphs: the raw bytes of each outermost <w:p> (found
  with a byte scanner, not a re-serializer) are aligned between the two files; a paragraph whose bytes changed and
  that is not in the intended set is a violation, as is any change to the bytes *between* paragraphs (table
  markup, sectPr, root element, XML declaration);
* per intended paragraph (ported from the bake-off scorer): changed characters vs redline characters, whether pPr
  survived, and whether the run formatting of the unchanged prefix/suffix survived.

Intended paragraphs are given as w14:paraId (exactly 8 hex digits), as an index into the main part's paragraphs (`5`), or
as `<part>:<index-or-paraId>` for other parts. Indices count outermost w:p elements in document order (table-cell
paragraphs count, text-box paragraphs do not), which is the text oracle's `index`.
"""
from __future__ import annotations

import difflib
import fnmatch
import re
from dataclasses import dataclass, field

from lxml import etree

from .ooxml import W14_NS, W_NS, Package, w

DEFAULT_ALLOWED_PARTS = [
    "[[]Content_Types].xml",  # fnmatch: a literal opening bracket, not a character class
    "word/_rels/document.xml.rels",
    "word/comments.xml",
    "word/commentsExtended.xml",
    "word/commentsIds.xml",
    "word/commentsExtensible.xml",
    "word/people.xml",
]

# --------------------------------------------------------------------------------------------------------------
# Raw byte scanner for outermost w:p spans
# --------------------------------------------------------------------------------------------------------------

_TOKEN = re.compile(
    rb"<!--.*?-->|<!\[CDATA\[.*?\]\]>|<\?.*?\?>|<!DOCTYPE[^>]*>"
    rb"|<(/?)([^\s/>!?]+)((?:[^>\"']|\"[^\"]*\"|'[^']*')*?)(/?)>",
    re.S)
_ATTR = re.compile(rb"([^\s=]+)\s*=\s*(\"[^\"]*\"|'[^']*')")


@dataclass
class Span:
    start: int
    end: int
    para_id: str | None


def scan_paragraphs(data: bytes) -> list[Span]:
    """Byte spans of outermost WordprocessingML <w:p> elements (any prefix bound to the W namespace)."""
    spans: list[Span] = []
    stack: list[tuple[bytes, dict]] = []  # (qname, nsmap)
    depth_p = 0
    cur_start = 0
    cur_id: str | None = None
    for m in _TOKEN.finditer(data):
        if m.group(2) is None:
            continue
        closing, qname, attrs, selfclose = m.group(1), m.group(2), m.group(3), m.group(4)
        if closing:
            if not stack:
                continue
            q, ns = stack.pop()
            if _is_wp(q, ns):
                depth_p -= 1
                if depth_p == 0:
                    spans.append(Span(cur_start, m.end(), cur_id))
            continue
        ns = dict(stack[-1][1]) if stack else {}
        parsed = _ATTR.findall(attrs)
        for k, v in parsed:
            if k == b"xmlns":
                ns[b""] = v[1:-1]
            elif k.startswith(b"xmlns:"):
                ns[k[6:]] = v[1:-1]
        is_p = _is_wp(qname, ns)
        if is_p and depth_p == 0:
            cur_start = m.start()
            cur_id = None
            for k, v in parsed:
                if b":" in k:
                    pfx, local = k.split(b":", 1)
                    if local == b"paraId" and ns.get(pfx) == W14_NS.encode():
                        cur_id = v[1:-1].decode("ascii", "replace").upper()
        if selfclose:
            if is_p and depth_p == 0:
                spans.append(Span(m.start(), m.end(), cur_id))
            continue
        stack.append((qname, ns))
        if is_p:
            depth_p += 1
    return spans


def _is_wp(qname: bytes, ns: dict) -> bool:
    if b":" in qname:
        pfx, local = qname.split(b":", 1)
    else:
        pfx, local = b"", qname
    return local == b"p" and ns.get(pfx) == W_NS.encode()


# --------------------------------------------------------------------------------------------------------------
# Per-paragraph formatting helpers (ported from the bake-off scorer)
# --------------------------------------------------------------------------------------------------------------

_DROP_RPR = {w("rPrChange"), w("ins"), w("del"), w("moveFrom"), w("moveTo"), w("noProof"), w("lang")}
_DROP_PPR = {w("pPrChange"), w("rPr"), w("sectPr")}


def _key(el, drop) -> str:
    if el is None:
        return ""
    el = etree.fromstring(etree.tostring(el))
    for c in list(el):
        if c.tag in drop:
            el.remove(c)
    for e in el.iter():
        for a in list(e.attrib):
            if "rsid" in a or a.startswith("{%s}" % W14_NS):
                del e.attrib[a]
    return etree.tostring(el, method="c14n").decode()


def _chars(p, mode: str) -> list[tuple[str, str]]:
    """(char, run-format key) for every visible character of paragraph p in the accept or reject view."""
    out: list[tuple[str, str]] = []
    keep_add, keep_rm = mode == "accept", mode == "reject"

    def walk(el):
        for c in el:
            tag = c.tag
            if not isinstance(tag, str):
                continue
            if tag in (w("ins"), w("moveTo")):
                if keep_add:
                    walk(c)
            elif tag in (w("del"), w("moveFrom")):
                if keep_rm:
                    walk(c)
            elif tag == w("r"):
                k = _key(c.find(w("rPr")), _DROP_RPR)
                for x in c:
                    if x.tag in (w("t"), w("delText")):
                        out.extend((ch, k) for ch in (x.text or ""))
                    elif x.tag in (w("tab"), w("ptab")):
                        out.append(("\t", k))
                    elif x.tag in (w("br"), w("cr")):
                        out.append(("\n", k))
            elif tag in (w("pPr"),):
                continue
            else:
                walk(c)
    walk(p)
    return out


def _tracked_chars(p) -> int:
    n = 0
    for tag in ("ins", "del", "moveFrom", "moveTo"):
        for el in p.iter(w(tag)):
            if el.getparent() is not None and el.getparent().tag == w("rPr"):
                continue  # paragraph-mark revision, no characters
            n += sum(len(t.text or "") for t in el.iter(w("t"), w("delText")))
    return n


def paragraph_stats(op, np_) -> dict:
    """Edit statistics for one intended paragraph (lxml elements of the original and edited paragraph)."""
    o_acc = _chars(op, "accept")
    n_acc = _chars(np_, "accept")
    n_rej = _chars(np_, "reject")
    o_txt = "".join(c for c, _ in o_acc)
    n_txt = "".join(c for c, _ in n_acc)
    sm = difflib.SequenceMatcher(None, o_txt, n_txt, autojunk=False)
    changed = sum((i2 - i1) + (j2 - j1) for tag, i1, i2, j1, j2 in sm.get_opcodes() if tag != "equal")
    redline = _tracked_chars(np_) - _tracked_chars(op)
    # formatting of the untouched prefix/suffix must survive
    pre = 0
    while pre < min(len(o_txt), len(n_txt)) and o_txt[pre] == n_txt[pre]:
        pre += 1
    suf = 0
    while suf < min(len(o_txt), len(n_txt)) - pre and o_txt[-1 - suf] == n_txt[-1 - suf]:
        suf += 1
    fmt_lost = sum(1 for i in range(pre) if o_acc[i][1] != n_acc[i][1])
    fmt_lost += sum(1 for i in range(1, suf + 1) if o_acc[-i][1] != n_acc[-i][1])
    reject_text = "".join(c for c, _ in n_rej)
    return {
        "text_before": len(o_txt),
        "text_after": len(n_txt),
        "changed_chars": changed,
        "redline_chars": redline,
        "redline_ratio": round(redline / changed, 3) if changed and redline > 0 else None,
        "reject_equals_original": reject_text == o_txt if redline > 0 else None,
        "ppr_preserved": _key(op.find(w("pPr")), _DROP_PPR) == _key(np_.find(w("pPr")), _DROP_PPR),
        "affix_chars": pre + suf,
        "affix_format_lost": fmt_lost,
    }


# --------------------------------------------------------------------------------------------------------------
# Main entry
# --------------------------------------------------------------------------------------------------------------

@dataclass
class Intended:
    ids: set = field(default_factory=set)  # (part, paraId)
    idx: set = field(default_factory=set)  # (part, index)

    @classmethod
    def parse(cls, specs, main: str) -> "Intended":
        it = cls()
        for s in specs or []:
            s = str(s)
            part = None
            if ":" in s:
                part, s = s.rsplit(":", 1)
            if s.isdigit() and len(s) != 8:  # 8 hex digits is always a paraId, even when all digits
                it.idx.add((part or main, int(s)))
            else:  # paraIds are unique across the package, so a bare one matches in any part
                it.ids.add((part or "*", s.upper()))
        return it

    def hit(self, part, index, *para_ids) -> bool:
        return (part, index) in self.idx or any((part, p) in self.ids or ("*", p) in self.ids
                                                for p in para_ids if p)


def _allowed(name: str, patterns) -> bool:
    return any(fnmatch.fnmatchcase(name, pat) for pat in patterns)


def locality(original, edited, intended=(), allowed_parts=None, stats: bool = True) -> dict:
    a = original if isinstance(original, Package) else Package.open(original)
    b = edited if isinstance(edited, Package) else Package.open(edited)
    allowed = DEFAULT_ALLOWED_PARTS if allowed_parts is None else list(allowed_parts)
    main = a.main_part()
    want = Intended.parse(intended, main)
    violations: list[dict] = []
    report: dict = {
        "byte_identical": a.raw == b.raw,
        "parts": {"added": [], "removed": [], "changed": [], "recompressed": [], "order_changed": False},
        "paragraphs": {},
        "intended": [],
        "violations": violations,
    }
    sa, sb = set(a.names), set(b.names)
    report["parts"]["added"] = [n for n in b.names if n not in sa]
    report["parts"]["removed"] = [n for n in a.names if n not in sb]
    common = [n for n in a.names if n in sb]
    report["parts"]["order_changed"] = common != [n for n in b.names if n in sa]
    for n in report["parts"]["added"]:
        if not _allowed(n, allowed):
            violations.append({"kind": "part_added", "part": n})
    for n in report["parts"]["removed"]:
        violations.append({"kind": "part_removed", "part": n})
    if report["parts"]["order_changed"]:
        violations.append({"kind": "entry_order_changed", "part": None})

    for n in common:
        if a.data[n] == b.data[n]:
            ia, ib = a.infos[n], b.infos[n]
            if (ia.compress_type, ia.compress_size, ia.flag_bits) != (ib.compress_type, ib.compress_size,
                                                                     ib.flag_bits):
                report["parts"]["recompressed"].append(n)
            continue
        report["parts"]["changed"].append(n)
        if _allowed(n, allowed):
            continue
        if not n.endswith(".xml"):
            violations.append({"kind": "part_changed", "part": n})
            continue
        pa, pb = scan_paragraphs(a.data[n]), scan_paragraphs(b.data[n])
        if not pa and not pb:
            violations.append({"kind": "part_changed", "part": n})
            continue
        report["paragraphs"][n] = _compare_part(n, a.data[n], b.data[n], pa, pb, want, violations, stats,
                                                report["intended"])
    report["ok"] = not violations
    return report


def _outer_ps(data: bytes):
    root = etree.fromstring(data, parser=etree.XMLParser(resolve_entities=False, no_network=True))
    return [p for p in root.iter(w("p")) if not any(x.tag == w("p") for x in p.iterancestors())]


def _compare_part(part, da, db, pa, pb, want, violations, stats, intended_out) -> dict:
    ba = [da[s.start:s.end] for s in pa]
    bb = [db[s.start:s.end] for s in pb]
    sm = difflib.SequenceMatcher(None, ba, bb, autojunk=False)
    changed, inserted, removed = [], [], []
    keep_a, keep_b = [], []  # paragraphs present on both sides, in order (for gap comparison)
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            keep_a += range(i1, i2)
            keep_b += range(j1, j2)
            continue
        pairs = _pair(pa, pb, i1, i2, j1, j2) if tag == "replace" else []
        for i, j in pairs:
            changed.append((i, j))
            keep_a.append(i)
            keep_b.append(j)
        removed += sorted(set(range(i1, i2)) - {i for i, _ in pairs})
        inserted += sorted(set(range(j1, j2)) - {j for _, j in pairs})

    rec = {"paragraphs_before": len(pa), "paragraphs_after": len(pb), "changed": [], "inserted": [], "removed": [],
           "untouched_identical": len(pa) - len(changed) - len(removed)}
    ea = eb = None
    if stats and changed:
        try:
            ea, eb = _outer_ps(da), _outer_ps(db)
            if len(ea) != len(pa) or len(eb) != len(pb):
                ea = eb = None
        except etree.XMLSyntaxError:
            ea = eb = None
    intended_new = set()
    for i, j in changed:
        ok = want.hit(part, i, pa[i].para_id, pb[j].para_id)
        entry = {"index": i, "new_index": j, "para_id": pa[i].para_id or pb[j].para_id, "intended": ok}
        rec["changed"].append(entry)
        if ok:
            intended_new.add(j)
            if ea is not None:
                intended_out.append({"part": part, **entry, **paragraph_stats(ea[i], eb[j])})
            else:
                intended_out.append({"part": part, **entry})
        else:
            violations.append({"kind": "paragraph_changed", "part": part, "index": i, "para_id": entry["para_id"]})
    for i in removed:
        ok = want.hit(part, i, pa[i].para_id)
        rec["removed"].append({"index": i, "para_id": pa[i].para_id, "intended": ok})
        if not ok:
            violations.append({"kind": "paragraph_removed", "part": part, "index": i, "para_id": pa[i].para_id})
    # inserted paragraphs are fine next to an intended (or intentionally inserted) paragraph
    ins_set = set(inserted)
    for j in inserted:
        if want.hit(part, -1, pb[j].para_id):
            intended_new.add(j)
    # map new-side indices of kept paragraphs that were intended
    for i, j in zip(keep_a, keep_b):
        if want.hit(part, i, pa[i].para_id, pb[j].para_id):
            intended_new.add(j)
    for j in inserted:
        lo = j - 1
        while lo >= 0 and lo in ins_set:
            lo -= 1
        hi = j + 1
        while hi < len(pb) and hi in ins_set:
            hi += 1
        ok = j in intended_new or lo in intended_new or hi in intended_new
        rec["inserted"].append({"new_index": j, "para_id": pb[j].para_id, "intended": ok})
        if not ok:
            violations.append({"kind": "paragraph_inserted", "part": part, "new_index": j,
                               "para_id": pb[j].para_id})

    # bytes between paragraphs (inserted/removed paragraphs dropped, their neighbouring gaps joined)
    ga = _gaps(da, pa, set(removed))
    gb = _gaps(db, pb, ins_set)
    if len(ga) != len(gb):
        violations.append({"kind": "structure_changed", "part": part})
    else:
        for k, (x, y) in enumerate(zip(ga, gb)):
            if x != y:
                where = "before paragraph %d" % keep_a[k] if k < len(keep_a) else "after the last paragraph"
                violations.append({"kind": "markup_between_paragraphs_changed", "part": part, "where": where,
                                   "bytes_before": len(x), "bytes_after": len(y)})
    return rec


def _pair(pa, pb, i1, i2, j1, j2) -> list[tuple[int, int]]:
    """Pair old and new paragraphs inside a replaced block: by paraId when both sides carry matching ids (in
    order), otherwise position by position."""
    ids_b = {pb[j].para_id: j for j in range(j1, j2) if pb[j].para_id}
    by_id, last = [], j1 - 1
    for i in range(i1, i2):
        j = ids_b.get(pa[i].para_id) if pa[i].para_id else None
        if j is not None and j > last:
            by_id.append((i, j))
            last = j
    if by_id:
        return by_id
    return [(i1 + d, j1 + d) for d in range(min(i2 - i1, j2 - j1))]


def _gaps(data: bytes, spans: list[Span], drop: set) -> list[bytes]:
    gaps, cur, pos = [], bytearray(), 0
    for k, s in enumerate(spans):
        cur += data[pos:s.start]
        pos = s.end
        if k in drop:
            continue
        gaps.append(bytes(cur))
        cur = bytearray()
    cur += data[pos:]
    gaps.append(bytes(cur))
    return gaps
