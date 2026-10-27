import json

import pytest

from conftest import FIXTURES, invalid_fixtures, valid_fixtures
from quillwork_oracle import expect, render

pytestmark = pytest.mark.skipif(not render.available(), reason="gotenberg not reachable (see render.py docstring)")


def _expected_pages(f):
    return json.loads(expect.sidecar(f).read_text(encoding="utf-8")).get("pages", 1)


@pytest.mark.parametrize("docx", valid_fixtures(), ids=lambda p: p.name)
def test_valid_fixtures_render(docx):
    assert render.page_count(render.to_pdf(docx)) == _expected_pages(docx)


@pytest.mark.parametrize("name,code", [("malformed-xml.docx", "libreoffice.open_failed"),
                                       ("missing-content-type.docx", "libreoffice.open_failed"),
                                       ("not-a-zip.docx", "oracle.not_a_zip")])
def test_unopenable_files_raise(name, code):
    with pytest.raises(render.RenderError) as e:
        render.to_pdf(FIXTURES / "invalid" / name)
    assert e.value.code == code


def test_render_diff_identical_and_different(tmp_path):
    a = FIXTURES / "01-run-formatting.docx"
    same = render.render_diff(a, a)
    assert same["pages_a"] == same["pages_b"] == 1
    assert same["min_ssim"] > 0.999
    other = render.render_diff(a, FIXTURES / "05-headers-footers.docx")
    assert other["pages_b"] == 3
    assert other["pages"][1]["ssim"] == 0.0 and other["pages"][1]["missing"] == "a"
    assert other["pages"][0]["ssim"] < same["pages"][0]["ssim"]


def test_render_writes_pngs(tmp_path):
    paths = render.render(FIXTURES / "05-headers-footers.docx", tmp_path, dpi=40)
    assert [p.name for p in paths] == ["page-001.png", "page-002.png", "page-003.png"]
    assert all(p.read_bytes().startswith(b"\x89PNG") for p in paths)
