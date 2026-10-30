import io
import zipfile

from quillwork_oracle import text

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
CT = ('<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
      '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
      '<Override PartName="/word/document.xml" '
      'ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
RELS = ('<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        '<Relationship Id="rId1" Target="word/document.xml" Type="http://schemas.openxmlformats.org/'
        'officeDocument/2006/relationships/officeDocument"/></Relationships>')


def docx(body: str) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        z.writestr("[Content_Types].xml", CT)
        z.writestr("_rels/.rels", RELS)
        z.writestr("word/document.xml", '<w:document xmlns:w="%s"><w:body>%s</w:body></w:document>' % (W, body))
    return buf.getvalue()


def p(t: str) -> str:
    return "<w:p><w:r><w:t>%s</w:t></w:r></w:p>" % t


def tc(t: str, mark: str = "") -> str:
    pr = '<w:tcPr><w:%s w:id="1" w:author="a"/></w:tcPr>' % mark if mark else ""
    return "<w:tc>%s%s</w:tc>" % (pr, p(t))


def test_tracked_column_cells_follow_the_view():
    # One row: a plain cell, an inserted cell and a deleted cell.
    src = docx(p("before") + "<w:tbl><w:tr>" + tc("keep") + tc("new", "cellIns") + tc("old", "cellDel")
               + "</w:tr></w:tbl>" + p("after"))
    assert text.texts(src, "markup")["word/document.xml"] == ["before", "keep", "new", "old", "after"]
    assert text.texts(src, "accept")["word/document.xml"] == ["before", "keep", "new", "after"]
    assert text.texts(src, "reject")["word/document.xml"] == ["before", "keep", "old", "after"]
    # Paragraph indices are positions before any view, so a dropped cell still counts.
    assert [q.index for q in text.extract(src, "reject")["word/document.xml"]] == [0, 1, 3, 4]
