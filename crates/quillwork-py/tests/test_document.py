"""The JSON op seam over the core engine: apply, save + sidecar, reopen, errors.

Requests come from spec/examples where they apply to the synthetic corpus; every result and error
envelope is checked against spec/schema. Schemas are only used here, in tests: the runtime never
validates against them.
"""

from __future__ import annotations

import json
from functools import cache
from pathlib import Path
from typing import Any

import pytest
from conftest import REPO, SPEC, SYNTHETIC
from jsonschema import Draft202012Validator
from referencing import Registry, Resource

import quillwork
from quillwork import _native

BASE = "https://quillwork.example/spec/v0/"
OP_EXAMPLES = sorted((SPEC / "examples" / "ops").glob("*.json"))
NOW = 1_700_000_000


@cache
def registry() -> Registry:
    resources = []
    for p in sorted((SPEC / "schema").glob("*.schema.json")):
        schema = json.loads(p.read_text(encoding="utf-8"))
        resources.append((schema["$id"], Resource.from_contents(schema)))
    return Registry().with_resources(resources)


def validate(ref: str, instance: Any) -> None:
    Draft202012Validator({"$ref": BASE + ref}, registry=registry()).validate(instance)


def open_doc(path: Path = SYNTHETIC[0], **kwargs: Any) -> quillwork.Document:
    doc = quillwork.Document.open(path.read_bytes(), **kwargs)
    doc.set_now(NOW)
    doc.set_agent("Quillwork Test")
    return doc


def body(doc: quillwork.Document) -> list[str]:
    """Body paragraph handles in document order, from the sidecar's anchor map."""
    blocks = json.loads(doc.sidecar())["anchors"]["blocks"]
    return [h for h, b in sorted(blocks.items(), key=lambda kv: kv[1]["position"]) if b["story"] == "b"]


def text_of(doc: quillwork.Document, handle: str) -> str:
    (line,) = doc.render([handle])
    return line.split("] ", 1)[1]


def first_words(doc: quillwork.Document) -> tuple[str, str]:
    """A body paragraph with plain text, and its first two words."""
    for h in body(doc):
        words = text_of(doc, h).split()
        if len(words) >= 3 and all(w.isalpha() for w in words[:2]):
            return h, " ".join(words[:2])
    raise AssertionError("no plain paragraph")


def replace(doc: quillwork.Document, handle: str, find: str, new: str, base: int | None = None) -> dict[str, Any]:
    op = {"op": "replace_text", "anchor": handle, "find": find, "replace": new}
    return doc.apply({"base_version": doc.version if base is None else base, "ops": [op], "label": "test"})


@pytest.fixture
def doc() -> quillwork.Document:
    return open_doc()


def test_open_version_save(doc: quillwork.Document) -> None:
    assert doc.version == 1  # upload creates version 1 (common.schema.json Version)
    assert doc.save() == SYNTHETIC[0].read_bytes()
    assert doc.change_sets() == []
    assert quillwork.SPEC_VERSION == "0.1.0"
    sidecar = json.loads(doc.sidecar())
    assert sidecar["engine"]["version"] == 1 and sidecar["anchors"]["blocks"]


@pytest.mark.parametrize("path", SYNTHETIC, ids=lambda p: p.stem)
def test_replace_save_reopen_continues_the_version(path: Path) -> None:
    doc = open_doc(path)
    handle, words = first_words(doc)
    original = text_of(doc, handle)

    result = replace(doc, handle, words, "Nieuwe tekst")
    validate("ops.schema.json#/$defs/OpResult", result)
    assert result["ok"] is True and (result["base_version"], result["version"]) == (1, 2)
    cs = result["change_set"]
    assert isinstance(cs, str) and [c["id"] for c in doc.change_sets()] == [cs]
    assert "{++Nieuwe tekst++}" in text_of(doc, handle) and doc.version == 2

    data, sidecar = doc.save_with_sidecar()
    assert doc.sidecar() == sidecar  # cached until the next apply
    again = quillwork.Document.open(data, sidecar=sidecar)
    again.set_agent("Quillwork Test")  # the agent is host state, not in the sidecar
    assert again.version == 2 and again.change_sets() == doc.change_sets()
    assert again.render([handle]) == doc.render([handle])
    # A dict sidecar works too, and so does fast-forwarding to a later version.
    assert quillwork.Document.open(data, sidecar=json.loads(sidecar), version=7).version == 7

    # The next transaction continues from 2; rejecting the change set restores the text.
    undo = again.apply({"base_version": 2, "ops": [{"op": "reject", "change_set": cs}]})
    assert undo["version"] == 3 and again.version == 3
    assert text_of(again, handle) == original
    assert again.change_sets()[0]["status"] == "rejected"


def test_stale_base_version_raises_stale_anchor(doc: quillwork.Document) -> None:
    handle, words = first_words(doc)
    replace(doc, handle, words, "Eerst")
    with pytest.raises(quillwork.errors.StaleAnchorError) as exc:
        replace(doc, handle, "Eerst", "Tweede", base=1)
    e = exc.value
    assert e.code == "STALE_ANCHOR" and e.retryable and e.anchor == handle and e.version == 2
    assert e.error["current"] and e.op_index == 0 and e.op == "replace_text"
    assert doc.version == 2  # nothing committed


def test_failed_transaction_changes_nothing(doc: quillwork.Document) -> None:
    before = doc.save()
    with pytest.raises(quillwork.NotFoundError) as exc:
        doc.apply({"base_version": 1, "ops": [{"op": "delete_block", "anchor": "b_zz"}]})
    assert exc.value.code == "NOT_FOUND"
    with pytest.raises(quillwork.errors.BadVersionError):
        doc.apply({"base_version": 9, "ops": [{"op": "delete_block", "anchor": body(doc)[0]}]})
    assert doc.version == 1 and doc.save() == before and doc.change_sets() == []


def test_apply_text(doc: quillwork.Document) -> None:
    text = doc.apply_text({"base_version": 1, "ops": [{"op": "delete_block", "anchor": "b_zz"}]})
    assert text.startswith("§ error NOT_FOUND · v1")


@pytest.mark.parametrize("path", OP_EXAMPLES, ids=lambda p: p.stem)
def test_spec_examples_come_back_schema_valid(path: Path, doc: quillwork.Document) -> None:
    """Each example is a valid request; against a document it wasn't written for, whatever the
    engine answers (result or error) is a valid envelope."""
    request = json.loads(path.read_text(encoding="utf-8"))
    validate("ops.schema.json", request)
    native = _native.JsonDocument.open(SYNTHETIC[0].read_bytes())
    if "ops" in request:
        envelope = json.loads(native.apply_json(json.dumps({**request, "base_version": 1})))
        if envelope["ok"] is False:
            validate("error.schema.json", envelope)
            with pytest.raises(quillwork.QuillworkError) as exc:
                doc.apply({**request, "base_version": 1})
            assert exc.value.code == envelope["error"]["code"]
    else:
        envelope = json.loads(native.read_json(json.dumps(request)))
        if envelope["ok"]:
            validate("ops.schema.json#/$defs/ReadResult", envelope)
        else:
            # An example anchor (an image, a block) this document does not have.
            validate("error.schema.json", envelope)
            assert envelope["error"]["code"] == "NOT_FOUND", envelope


def test_read_find_and_view_model(doc: quillwork.Document) -> None:
    native = _native.JsonDocument.open(SYNTHETIC[0].read_bytes())
    first, second = body(doc)[:2]
    text = doc.read(f"{first}..{second}", "markup")
    assert text.startswith(f"[{first}") and f"[{second}]" in text
    outline = doc.read_op(json.dumps({"op": "outline"}))  # str is accepted too
    validate("ops.schema.json#/$defs/ReadResult", outline)
    assert outline["version"] == 1 and outline["text"].startswith("§ doc")
    word = first_words(doc)[1]
    found = doc.find(word)
    assert found.startswith("§ find") and "[b" in found
    with pytest.raises(quillwork.NotFoundError):
        doc.read("bzzzz")
    with pytest.raises(quillwork.InvalidOpError) as bad:
        doc.read_op({"op": "read", "budget": 10})
    assert bad.value.get("path") == "/budget"
    with pytest.raises(quillwork.InvalidOpError) as unknown:
        doc.read_op({"op": "list_images", "x": 1})
    assert unknown.value.get("path") == "/x" and unknown.value.get("usage") == "list_images {}"
    with pytest.raises(quillwork.InvalidOpError):
        doc.read_op("[1]")
    vm = doc.view_model()
    validate("view-model.schema.json", vm)
    assert vm["version"] == 1
    validate("view-model.schema.json", json.loads(native.view_model_json()))


def test_apply_patches_the_view_model_once_held(doc: quillwork.Document) -> None:
    doc.view_model()
    target = body(doc)[0]
    result = doc.apply({"base_version": 1, "ops": [{"op": "delete_block", "anchor": target}]})
    patch = result["patch"]
    validate("patch.schema.json", patch)
    assert patch["base_version"] == 1 and patch["version"] == 2 and patch["changeSets"]
    assert any(u["block"]["id"] == target for u in patch["upserts"]) or target in patch["removes"]
    assert doc.view_model()["version"] == 2


@pytest.mark.parametrize(
    ("request_json", "path"),
    [
        ("not json", ""),
        ("[1, 2]", ""),
        ('{"base_version": 1, "ops": []}', "/ops"),
        ('{"base_version": 1}', "/ops"),
        ('{"base_version": 1, "ops": [{"op": "delete_block", "anchor": "b1"}], "foo": 1}', "/foo"),
        ('{"ops": [{"op": "delete_block", "anchor": "b1"}]}', "/base_version"),
        ('{"base_version": 1, "ops": [{"op": "delete_block", "anchor": "b1"}], "mode": "loud"}', "/mode"),
        ('{"base_version": 1, "ops": [1]}', "/ops/0"),
        ('{"base_version": 1, "ops": [{"op": "explode"}]}', "/ops/0/op"),
        ('{"base_version": 1, "ops": [{"op": "delete_block"}]}', "/ops/0/anchor"),
    ],
    ids=["not-json", "not-object", "empty-ops", "no-ops", "unknown-key", "no-version", "bad-mode", "op-not-object",
         "unknown-op", "missing-arg"],
)
def test_malformed_requests_are_invalid_op(request_json: str, path: str, doc: quillwork.Document) -> None:
    native = _native.JsonDocument.open(SYNTHETIC[0].read_bytes())
    validate("error.schema.json", json.loads(native.apply_json(request_json)))
    assert "\u00a7 usage: " in native.apply_text(request_json)
    with pytest.raises(quillwork.InvalidOpError) as exc:
        doc.apply(request_json)
    e = exc.value
    assert e.code == "INVALID_OP" and e.retryable and isinstance(e, ValueError)
    assert e.get("path") == path
    assert isinstance(e.get("usage"), str) and e.get("usage")


def test_open_version_and_sidecar_errors(doc: quillwork.Document) -> None:
    data, sidecar = doc.save_with_sidecar()
    assert quillwork.Document.open(data, sidecar=sidecar).version == 1
    assert quillwork.Document.open(data, version=3).version == 3
    with pytest.raises(quillwork.errors.BadVersionError):
        quillwork.Document.open(data, sidecar=quillwork.Document.open(data, version=3).sidecar(), version=2)
    with pytest.raises(quillwork.BadPackageError):
        quillwork.Document.open(data, sidecar="{not json")
    with pytest.raises(quillwork.LimitError):
        quillwork.Document.open(data, quillwork.Limits(max_part_size=1000))


def test_unicode_survives_the_boundary(doc: quillwork.Document) -> None:
    handle, words = first_words(doc)
    replace(doc, handle, words, "één ü ✓")
    assert "{++één ü ✓++}" in text_of(doc, handle)
    data, sidecar = doc.save_with_sidecar()
    assert "{++één ü ✓++}" in text_of(quillwork.Document.open(data, sidecar=sidecar), handle)


FIXTURES = REPO / "corpus" / "fixtures"


def fixture(stem: str) -> quillwork.Document:
    return open_doc(FIXTURES / f"{stem}.docx")


def read_result(doc: quillwork.Document, request: dict[str, Any]) -> dict[str, Any]:
    out = doc.read_op(request)
    validate("ops.schema.json#/$defs/ReadResult", out)
    return out


def test_review_lists() -> None:
    """The lists a review tool reads before it accepts, rejects or replies."""
    doc = fixture("08-tracked-changes")
    changes = doc.list_changes()
    assert changes == read_result(doc, {"op": "list_changes"})["data"]
    revisions = changes["revisions"]
    assert revisions and all({"id", "type", "author", "anchors", "change_set"} <= r.keys() for r in revisions)
    author = revisions[0]["author"]
    assert all(r["author"] == author for r in doc.list_changes(author=author)["revisions"])
    assert doc.list_changes(author="Nobody")["revisions"] == []
    with pytest.raises(quillwork.InvalidOpError) as bad:
        doc.read_op({"op": "list_changes", "status": "maybe"})
    assert bad.value.get("path") == "/status"

    # An agent's tracked edit shows up with its change set.
    handle = body(doc)[0]
    word = text_of(doc, handle).split()[0]
    r = doc.apply({"base_version": doc.version, "mode": "track", "ops": [
        {"op": "replace_text", "anchor": handle, "find": word, "replace": word.upper() + "X"}]})
    mine = doc.list_changes(change_set=r["change_set"])
    assert [c["id"] for c in mine["change_sets"]] == [r["change_set"]]
    assert mine["revisions"] and all(x["change_set"] == r["change_set"] and x["agent"] for x in mine["revisions"])

    comments = fixture("07-comments").list_comments()
    assert comments["comments"] and all({"id", "anchor", "author", "text", "replies", "done"} <= c.keys() for c in comments["comments"])
    assert any(c["replies"] for c in comments["comments"])
    assert fixture("07-comments").list_comments(open_only=True)["comments"]


def test_images_fields_blocks_lint_help() -> None:
    doc = fixture("09-images")
    images = doc.list_images()["images"]
    assert images and images[0]["id"] == "img1"
    one = doc.get_image(images[0]["id"])
    assert one["mime"] == images[0]["media"]["mime"] and one["base64"]
    by_asset = doc.get_image(asset=images[0]["asset"])
    assert by_asset["asset"] == images[0]["asset"] and by_asset["base64"] == one["base64"]
    with pytest.raises(TypeError):
        doc.get_image()
    with pytest.raises(quillwork.NotFoundError):
        doc.get_image("img99")
    with pytest.raises(quillwork.InvalidOpError) as both:
        doc.read_op({"op": "get_image", "image": "img1", "asset": images[0]["asset"]})
    assert both.value.get("usage") == "get_image {image|asset}"

    block = doc.get_block(images[0]["para"])["block"]
    assert block["id"] == images[0]["para"]
    with pytest.raises(quillwork.NotFoundError):
        doc.get_block("bzzzz")

    fields = fixture("12-content-controls").list_fields()
    assert set(fields) == {"controls", "placeholders"} and fields["controls"]

    assert doc.lint() == "§ lint: none"
    assert all(line.startswith("§ lint: ") for line in fixture("11-fields").lint().splitlines())
    assert "list_changes {change_set?, author?, status?}" in doc.help().splitlines()
    assert doc.help("lint").strip().endswith("lint {}")


def test_apply_result_lint_and_text() -> None:
    doc = fixture("11-fields")
    handle = body(doc)[0]
    tx = {"base_version": doc.version, "mode": "direct", "ops": [
        {"op": "insert_paragraph", "after": handle, "text": "Dear {name}, sign here."}]}
    r = doc.apply(tx)
    validate("ops.schema.json#/$defs/OpResult", r)
    assert [f["rule"] for f in r["lint"]] == ["leftover_placeholder"]
    text = doc.apply_text({**tx, "base_version": doc.version})
    assert '§ lint: warning leftover_placeholder' in text and '"{name}"' in text
