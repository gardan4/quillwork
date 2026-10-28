"""Generate a deterministic public business PPTX template, using only the standard library.

This is synthetic test data, not a slide design/rendering API. All slide text, notes,
styles and relationships are explicitly authored here so preservation can be tested.
"""

from __future__ import annotations

import sys
import zipfile
from pathlib import Path

P = "http://schemas.openxmlformats.org/presentationml/2006/main"
A = "http://schemas.openxmlformats.org/drawingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
DECL = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
NS = f'xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}"'
GROUP = '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
COLORS = '<p:clrMap accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" bg1="lt1" bg2="lt2" folHlink="folHlink" hlink="hlink" tx1="dk1" tx2="dk2"/>'


def run(text: str, *, bold: bool = False, size: int = 2400) -> str:
    return f'<a:r><a:rPr lang="en-US" sz="{size}" b="{int(bold)}"><a:solidFill><a:srgbClr val="183548"/></a:solidFill><a:latin typeface="Arial"/></a:rPr><a:t xml:space="preserve">{text}</a:t></a:r>'


def shape(
    sid: int, name: str, paragraphs: list[str], *, y: int = 900000, title: bool = False
) -> str:
    ph = '<p:ph type="title"/>' if title else ""
    return (
        f'<p:sp><p:nvSpPr><p:cNvPr id="{sid}" name="{name}"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr>'
        f'<p:spPr><a:xfrm><a:off x="650000" y="{y}"/><a:ext cx="10800000" cy="1600000"/></a:xfrm>'
        '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/></p:spPr>'
        "<p:txBody><a:bodyPr/><a:lstStyle/>"
        + "".join(f'<a:p>{p}<a:endParaRPr lang="en-US"/></a:p>' for p in paragraphs)
        + "</p:txBody></p:sp>"
    )


def rels(items: list[tuple[str, str, str]]) -> str:
    return (
        DECL
        + '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        + "".join(
            f'<Relationship Id="{rid}" Type="{R}/{kind}" Target="{target}"/>'
            for rid, kind, target in items
        )
        + "</Relationships>"
    )


def build(path: Path) -> None:
    parts: dict[str, str] = {}
    overrides = {
        "ppt/presentation.xml": "presentationml.presentation.main",
        "ppt/slideMasters/slideMaster1.xml": "presentationml.slideMaster",
        "ppt/slideLayouts/slideLayout1.xml": "presentationml.slideLayout",
        "ppt/slides/slide1.xml": "presentationml.slide",
        "ppt/slides/slide2.xml": "presentationml.slide",
        "ppt/notesSlides/notesSlide1.xml": "presentationml.notesSlide",
        "ppt/notesMasters/notesMaster1.xml": "presentationml.notesMaster",
        "ppt/theme/theme1.xml": "theme",
    }
    parts["[Content_Types].xml"] = (
        DECL
        + '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>'
        + "".join(
            f'<Override PartName="/{part}" ContentType="application/vnd.openxmlformats-officedocument.{ctype}+xml"/>'
            for part, ctype in overrides.items()
        )
        + "</Types>"
    )
    parts["_rels/.rels"] = rels([("rId1", "officeDocument", "ppt/presentation.xml")])
    parts["ppt/presentation.xml"] = (
        DECL
        + f'<p:presentation {NS}><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst><p:notesMasterIdLst><p:notesMasterId r:id="rId4"/></p:notesMasterIdLst><p:sldSz cx="12192000" cy="6858000"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>'
    )
    parts["ppt/_rels/presentation.xml.rels"] = rels(
        [
            ("rId1", "slideMaster", "slideMasters/slideMaster1.xml"),
            ("rId2", "slide", "slides/slide1.xml"),
            ("rId3", "slide", "slides/slide2.xml"),
            ("rId4", "notesMaster", "notesMasters/notesMaster1.xml"),
        ]
    )
    parts["ppt/slideMasters/slideMaster1.xml"] = (
        DECL
        + f'<p:sldMaster {NS}><p:cSld name="Example company"><p:bg><p:bgPr><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree>{GROUP}</p:spTree></p:cSld>{COLORS}<p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle/><p:bodyStyle/><p:otherStyle/></p:txStyles></p:sldMaster>'
    )
    parts["ppt/slideMasters/_rels/slideMaster1.xml.rels"] = rels(
        [
            ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
            ("rId2", "theme", "../theme/theme1.xml"),
        ]
    )
    parts["ppt/slideLayouts/slideLayout1.xml"] = (
        DECL
        + f'<p:sldLayout {NS} type="blank" preserve="1"><p:cSld name="Business template"><p:spTree>{GROUP}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>'
    )
    parts["ppt/slideLayouts/_rels/slideLayout1.xml.rels"] = rels(
        [("rId1", "slideMaster", "../slideMasters/slideMaster1.xml")]
    )
    title = (
        run("Proposal for ", size=3600)
        + run("{{cli", bold=True, size=3600)
        + run("ent}}", bold=True, size=3600)
    )
    body = [
        run("Service period: ") + run("{{period}}", bold=True),
        run("Example company | Advisory services", size=1800),
    ]
    parts["ppt/slides/slide1.xml"] = (
        DECL
        + f"<p:sld {NS}><p:cSld><p:spTree>{GROUP}"
        + shape(2, "Client title", [title], title=True)
        + shape(3, "Service details", body, y=2800000)
        + "</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"
    )
    parts["ppt/slides/_rels/slide1.xml.rels"] = rels(
        [
            ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
            ("rId2", "notesSlide", "../notesSlides/notesSlide1.xml"),
        ]
    )
    cell = lambda text: (
        "<a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p>"
        + run(text)
        + "</a:p></a:txBody><a:tcPr/></a:tc>"
    )
    table = (
        '<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="4" name="Commercial terms"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="650000" y="2800000"/><a:ext cx="10800000" cy="1600000"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr/><a:tblGrid><a:gridCol w="5400000"/><a:gridCol w="5400000"/></a:tblGrid><a:tr h="800000">'
        + cell("Item")
        + cell("Amount")
        + '</a:tr><a:tr h="800000">'
        + cell("Advisory fee")
        + cell("{{fee}}")
        + "</a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"
    )
    parts["ppt/slides/slide2.xml"] = (
        DECL
        + f"<p:sld {NS}><p:cSld><p:spTree>{GROUP}"
        + shape(
            2,
            "Terms title",
            [run("Commercial terms", bold=True, size=3600)],
            title=True,
        )
        + table
        + "</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"
    )
    parts["ppt/slides/_rels/slide2.xml.rels"] = rels(
        [("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml")]
    )
    notes = shape(
        2, "Speaker notes", [run("Discuss the scope with {{client}}.")]
    ).replace("<p:nvPr/>", '<p:nvPr><p:ph type="body" idx="1"/></p:nvPr>')
    parts["ppt/notesSlides/notesSlide1.xml"] = (
        DECL
        + f"<p:notes {NS}><p:cSld><p:spTree>{GROUP}{notes}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:notes>"
    )
    parts["ppt/notesSlides/_rels/notesSlide1.xml.rels"] = rels(
        [
            ("rId1", "notesMaster", "../notesMasters/notesMaster1.xml"),
            ("rId2", "slide", "../slides/slide1.xml"),
        ]
    )
    parts["ppt/notesMasters/notesMaster1.xml"] = (
        DECL
        + f"<p:notesMaster {NS}><p:cSld><p:spTree>{GROUP}</p:spTree></p:cSld>{COLORS}<p:notesStyle/></p:notesMaster>"
    )
    parts["ppt/notesMasters/_rels/notesMaster1.xml.rels"] = rels(
        [("rId1", "theme", "../theme/theme1.xml")]
    )
    palette = {
        "dk1": "183548",
        "lt1": "FFFFFF",
        "dk2": "293E40",
        "lt2": "F0F4F4",
        "accent1": "287C83",
        "accent2": "B56E31",
        "accent3": "7C669E",
        "accent4": "49764D",
        "accent5": "B14D58",
        "accent6": "59849E",
        "hlink": "145CB5",
        "folHlink": "754597",
    }
    fill = '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
    parts["ppt/theme/theme1.xml"] = (
        DECL
        + f'<a:theme xmlns:a="{A}" name="Example company"><a:themeElements><a:clrScheme name="Company">'
        + "".join(
            f'<a:{name}><a:srgbClr val="{color}"/></a:{name}>'
            for name, color in palette.items()
        )
        + '</a:clrScheme><a:fontScheme name="Office"><a:majorFont><a:latin typeface="Arial"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="Arial"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="Company"><a:fillStyleLst>'
        + fill * 3
        + "</a:fillStyleLst><a:lnStyleLst>"
        + ('<a:ln w="9525">' + fill + '<a:prstDash val="solid"/></a:ln>') * 3
        + "</a:lnStyleLst><a:effectStyleLst>"
        + "<a:effectStyle><a:effectLst/></a:effectStyle>" * 3
        + "</a:effectStyleLst><a:bgFillStyleLst>"
        + fill * 3
        + "</a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>"
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, content in parts.items():
            info = zipfile.ZipInfo(name, (2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, content.encode())


if __name__ == "__main__":
    build(Path(sys.argv[1]) if len(sys.argv) > 1 else Path("business-template.pptx"))
