"""Hostile and broken input surfaces as typed QuillworkError subclasses, never a crash."""

from __future__ import annotations

import io
import json
import zipfile
from pathlib import Path

import pytest
from conftest import SPEC, SYNTHETIC

import quillwork
from quillwork import errors


def docx_with(extra: dict[str, bytes]) -> bytes:
    """The first synthetic document plus extra deflated entries."""
    buf = io.BytesIO()
    with zipfile.ZipFile(io.BytesIO(SYNTHETIC[0].read_bytes())) as src, zipfile.ZipFile(
        buf, "w", zipfile.ZIP_DEFLATED
    ) as dst:
        for info in src.infolist():
            dst.writestr(info, src.read(info))
        for name, body in extra.items():
            dst.writestr(name, body)
    return buf.getvalue()


def set_encrypted_flag(data: bytes) -> bytes:
    """Set general-purpose bit 0 (traditional encryption) on every local and central header."""
    b = bytearray(data)
    for sig, at in ((b"PK\x03\x04", 6), (b"PK\x01\x02", 8)):
        i = b.find(sig)
        while i != -1:
            b[i + at] |= 1
            i = b.find(sig, i + 4)
    return bytes(b)


def encrypted_ole() -> bytes:
    """An OLE compound file carrying an EncryptedPackage stream (password-protected .docx)."""
    ole = bytearray(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1")
    ole.extend(bytes(512 - len(ole)))
    ole.extend("EncryptedPackage".encode("utf-16-le"))
    ole.extend(bytes(4096 - len(ole)))
    return bytes(ole)


@pytest.mark.parametrize(
    "data",
    [b"", b"garbage", b"PK\x03\x04" + bytes(100), bytes(range(256)) * 64],
    ids=["empty", "text", "truncated-zip", "noise"],
)
def test_garbage_is_bad_package(data: bytes) -> None:
    with pytest.raises(quillwork.BadPackageError) as exc:
        quillwork.Package.open(data)
    e = exc.value
    assert e.code == "BAD_PACKAGE" and e.retryable is False and e.message
    assert isinstance(e, quillwork.QuillworkError) and isinstance(e, ValueError)


def test_legacy_ole_is_bad_package() -> None:
    with pytest.raises(quillwork.BadPackageError):
        quillwork.Package.open(encrypted_ole()[:512])


def test_encrypted_ole() -> None:
    with pytest.raises(quillwork.EncryptedError) as exc:
        quillwork.Package.open(encrypted_ole())
    e = exc.value
    assert isinstance(e, quillwork.ProtectedError)
    assert e.code == "PROTECTED" and e.reason == "encrypted" and e.retryable is False
    assert e.error["reason"] == "encrypted"


def test_encrypted_zip_entries() -> None:
    with pytest.raises(quillwork.EncryptedError):
        quillwork.Package.open(set_encrypted_flag(SYNTHETIC[0].read_bytes()))


def test_compression_ratio_bomb() -> None:
    bomb = docx_with({"word/media/bomb.bin": bytes(20 * 1024 * 1024)})
    assert len(bomb) < 1024 * 1024
    with pytest.raises(quillwork.LimitError) as exc:
        quillwork.Package.open(bomb)
    assert exc.value.code == "LIMIT" and exc.value.retryable is True
    # Structured limit info is not reported by the core yet (docs/BINDING.md).
    assert exc.value.limit is None
    # Raised limits let it through.
    relaxed = quillwork.Limits(max_ratio=10_000)
    pkg = quillwork.Package.open(bomb, relaxed)
    assert len(pkg.part("word/media/bomb.bin")) == 20 * 1024 * 1024


@pytest.mark.parametrize(
    "limits",
    [
        quillwork.Limits(max_entries=3),
        quillwork.Limits(max_total_uncompressed=1000),
        quillwork.Limits(max_part_size=1000),
    ],
    ids=["entries", "total", "part"],
)
def test_size_limits(limits: quillwork.Limits) -> None:
    with pytest.raises(quillwork.LimitError):
        quillwork.Package.open(SYNTHETIC[0].read_bytes(), limits)


def test_document_open_maps_errors_too() -> None:
    with pytest.raises(quillwork.EncryptedError):
        quillwork.Document.open(encrypted_ole())
    with pytest.raises(quillwork.BadPackageError):
        quillwork.Document.open(b"nope")


def test_hierarchy() -> None:
    assert issubclass(quillwork.XmlDoctypeError, quillwork.XmlParseError)
    assert issubclass(quillwork.XmlParseError, quillwork.XmlError)
    assert issubclass(quillwork.XmlError, quillwork.BadPackageError)
    assert issubclass(quillwork.XmlNodeError, quillwork.InvalidOpError)
    assert issubclass(quillwork.NotFoundError, LookupError)
    for cls in errors.BY_CODE.values():
        assert issubclass(cls, quillwork.QuillworkError)
    assert len(errors.BY_CODE) == 14


@pytest.mark.parametrize("path", sorted((SPEC / "examples" / "errors").glob("*.json")), ids=lambda p: p.stem)
def test_every_spec_error_example_maps_to_its_class(path: Path) -> None:
    envelope = json.loads(path.read_text(encoding="utf-8"))
    body = envelope["error"]
    e = errors.error_from_dict(body)
    assert type(e) is not quillwork.QuillworkError
    assert e.code == body["code"] == path.stem
    assert e.retryable == body["retryable"] and e.version == body["version"]
    assert e.op_index == body["op_index"] and e.message == body["message"]
    assert e.error == body
    for key, value in body.items():
        assert e.get(key) == value
    with pytest.raises(quillwork.QuillworkError):
        raise e
