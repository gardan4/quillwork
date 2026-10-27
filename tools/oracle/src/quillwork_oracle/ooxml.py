"""Shared OOXML helpers: namespaces, safe XML parsing, package reading."""
from __future__ import annotations

import io
import zipfile
from dataclasses import dataclass

from lxml import etree

W_NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
W14_NS = "http://schemas.microsoft.com/office/word/2010/wordml"
W15_NS = "http://schemas.microsoft.com/office/word/2012/wordml"
MC_NS = "http://schemas.openxmlformats.org/markup-compatibility/2006"
R_NS = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
WP_NS = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
A_NS = "http://schemas.openxmlformats.org/drawingml/2006/main"
PIC_NS = "http://schemas.openxmlformats.org/drawingml/2006/picture"
M_NS = "http://schemas.openxmlformats.org/officeDocument/2006/math"
V_NS = "urn:schemas-microsoft-com:vml"
PKG_REL_NS = "http://schemas.openxmlformats.org/package/2006/relationships"
CT_NS = "http://schemas.openxmlformats.org/package/2006/content-types"

NSMAP = {"w": W_NS, "w14": W14_NS, "w15": W15_NS, "mc": MC_NS, "r": R_NS, "wp": WP_NS, "a": A_NS, "pic": PIC_NS,
         "m": M_NS, "v": V_NS}


def w(tag: str) -> str:
    return "{%s}%s" % (W_NS, tag)


W14_PARAID = "{%s}paraId" % W14_NS

_PARSER = etree.XMLParser(resolve_entities=False, no_network=True, huge_tree=False, remove_blank_text=False,
                          load_dtd=False)


def parse_xml(data: bytes) -> etree._Element:
    return etree.fromstring(data, parser=_PARSER)


class NotADocx(Exception):
    pass


@dataclass
class Package:
    """The raw entries of an OPC package, in zip order."""
    names: list[str]
    data: dict[str, bytes]
    infos: dict[str, zipfile.ZipInfo]
    raw: bytes

    @classmethod
    def open(cls, src: str | bytes) -> "Package":
        raw = src if isinstance(src, bytes) else open(src, "rb").read()
        try:
            z = zipfile.ZipFile(io.BytesIO(raw))
        except zipfile.BadZipFile as e:
            raise NotADocx("not a zip file: %s" % e) from None
        names, data, infos = [], {}, {}
        with z:
            for info in z.infolist():
                names.append(info.filename)
                infos[info.filename] = info
                data[info.filename] = z.read(info)
        if "[Content_Types].xml" not in data:
            raise NotADocx("zip has no [Content_Types].xml")
        return cls(names, data, infos, raw)

    def xml(self, name: str) -> etree._Element:
        return parse_xml(self.data[name])

    def main_part(self) -> str:
        rels = self.xml("_rels/.rels")
        for rel in rels:
            if rel.get("Type", "").endswith("/officeDocument"):
                return rel.get("Target").lstrip("/")
        return "word/document.xml"

    def rels_of(self, part: str) -> dict[str, tuple[str, str, str]]:
        """rId -> (type, resolved target part name or external URL, target mode) for a part."""
        d, f = part.rsplit("/", 1) if "/" in part else ("", part)
        rels_name = (d + "/" if d else "") + "_rels/" + f + ".rels"
        out = {}
        if rels_name not in self.data:
            return out
        for rel in self.xml(rels_name):
            target, mode = rel.get("Target", ""), rel.get("TargetMode", "Internal")
            if mode != "External":
                target = resolve_target(d, target)
            out[rel.get("Id")] = (rel.get("Type", ""), target, mode)
        return out


def resolve_target(base_dir: str, target: str) -> str:
    if target.startswith("/"):
        return target[1:]
    parts = (base_dir.split("/") if base_dir else []) + target.split("/")
    out: list[str] = []
    for p in parts:
        if p == "..":
            if out:
                out.pop()
        elif p and p != ".":
            out.append(p)
    return "/".join(out)
