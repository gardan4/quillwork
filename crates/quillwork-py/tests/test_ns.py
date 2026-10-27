"""quillwork.ns mirrors core xml::ns."""

from __future__ import annotations

from quillwork import _native, ns


def test_constants_match_core() -> None:
    core = _native.conventional_namespaces()
    assert list(ns.CONVENTIONAL.items()) == core
    for prefix, uri in core:
        assert getattr(ns, prefix.upper()) == uri
