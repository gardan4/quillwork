"""Real native PPTX API, template generation, candidate identity and safe failures."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import zipfile
from pathlib import Path

import pytest
import quillwork
from conftest import REPO, raw_entries
from jsonschema import Draft202012Validator
from quillwork.errors import ConflictError


@pytest.fixture
def source(tmp_path: Path) -> bytes:
    spec = importlib.util.spec_from_file_location(
        "gen_presentation", REPO / "corpus/synthetic/gen_presentation.py"
    )
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    path = tmp_path / "business.pptx"
    module.build(path)
    first = path.read_bytes()
    module.build(path)
    assert path.read_bytes() == first
    return first


def rewrite(data: bytes, part: str, old: bytes, new: bytes) -> bytes:
    output = io.BytesIO()
    with zipfile.ZipFile(io.BytesIO(data)) as src, zipfile.ZipFile(output, "w") as dst:
        for info in src.infolist():
            value = src.read(info)
            dst.writestr(
                info, value.replace(old, new) if info.filename == part else value
            )
    return output.getvalue()


def test_presentation_round_trip_and_discovery(source: bytes) -> None:
    deck = quillwork.Presentation.open(source)
    assert deck.save() == source and deck.version == 1
    model = deck.describe()
    assert model["sha256"] == hashlib.sha256(source).hexdigest()
    assert [s["id"] for s in model["slides"]] == ["s256", "s257"]
    assert "[s256.sh2.p1]" in deck.read()
    assert "s256.n.sh2.p1 NOTES" in deck.read()
    assert "s257.sh4.r2.c2.p1" in deck.read()
    assert deck.review()["changes"] == []


def test_custom_package_limit_applies_to_later_mutations(source: bytes) -> None:
    with zipfile.ZipFile(io.BytesIO(source)) as archive:
        largest = max(info.file_size for info in archive.infolist())
    deck = quillwork.Presentation.open(
        source, limits=quillwork.Limits(max_part_size=largest + 10)
    )
    before = deck.describe()["sha256"]
    with pytest.raises(quillwork.LimitError):
        deck.apply(
            {
                "base_version": 1,
                "ops": [
                    {
                        "op": "replace_text",
                        "anchor": "s256.sh2.p1",
                        "find": "Proposal",
                        "replace": "x" * (largest * 2),
                    }
                ],
            }
        )
    assert deck.save() == source and deck.version == 1
    assert deck.describe()["sha256"] == before


def test_fill_preserves_run_properties_and_unrelated_raw_entries(source: bytes) -> None:
    deck = quillwork.Presentation.open(source)
    result = deck.fill(
        {"client": "Zoë & 李", "period": "Q4", "fee": "EUR 4,500"},
        expected_counts={"client": 2, "period": 1, "fee": 1},
    )
    candidate, review = deck.save(), deck.review()
    assert result["version"] == 2 and len(review["changes"]) == 4
    assert review["candidate_sha256"] == hashlib.sha256(candidate).hexdigest()
    assert review["source_sha256"] == hashlib.sha256(source).hexdigest()
    changed = {c["part"] for c in review["changes"]}
    before, after = raw_entries(source), raw_entries(candidate)
    assert before.keys() == after.keys()
    assert all(before[name] == after[name] for name in before if name not in changed)
    assert changed == {
        "ppt/slides/slide1.xml",
        "ppt/slides/slide2.xml",
        "ppt/notesSlides/notesSlide1.xml",
    }
    with zipfile.ZipFile(io.BytesIO(candidate)) as archive:
        xml = archive.read("ppt/slides/slide1.xml")
        assert b"Zo\xc3\xab &amp; \xe6\x9d\x8e" in xml
        assert b'<a:rPr lang="en-US" sz="3600" b="1">' in xml
        assert b'<a:t xml:space="preserve">Proposal for </a:t>' in xml
    reopened = quillwork.Presentation.open(candidate)
    assert "Zoë & 李" in reopened.read() and "{{client}}" not in reopened.read()


@pytest.mark.parametrize(
    "values,counts",
    [
        ({"client": "X"}, None),
        ({"client": "X", "period": "Q4", "fee": "100", "unused": "X"}, None),
        (
            {"client": "X", "period": "Q4", "fee": "100"},
            {"client": 1, "period": 1, "fee": 1},
        ),
        ({"client": "X", "period": "Q4", "fee": "100"}, {"client": 2}),
        (
            {"client": "X", "period": "Q4", "fee": "100"},
            {"client": True, "period": 1, "fee": 1},
        ),
        ({}, None),
    ],
)
def test_fill_refuses_missing_unused_and_wrong_counts_atomically(
    source: bytes, values: dict, counts: dict | None
) -> None:
    deck = quillwork.Presentation.open(source)
    with pytest.raises(quillwork.InvalidOpError):
        deck.fill(values, expected_counts=counts)
    assert (
        deck.save() == source and deck.version == 1 and deck.review()["changes"] == []
    )


def test_repeated_slots_and_values_are_not_recursively_expanded(source: bytes) -> None:
    source = rewrite(
        source, "ppt/slides/slide1.xml", b"Service period: ", b"{{period}} - "
    )
    deck = quillwork.Presentation.open(source)
    deck.fill(
        {"client": "{{period}}", "period": "Q4", "fee": "100"},
        expected_counts={"client": 2, "period": 2, "fee": 1},
    )
    text = deck.read()
    assert "Q4 - Q4" in text
    assert "Proposal for {{period}}" in text
    assert "Discuss the scope with {{period}}." in text


def test_failed_late_operation_and_stale_cross_session_hash_leave_state_unchanged(
    source: bytes,
) -> None:
    deck = quillwork.Presentation.open(source)
    model = deck.describe()
    op = {
        "op": "replace_text",
        "anchor": "s256.sh2.p1",
        "find": "{{client}}",
        "replace": "X",
    }
    with pytest.raises(quillwork.NotFoundError):
        deck.apply({"base_version": 1, "ops": [op, {**op, "anchor": "missing"}]})
    assert deck.save() == source and deck.version == 1
    deck.apply({"base_version": 1, "base_sha256": model["sha256"], "ops": [op]})
    again = quillwork.Presentation.open(deck.save())
    assert again.version == 1
    with pytest.raises(ConflictError):
        again.apply({"base_version": 1, "base_sha256": model["sha256"], "ops": [op]})


def test_transaction_contract_and_word_format_boundary(source: bytes) -> None:
    tx = {
        "spec_version": "0.1.0",
        "base_version": 1,
        "ops": [
            {
                "op": "replace_text",
                "anchor": "s256.sh2.p1",
                "find": "{{client}}",
                "replace": "X",
            }
        ],
    }
    schema = json.loads(
        (REPO / "spec/presentation/transaction.schema.json").read_text()
    )
    Draft202012Validator.check_schema(schema)
    Draft202012Validator(schema).validate(tx)
    assert quillwork.Presentation.open(source).apply(tx)["ok"]
    with pytest.raises(quillwork.BadPackageError):
        quillwork.Document.open(source)
    with pytest.raises(quillwork.BadPackageError):
        quillwork.Presentation.open(
            (REPO / "corpus/fixtures/01-run-formatting.docx").read_bytes()
        )


def test_invalid_json_and_unsupported_targets_are_errors(source: bytes) -> None:
    deck = quillwork.Presentation.open(source)
    with pytest.raises(quillwork.InvalidOpError):
        deck.apply("not json")
    source = rewrite(
        source,
        "ppt/slides/slide1.xml",
        b"Service period: ",
        b"Service period: </a:t></a:r><a:br/><a:r><a:t>",
    )
    deck = quillwork.Presentation.open(source)
    with pytest.raises(quillwork.UnsupportedError):
        deck.fill({"client": "X", "period": "Q4", "fee": "100"})
    assert deck.save() == source
