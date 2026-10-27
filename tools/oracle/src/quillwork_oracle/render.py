"""Render oracle: .docx -> PDF through LibreOffice in a local gotenberg container, PDF -> PNG pages via pdfium.

Start the container once (it only listens on loopback):

    docker run -d --name quillwork-gotenberg -p 127.0.0.1:39300:3000 --read-only \\
        --tmpfs /tmp --tmpfs /home/gotenberg gotenberg/gotenberg:8 gotenberg --api-timeout=120s

`QUILLWORK_GOTENBERG` overrides the URL (default http://127.0.0.1:39300). Documents never leave the machine.
"""
from __future__ import annotations

import io
import json
import os
import time
import urllib.error
import urllib.request
import uuid
import zipfile
from pathlib import Path

DEFAULT_URL = "http://127.0.0.1:39300"


class RenderError(Exception):
    """The document could not be rendered. `code` is stable: libreoffice.open_failed, oracle.not_a_zip, ..."""

    def __init__(self, code: str, message: str):
        super().__init__("%s: %s" % (code, message))
        self.code = code


def gotenberg_url() -> str:
    return os.environ.get("QUILLWORK_GOTENBERG", DEFAULT_URL).rstrip("/")


def available(url: str | None = None, timeout: float = 3) -> bool:
    try:
        with urllib.request.urlopen((url or gotenberg_url()) + "/health", timeout=timeout) as r:
            return r.status == 200
    except (urllib.error.URLError, OSError):
        return False


def to_pdf(docx: str | Path | bytes, *, url: str | None = None, timeout: float = 180, attempts: int = 3) -> bytes:
    """PDF bytes. LibreOffice inside gotenberg occasionally segfaults on a perfectly good file (and gotenberg
    then restarts it), so an HTTP 5xx is retried; only a file that fails every attempt is reported as unopenable."""
    data = docx if isinstance(docx, bytes) else Path(docx).read_bytes()
    name = "input.docm" if str(docx).lower().endswith(".docm") and not isinstance(docx, bytes) else "input.docx"
    # LibreOffice happily imports arbitrary bytes as plain text, so refuse non-packages up front
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            if "[Content_Types].xml" not in z.namelist():
                raise RenderError("oracle.no_content_types", "zip has no [Content_Types].xml")
    except zipfile.BadZipFile as e:
        raise RenderError("oracle.not_a_zip", str(e)) from None
    boundary = uuid.uuid4().hex
    body = b"".join([
        b"--%s\r\n" % boundary.encode(),
        b'Content-Disposition: form-data; name="files"; filename="%s"\r\n' % name.encode(),
        b"Content-Type: application/octet-stream\r\n\r\n", data, b"\r\n",
        b"--%s--\r\n" % boundary.encode(),
    ])
    req = urllib.request.Request((url or gotenberg_url()) + "/forms/libreoffice/convert", data=body, method="POST",
                                 headers={"Content-Type": "multipart/form-data; boundary=" + boundary})
    for attempt in range(1, attempts + 1):
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                pdf = r.read()
            break
        except urllib.error.HTTPError as e:
            detail = e.read()[:300].decode("utf-8", "replace")
            if e.code >= 500 and attempt < attempts:
                time.sleep(0.5 * attempt)
                continue
            if e.code >= 500 or e.code == 400:
                raise RenderError("libreoffice.open_failed", "LibreOffice failed to open the document (HTTP %d: %s)"
                                  % (e.code, detail.strip())) from None
            raise RenderError("gotenberg.http_%d" % e.code, detail) from None
        except (urllib.error.URLError, OSError) as e:
            raise RenderError("gotenberg.unreachable", "%s (%s)" % (e, url or gotenberg_url())) from None
    if not pdf.startswith(b"%PDF"):
        raise RenderError("libreoffice.no_pdf", "gotenberg answered without a PDF")
    return pdf


def pdf_pages(pdf: bytes, *, dpi: int = 96):
    """PIL images, one per page."""
    import pypdfium2 as pdfium

    doc = pdfium.PdfDocument(pdf)
    try:
        return [doc[i].render(scale=dpi / 72).to_pil().convert("RGB") for i in range(len(doc))]
    finally:
        doc.close()


def page_count(pdf: bytes) -> int:
    import pypdfium2 as pdfium

    doc = pdfium.PdfDocument(pdf)
    try:
        return len(doc)
    finally:
        doc.close()


def render(docx, out_dir: str | Path | None = None, *, dpi: int = 96, url: str | None = None) -> list:
    """Render every page. With out_dir, writes page-001.png ... and returns their paths; else returns PIL images."""
    images = pdf_pages(to_pdf(docx, url=url), dpi=dpi)
    if out_dir is None:
        return images
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    paths = []
    for i, im in enumerate(images, 1):
        p = out / ("page-%03d.png" % i)
        im.save(p)
        paths.append(p)
    return paths


# --------------------------------------------------------------------------------------------------------------
# Page comparison
# --------------------------------------------------------------------------------------------------------------

def _gray(im, size=None):
    import numpy as np

    im = im.convert("L")
    if size and im.size != size:
        im = im.resize(size)
    return np.asarray(im, dtype=np.float64)


def ssim(a, b) -> float:
    """Mean SSIM over 8x8 blocks (no external dependency beyond numpy)."""
    import numpy as np

    h, w = (min(a.shape[0], b.shape[0]) // 8) * 8, (min(a.shape[1], b.shape[1]) // 8) * 8
    if h == 0 or w == 0:
        return 1.0 if a.shape == b.shape and np.array_equal(a, b) else 0.0
    a = a[:h, :w].reshape(h // 8, 8, w // 8, 8).swapaxes(1, 2).reshape(-1, 64)
    b = b[:h, :w].reshape(h // 8, 8, w // 8, 8).swapaxes(1, 2).reshape(-1, 64)
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    ma, mb = a.mean(1), b.mean(1)
    va, vb = a.var(1), b.var(1)
    cov = ((a - ma[:, None]) * (b - mb[:, None])).mean(1)
    s = ((2 * ma * mb + c1) * (2 * cov + c2)) / ((ma ** 2 + mb ** 2 + c1) * (va + vb + c2))
    return float(s.mean())


def compare_pages(pages_a, pages_b, *, threshold: int = 32) -> list[dict]:
    """Per-page scores. `ssim` 1.0 = identical; `diff_ratio` = share of pixels differing by more than threshold.
    Pages missing on one side score 0."""
    import numpy as np

    out = []
    for i in range(max(len(pages_a), len(pages_b))):
        if i >= len(pages_a) or i >= len(pages_b):
            out.append({"page": i + 1, "ssim": 0.0, "diff_ratio": 1.0, "missing": "a" if i >= len(pages_a) else "b"})
            continue
        ga = _gray(pages_a[i])
        gb = _gray(pages_b[i], size=pages_a[i].size)
        diff = float((np.abs(ga - gb) > threshold).mean())
        out.append({"page": i + 1, "ssim": round(ssim(ga, gb), 5), "diff_ratio": round(diff, 6)})
    return out


def render_diff(a, b, *, dpi: int = 72, url: str | None = None) -> dict:
    pa = pdf_pages(to_pdf(a, url=url), dpi=dpi)
    pb = pdf_pages(to_pdf(b, url=url), dpi=dpi)
    pages = compare_pages(pa, pb)
    return {"pages_a": len(pa), "pages_b": len(pb), "pages": pages,
            "min_ssim": min((p["ssim"] for p in pages), default=1.0)}


if __name__ == "__main__":  # pragma: no cover
    import sys

    print(json.dumps(render_diff(sys.argv[1], sys.argv[2]), indent=1))
