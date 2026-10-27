"""Text oracle: per-paragraph text of every story in a .docx, in three views, read with lxml and nothing from the
engine.

Views:
  markup  everything, with tracked changes bracketed: {+inserted+} {-deleted-} {m+moved to+m} {m-moved from-m};
          a tracked paragraph mark shows as {+¶+} / {-¶-} at the end of its paragraph.
  accept  the document as if every tracked change were accepted.
  reject  the document as if every tracked change were rejected.

A paragraph whose mark is removed in a view (accept: deleted mark; reject: inserted mark) merges into the next
paragraph of the same container, as Word does. Table rows whose trPr carries w:ins / w:del are dropped in the view
that removes them.

Stories: the main part, headers/footers (by part name), `<part>#textbox` (text boxes inside that part, Choice branch
of mc:AlternateContent only), `word/footnotes.xml#<id>`, `word/endnotes.xml#<id>`, `word/comments.xml#<id>`.
Paragraph `index` is the position among that story's paragraphs in document order before any merge, which is also
the index the locality oracle uses (outermost w:p elements; table-cell paragraphs count, text-box ones do not).
"""
from __future__ import annotations

import re
from dataclasses import asdict, dataclass, field

from lxml import etree

from .ooxml import M_NS, MC_NS, W14_PARAID, W_NS, WP_NS, PIC_NS, Package, w

MODES = ("markup", "accept", "reject")
ZERO_WIDTH = re.compile("[​‌‍⁠﻿]")
BR_SKIP = {"page", "column"}

INS, DEL, MOVE_TO, MOVE_FROM = w("ins"), w("del"), w("moveTo"), w("moveFrom")
REV_WRAP = {INS: ("{+", "+}", "add"), MOVE_TO: ("{m+", "+m}", "add"), DEL: ("{-", "-}", "remove"),
            MOVE_FROM: ("{m-", "-m}", "remove")}
MARK_TOKENS = {"ins": "{+¶+}", "moveTo": "{m+¶+m}", "del": "{-¶-}", "moveFrom": "{m-¶-m}"}
INLINE_CONTAINERS = {w("hyperlink"), w("smartTag"), w("customXml"), w("fldSimple"), w("bdo"), w("dir"),
                     w("sdtContent")}
BLOCK_CONTAINERS = {w("sdtContent"), w("customXml")}


def _onoff(el) -> bool:
    if el is None:
        return False
    v = el.get(w("val"))
    return v not in ("0", "false", "off")


# ------------------------------------------------------------------------------------------------------------
# Styles (just enough to resolve hidden-text properties, outline levels and numbering through style chains)
# ------------------------------------------------------------------------------------------------------------

class Styles:
    def __init__(self, pkg: Package):
        self.by_id: dict[str, etree._Element] = {}
        self.defaults_rpr = None
        self.default_para = None
        if "word/styles.xml" in pkg.data:
            root = pkg.xml("word/styles.xml")
            self.defaults_rpr = root.find("w:docDefaults/w:rPrDefault/w:rPr", {"w": W_NS})
            for s in root.findall(w("style")):
                sid = s.get(w("styleId"))
                if sid:
                    self.by_id[sid] = s
                if s.get(w("type")) == "paragraph" and s.get(w("default")) in ("1", "true", "on"):
                    self.default_para = sid

    def chain(self, sid):
        seen = set()
        while sid and sid in self.by_id and sid not in seen:
            seen.add(sid)
            yield self.by_id[sid]
            b = self.by_id[sid].find(w("basedOn"))
            sid = b.get(w("val")) if b is not None else None

    def para_prop(self, pstyle, tag):
        for s in self.chain(pstyle or self.default_para):
            el = s.find("w:pPr/w:%s" % tag, {"w": W_NS})
            if el is not None:
                return el
        return None

    def run_prop(self, rpr, rstyle, pstyle, tag):
        """Effective run property element: direct, run style chain, paragraph style chain, doc defaults."""
        if rpr is not None:
            el = rpr.find(w(tag))
            if el is not None:
                return el
        for s in self.chain(rstyle):
            el = s.find("w:rPr/w:%s" % tag, {"w": W_NS})
            if el is not None:
                return el
        for s in self.chain(pstyle or self.default_para):
            el = s.find("w:rPr/w:%s" % tag, {"w": W_NS})
            if el is not None:
                return el
        if self.defaults_rpr is not None:
            return self.defaults_rpr.find(w(tag))
        return None


# ------------------------------------------------------------------------------------------------------------
# Paragraph walking
# ------------------------------------------------------------------------------------------------------------

@dataclass
class Para:
    story: str
    index: int
    text: str
    para_id: str | None = None
    style: str | None = None
    outline: int | None = None
    num: list | None = None
    bidi: bool = False
    mark: str | None = None
    hidden: list = field(default_factory=list)

    def to_dict(self):
        return asdict(self)


class _Walker:
    def __init__(self, pkg: Package, styles: Styles, mode: str):
        self.pkg, self.styles, self.mode = pkg, styles, mode
        self.textboxes: list[etree._Element] = []  # w:txbxContent found while walking, in order

    # inline ---------------------------------------------------------------------------------------------
    def para_text(self, p, pstyle):
        out: list[str] = []
        hidden: list[tuple[str, str]] = []
        self._inline(p, out, hidden, pstyle)
        return "".join(out), hidden

    def _keep(self, kind):
        if self.mode == "markup":
            return True
        return (kind == "add") == (self.mode == "accept")

    def _inline(self, el, out, hidden, pstyle):
        for c in el:
            tag = c.tag
            if not isinstance(tag, str):
                continue
            if tag == w("r"):
                self._run(c, out, hidden, pstyle)
            elif tag in REV_WRAP:
                o, cl, kind = REV_WRAP[tag]
                if not self._keep(kind):
                    self._collect_textboxes(c)
                    continue
                if self.mode == "markup":
                    sub: list[str] = []
                    self._inline(c, sub, hidden, pstyle)
                    if sub:
                        out.append(o + "".join(sub) + cl)
                else:
                    self._inline(c, out, hidden, pstyle)
            elif tag == w("sdt"):
                sc = c.find(w("sdtContent"))
                if sc is not None:
                    self._inline(sc, out, hidden, pstyle)
            elif tag in INLINE_CONTAINERS:
                self._inline(c, out, hidden, pstyle)
            elif tag == "{%s}oMath" % M_NS or tag == "{%s}oMathPara" % M_NS:
                out.append("".join(t.text or "" for t in c.iter("{%s}t" % M_NS)))
            elif tag == "{%s}AlternateContent" % MC_NS:
                ch = c.find("{%s}Choice" % MC_NS)
                if ch is not None:
                    self._inline(ch, out, hidden, pstyle)

    def _run(self, r, out, hidden, pstyle):
        rpr = r.find(w("rPr"))
        rs = rpr.find(w("rStyle")) if rpr is not None else None
        rstyle = rs.get(w("val")) if rs is not None else None
        buf: list[str] = []
        for c in r:
            tag = c.tag
            if not isinstance(tag, str) or tag == w("rPr"):
                continue
            if tag in (w("t"), w("delText")):
                buf.append(c.text or "")
            elif tag in (w("tab"), w("ptab")):
                buf.append("\t")
            elif tag == w("br"):
                if c.get(w("type")) not in BR_SKIP:
                    buf.append("\n")
            elif tag == w("cr"):
                buf.append("\n")
            elif tag == w("noBreakHyphen"):
                buf.append("‑")
            elif tag == w("sym"):
                try:
                    buf.append(chr(int(c.get(w("char"), ""), 16)))
                except ValueError:
                    pass
            elif tag in (w("drawing"), w("pict"), w("object")) or tag == "{%s}AlternateContent" % MC_NS:
                self._collect_textboxes(c)
        text = "".join(buf)
        if not text:
            return
        out.append(text)
        sp = self.styles
        reason = None
        if _onoff(sp.run_prop(rpr, rstyle, pstyle, "vanish")) or _onoff(sp.run_prop(rpr, rstyle, pstyle,
                                                                                         "specVanish")):
            reason = "vanish"
        else:
            col = sp.run_prop(rpr, rstyle, pstyle, "color")
            sz = sp.run_prop(rpr, rstyle, pstyle, "sz")
            if col is not None and (col.get(w("val")) or "").upper() == "FFFFFF":
                reason = "white"
            elif sz is not None and (sz.get(w("val")) or "").isdigit() and int(sz.get(w("val"))) <= 4:
                reason = "tiny"
            elif ZERO_WIDTH.search(text):
                reason = "zero_width"
        if reason:
            if hidden and hidden[-1][0] == reason and hidden[-1][2] == len(out) - 1:
                hidden[-1] = (reason, hidden[-1][1] + text, len(out))
            else:
                hidden.append((reason, text, len(out)))

    def _collect_textboxes(self, el):
        """Find text-box content under a drawing/pict, taking only the first Choice of mc:AlternateContent."""
        if el.tag == "{%s}AlternateContent" % MC_NS:
            el = el.find("{%s}Choice" % MC_NS)
            if el is None:
                return
        stack = [el]
        while stack:
            e = stack.pop()
            for c in reversed(list(e)):
                if not isinstance(c.tag, str):
                    continue
                if c.tag == "{%s}AlternateContent" % MC_NS:
                    ch = c.find("{%s}Choice" % MC_NS)
                    if ch is not None:
                        stack.append(ch)
                elif c.tag == w("txbxContent"):
                    self.textboxes.append(c)
                else:
                    stack.append(c)
        # keep document order: the stack walk above visits in order because children are pushed reversed

    # blocks ---------------------------------------------------------------------------------------------
    def blocks(self, container, story, counter, out_paras):
        """Walk block-level content. Returns the list of (Para, removed_mark, element) for this container."""
        items = []
        self._blocks(container, story, counter, items)
        return items

    def _blocks(self, container, story, counter, items):
        for c in container:
            tag = c.tag
            if not isinstance(tag, str):
                continue
            if tag == w("p"):
                items.append(self._para(c, story, counter))
            elif tag == w("tbl"):
                items.append(("table", None))
                for tr in c.findall(w("tr")):
                    trpr = tr.find(w("trPr"))
                    drop = False
                    if trpr is not None and self.mode != "markup":
                        if trpr.find(w("ins")) is not None and self.mode == "reject":
                            drop = True
                        if trpr.find(w("del")) is not None and self.mode == "accept":
                            drop = True
                    row_items: list = []
                    for tc in _row_cells(tr):
                        cell: list = []
                        self._blocks(tc, story, counter, cell)
                        row_items.append(("cell", _merge(cell, self.mode)))
                    if not drop:
                        items.extend(row_items)
                items.append(("table", None))
            elif tag == w("sdt"):
                sc = c.find(w("sdtContent"))
                if sc is not None:
                    self._blocks(sc, story, counter, items)
            elif tag == w("customXml"):
                self._blocks(c, story, counter, items)
            elif tag in REV_WRAP:  # block-level tracked change wrapper (rare)
                if self._keep(REV_WRAP[tag][2]):
                    self._blocks(c, story, counter, items)
            elif tag == "{%s}AlternateContent" % MC_NS:
                ch = c.find("{%s}Choice" % MC_NS)
                if ch is not None:
                    self._blocks(ch, story, counter, items)

    def _para(self, p, story, counter):
        idx = counter[0]
        counter[0] += 1
        ppr = p.find(w("pPr"))
        pstyle = None
        para = Para(story=story, index=idx, text="", para_id=p.get(W14_PARAID))
        mark = None
        kinds: list[str] = []
        if ppr is not None:
            ps = ppr.find(w("pStyle"))
            pstyle = ps.get(w("val")) if ps is not None else None
            para.style = pstyle
            ol = ppr.find(w("outlineLvl"))
            if ol is None:
                ol = self.styles.para_prop(pstyle, "outlineLvl")
            para.outline = int(ol.get(w("val"))) if ol is not None and (ol.get(w("val")) or "").isdigit() else None
            np_ = ppr.find(w("numPr"))
            if np_ is None:
                np_ = self.styles.para_prop(pstyle, "numPr")
            if np_ is not None and np_.find(w("numId")) is not None:
                il = np_.find(w("ilvl"))
                para.num = [int(np_.find(w("numId")).get(w("val"))), int(il.get(w("val"))) if il is not None else 0]
            para.bidi = _onoff(ppr.find(w("bidi")))
            mrpr = ppr.find(w("rPr"))
            if mrpr is not None:
                kinds = [k for k in ("ins", "del", "moveFrom", "moveTo") if mrpr.find(w(k)) is not None]
                mark = kinds[0] if kinds else None
        else:
            ol = self.styles.para_prop(None, "outlineLvl")
            para.outline = int(ol.get(w("val"))) if ol is not None else None
        para.mark = mark
        text, hidden = self.para_text(p, pstyle)
        if self.mode == "markup" and mark:
            text += MARK_TOKENS[mark]
        para.text = text
        para.hidden = [{"reason": r_, "text": t_} for r_, t_, _ in hidden]
        # A mark can carry both w:ins and w:del (inserted, then deleted): each view drops it.
        removed = self.mode != "markup" and any(
            (self.mode == "accept" and k in ("del", "moveFrom")) or
            (self.mode == "reject" and k in ("ins", "moveTo")) for k in kinds)
        return ("p", para, removed)


def _row_cells(tr):
    for c in tr:
        if c.tag == w("tc"):
            yield c
        elif c.tag == w("sdt"):
            sc = c.find(w("sdtContent"))
            if sc is not None:
                yield from _row_cells(sc)
        elif c.tag == w("customXml"):
            yield from _row_cells(c)


def _merge(items, mode):
    """Flatten block items into Paras, merging paragraphs whose mark is removed into the next paragraph."""
    out: list[Para] = []
    pending: Para | None = None
    for it in items:
        if it[0] == "cell":
            if pending is not None:
                out.append(pending)
                pending = None
            out.extend(it[1])
            continue
        if it[0] == "table":
            if pending is not None:
                out.append(pending)
                pending = None
            continue
        _, para, removed = it
        if pending is not None:
            para = Para(**{**para.to_dict(), "text": pending.text + para.text,
                           "hidden": pending.hidden + para.hidden})
            pending = None
        if removed:
            pending = para
        else:
            out.append(para)
    if pending is not None:
        out.append(pending)
    return out


# ------------------------------------------------------------------------------------------------------------
# Stories
# ------------------------------------------------------------------------------------------------------------

def story_parts(pkg: Package) -> list[tuple[str, str]]:
    """(story key prefix, part name) for the main part and its header/footer/notes/comments parts."""
    main = pkg.main_part()
    out = [("main", main)]
    rels = pkg.rels_of(main)
    hf = sorted(t for (typ, t, mode) in rels.values() if mode != "External" and typ.rsplit("/", 1)[-1] in
                ("header", "footer") and t in pkg.data)
    out += [("hf", t) for t in hf]
    for kind in ("footnotes", "endnotes", "comments"):
        for typ, t, mode in rels.values():
            if typ.rsplit("/", 1)[-1] == kind and t in pkg.data:
                out.append((kind, t))
    return out


def extract(src, mode: str = "markup") -> dict[str, list[Para]]:
    """{story: [Para, ...]} in one view."""
    if mode not in MODES:
        raise ValueError("mode must be one of %s" % (MODES,))
    pkg = src if isinstance(src, Package) else Package.open(src)
    styles = Styles(pkg)
    stories: dict[str, list[Para]] = {}

    def run_story(name, container, walker):
        counter = [0]
        items = walker.blocks(container, name, counter, None)
        stories[name] = _merge(items, mode)

    for kind, part in story_parts(pkg):
        root = pkg.xml(part)
        walker = _Walker(pkg, styles, mode)
        if kind in ("main", "hf"):
            body = root.find(w("body")) if kind == "main" else root
            if body is None:
                body = root
            run_story(part, body, walker)
        else:
            tag = {"footnotes": "footnote", "endnotes": "endnote", "comments": "comment"}[kind]
            for n in root.findall(w(tag)):
                if kind != "comments" and n.get(w("type")) not in (None, "normal"):
                    continue
                run_story("%s#%s" % (part, n.get(w("id"))), n, walker)
        if walker.textboxes:
            tb_items: list = []
            counter = [0]
            for tb in walker.textboxes:
                tbw = _Walker(pkg, styles, mode)
                tb_items.extend(tbw.blocks(tb, part + "#textbox", counter, None))
            stories[part + "#textbox"] = _merge(tb_items, mode)
    return stories


def texts(src, mode: str = "markup") -> dict[str, list[str]]:
    return {k: [p.text for p in v] for k, v in extract(src, mode).items()}


# ------------------------------------------------------------------------------------------------------------
# Facts: revisions, comments, fields, images, content controls, bookmarks
# ------------------------------------------------------------------------------------------------------------

REV_TAGS = ("ins", "del", "moveFrom", "moveTo", "rPrChange", "pPrChange", "tblPrChange", "trPrChange",
            "tcPrChange", "sectPrChange", "numberingChange", "cellIns", "cellDel", "cellMerge")


def facts(src) -> dict:
    pkg = src if isinstance(src, Package) else Package.open(src)
    parts = [p for _, p in story_parts(pkg)]
    roots = {p: pkg.xml(p) for p in parts}
    main = parts[0]
    rev = {k: 0 for k in REV_TAGS}
    authors: set[str] = set()
    for root in roots.values():
        for el in root.iter():
            if not isinstance(el.tag, str) or not el.tag.startswith("{%s}" % W_NS):
                continue
            local = el.tag.split("}", 1)[1]
            if local in rev:
                rev[local] += 1
            if local in REV_TAGS or local.endswith("RangeStart") and local.startswith("move"):
                a = el.get(w("author"))
                if a:
                    authors.add(a)
    rev = {k: v for k, v in rev.items() if v}
    rev["authors"] = sorted(authors)

    fields: list[str] = []
    bookmarks: list[str] = []
    sdt = 0
    images = {"inline": 0, "anchor": 0, "pictures": 0}
    alt = 0
    for p in parts:
        if p.endswith("comments.xml"):
            continue
        root = roots[p]
        fields += _fields(root)
        bookmarks += [b.get(w("name")) for b in root.iter(w("bookmarkStart"))]
        sdt += sum(1 for _ in root.iter(w("sdt")))
        alt += sum(1 for _ in root.iter("{%s}AlternateContent" % MC_NS))
        images["inline"] += sum(1 for _ in root.iter("{%s}inline" % WP_NS))
        images["anchor"] += sum(1 for _ in root.iter("{%s}anchor" % WP_NS))
        images["pictures"] += sum(1 for _ in root.iter("{%s}pic" % PIC_NS))

    notes = {}
    for kind in ("footnote", "endnote"):
        part = next((p for p in parts if p.endswith(kind + "s.xml")), None)
        notes[kind + "s"] = 0 if part is None else sum(
            1 for n in roots[part].findall(w(kind)) if n.get(w("type")) in (None, "normal"))

    return {
        "revisions": rev,
        "comments": _comments(pkg, roots, parts),
        "fields": fields,
        "bookmarks": bookmarks,
        "sdt": sdt,
        "images": images,
        "alternate_content": alt,
        "tables": sum(1 for _ in roots[main].iter(w("tbl"))),
        "macros": any(n.lower().endswith("vbaproject.bin") for n in pkg.names),
        **notes,
    }


def _fields(root) -> list[str]:
    """Field instructions in the order their fields begin (complex fields nest; fldSimple counts at its spot)."""
    out: list[str] = []
    stack: list[int] = []  # indices into out for open complex fields still collecting instrText
    collecting: list[bool] = []
    for el in root.iter(w("fldChar"), w("instrText"), w("fldSimple")):
        tag = el.tag
        if tag == w("fldSimple"):
            out.append(" ".join((el.get(w("instr")) or "").split()))
        elif tag == w("fldChar"):
            t = el.get(w("fldCharType"))
            if t == "begin":
                out.append("")
                stack.append(len(out) - 1)
                collecting.append(True)
            elif t == "separate" and collecting:
                collecting[-1] = False
            elif t == "end" and stack:
                stack.pop()
                collecting.pop()
        elif stack and collecting and collecting[-1]:
            out[stack[-1]] += el.text or ""
    return [" ".join(x.split()) for x in out]


def _plain(el) -> str:
    buf = []
    for c in el.iter(w("t"), w("delText"), w("tab")):
        buf.append("\t" if c.tag == w("tab") else (c.text or ""))
    return "".join(buf)


def _comments(pkg: Package, roots, parts) -> list[dict]:
    cpart = next((p for p in parts if p.endswith("comments.xml")), None)
    if cpart is None:
        return []
    croot = roots[cpart]
    last_pid: dict[str, str] = {}
    out = []
    for c in croot.findall(w("comment")):
        cid = c.get(w("id"))
        paras = c.findall(".//" + w("p"))
        if paras and paras[-1].get(W14_PARAID):
            last_pid[paras[-1].get(W14_PARAID)] = cid
        out.append({"id": cid, "author": c.get(w("author")), "text": "\n".join(_plain(p) for p in paras),
                    "anchor": None, "parent": None})
    by_id = {c["id"]: c for c in out}
    # parents from commentsExtended
    ext = next((n for n in pkg.names if n.lower().endswith("commentsextended.xml")), None)
    if ext:
        w15 = "http://schemas.microsoft.com/office/word/2012/wordml"
        pid_of = {v: k for k, v in last_pid.items()}  # comment id -> last paraId
        for ex in pkg.xml(ext).iter("{%s}commentEx" % w15):
            pp = ex.get("{%s}paraIdParent" % w15)
            me = last_pid.get(ex.get("{%s}paraId" % w15))
            if pp and me in by_id:
                by_id[me]["parent"] = last_pid.get(pp)
        del pid_of
    # anchors: text between commentRangeStart/End, in every non-comment story
    for p in parts:
        if p == cpart:
            continue
        open_ids: dict[str, list[str]] = {}
        for el in roots[p].iter(w("commentRangeStart"), w("commentRangeEnd"), w("t"), w("delText"), w("tab")):
            if el.tag == w("commentRangeStart"):
                open_ids[el.get(w("id"))] = []
            elif el.tag == w("commentRangeEnd"):
                cid = el.get(w("id"))
                if cid in open_ids and cid in by_id:
                    by_id[cid]["anchor"] = "".join(open_ids.pop(cid))
            else:
                s = "\t" if el.tag == w("tab") else (el.text or "")
                for buf in open_ids.values():
                    buf.append(s)
    return out
