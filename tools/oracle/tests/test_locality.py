import shutil
import zipfile

import pytest

from conftest import FIXTURES
from quillwork_oracle.locality import locality, scan_paragraphs
from quillwork_oracle.ooxml import Package


def rewrite(src, dst, fn, part="word/document.xml"):
    """Copy a package entry by entry (same order and methods), passing one part's bytes through fn."""
    with zipfile.ZipFile(src) as zi, zipfile.ZipFile(dst, "w") as zo:
        for info in zi.infolist():
            data = zi.read(info)
            zo.writestr(info, fn(data) if info.filename == part else data, compress_type=info.compress_type)
    return dst


def flip_text_byte(data, para):
    """Change one character of the last w:t of outermost paragraph `para`."""
    s = scan_paragraphs(data)[para]
    at = s.start + data[s.start:s.end].rfind(b"</w:t>") - 1
    return data[:at] + (b"X" if data[at:at + 1] != b"X" else b"Y") + data[at + 1:]


FILES = ["01-run-formatting.docx", "04-tables.docx", "08-tracked-changes.docx", "16-libreoffice-style.docx"]


@pytest.mark.parametrize("name", FILES)
def test_untouched_copy_passes(tmp_path, name):
    src = FIXTURES / name
    r = locality(src, shutil.copy(src, tmp_path / name))
    assert r["ok"] and r["byte_identical"]
    r = locality(src, rewrite(src, tmp_path / "re.docx", lambda d: d))  # recompressed, same content
    assert r["ok"] and r["parts"]["changed"] == []


@pytest.mark.parametrize("name", FILES)
def test_one_byte_outside_intended_is_flagged(tmp_path, name):
    src = FIXTURES / name
    ed = rewrite(src, tmp_path / "ed.docx", lambda d: flip_text_byte(d, 1))
    ok = locality(src, ed, intended=[1])
    assert ok["ok"], ok["violations"]
    assert [s["index"] for s in ok["intended"]] == [1]
    assert ok["intended"][0]["changed_chars"] == 2 and ok["intended"][0]["ppr_preserved"]
    bad = locality(src, ed, intended=[0])
    assert not bad["ok"]
    assert [(v["kind"], v["index"]) for v in bad["violations"]] == [("paragraph_changed", 1)]


def test_intended_by_para_id(tmp_path):
    src = FIXTURES / "01-run-formatting.docx"
    pid = scan_paragraphs(Package.open(str(src)).data["word/document.xml"])[2].para_id
    ed = rewrite(src, tmp_path / "ed.docx", lambda d: flip_text_byte(d, 2))
    assert locality(src, ed, intended=[pid.lower()])["ok"]
    assert not locality(src, ed, intended=["7FFFFFFF"])["ok"]


def test_markup_between_paragraphs_flagged(tmp_path):
    src = FIXTURES / "01-run-formatting.docx"
    ed = rewrite(src, tmp_path / "ed.docx", lambda d: d.replace(b'<w:pgSz w:w="', b'<w:pgSz w:w="1', 1))
    r = locality(src, ed, intended=[0, 1, 2])
    assert [v["kind"] for v in r["violations"]] == ["markup_between_paragraphs_changed"]


def test_inserted_paragraph_next_to_intended(tmp_path):
    src = FIXTURES / "01-run-formatting.docx"

    def dup(d, k=1):
        s = scan_paragraphs(d)[k]
        return d[:s.end] + d[s.start:s.end].replace(b'w14:paraId="', b'w14:paraId="7', 1) + d[s.end:]

    ed = rewrite(src, tmp_path / "ed.docx", dup)
    assert locality(src, ed, intended=[1])["ok"]
    r = locality(src, ed, intended=[4])
    assert [v["kind"] for v in r["violations"]] == ["paragraph_inserted"]


def test_other_parts(tmp_path):
    src = FIXTURES / "05-headers-footers.docx"
    pkg = Package.open(str(src))
    header = next(n for n in pkg.names if n.startswith("word/header"))
    ed = rewrite(src, tmp_path / "ed.docx", lambda d: flip_text_byte(d, 0), part=header)
    r = locality(src, ed, intended=[0])  # index 0 of the main part, not of the header
    assert [v["kind"] for v in r["violations"]] == ["paragraph_changed"] and r["violations"][0]["part"] == header
    assert locality(src, ed, intended=["%s:0" % header])["ok"]
    styles = rewrite(src, tmp_path / "st.docx", lambda d: d.replace(b"Calibri", b"Calibre", 1), part="word/styles.xml")
    assert [v["kind"] for v in locality(src, styles)["violations"]] == ["part_changed"]
    assert locality(src, styles, allowed_parts=["word/styles.xml"])["ok"]


def test_tracked_change_stats(tmp_path):
    src = FIXTURES / "01-run-formatting.docx"
    d = Package.open(str(src)).data["word/document.xml"]
    s = scan_paragraphs(d)[0]
    para = d[s.start:s.end]
    # wrap the paragraph's last run in w:del and add a w:ins with replacement text
    r0 = para.rfind(b"<w:r>")
    r1 = para.rfind(b"</w:r>") + len(b"</w:r>")
    run = para[r0:r1]
    deleted = run.replace(b"<w:t>", b"<w:delText>").replace(b"<w:t xml:space=\"preserve\">", b"<w:delText xml:space=\"preserve\">").replace(b"</w:t>", b"</w:delText>")
    new = (para[:r0] + b'<w:del w:id="901" w:author="Q" w:date="2026-01-01T00:00:00Z">' + deleted + b"</w:del>"
           + b'<w:ins w:id="902" w:author="Q" w:date="2026-01-01T00:00:00Z"><w:r><w:t>new</w:t></w:r></w:ins>'
           + para[r1:])
    ed = rewrite(src, tmp_path / "ed.docx", lambda x: x[:s.start] + new + x[s.end:])
    r = locality(src, ed, intended=[0])
    assert r["ok"]
    st = r["intended"][0]
    assert st["reject_equals_original"] is True
    assert st["redline_chars"] >= 3 and st["changed_chars"] > 0


def test_pairs_by_para_id_in_replaced_block(tmp_path):
    src = FIXTURES / "01-run-formatting.docx"

    def ins_before_and_edit(d):
        d = flip_text_byte(d, 1)
        s = scan_paragraphs(d)[1]
        new = d[s.start:s.end].replace(b'w14:paraId="', b'w14:paraId="7', 1)
        return d[:s.start] + new + d[s.start:]

    ed = rewrite(src, tmp_path / "ed.docx", ins_before_and_edit)
    r = locality(src, ed, intended=[1])
    assert r["ok"], r["violations"]
    rec = r["paragraphs"]["word/document.xml"]
    assert [(c["index"], c["new_index"]) for c in rec["changed"]] == [(1, 2)]
    assert [x["new_index"] for x in rec["inserted"]] == [1]


def test_cli_smoke(tmp_path, capsys):
    from quillwork_oracle.cli import main

    src = FIXTURES / "01-run-formatting.docx"
    ed = rewrite(src, tmp_path / "ed.docx", lambda d: flip_text_byte(d, 1))
    assert main(["locality", str(src), str(ed), "-i", "1"]) == 0
    assert main(["locality", str(src), str(ed), "-i", "0"]) == 1
    assert main(["textdiff", str(src), str(ed)]) == 1
    assert main(["tokens", str(src)]) == 0
    assert main(["expect", str(src)]) == 0
    out = capsys.readouterr().out
    assert "VIOLATION" in out and "tokens" in out
