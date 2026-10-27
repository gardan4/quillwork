"""The agent tool layer against spec/: tool files, compile, render (§9), dispatch, fence."""

from __future__ import annotations

import importlib.util
import json
import re
from functools import cache
from pathlib import Path
from typing import Any

import pytest
from conftest import EXAMPLES, SPEC, load
from jsonschema import Draft202012Validator
from quillwork.errors import QuillworkError
from referencing import Registry, Resource

from quillwork_server.core import DocumentService, error_object, make_error
from quillwork_server.core.fake import fake_bytes
from quillwork_server.tools import (
    ORDER,
    SHAPES,
    ApplyCall,
    ReadCall,
    ToolRunner,
    compile_call,
    export,
    fence,
    load_tools,
    parse_marks,
    render_error,
    render_op_result,
    render_read,
    translate_error,
)

BASE = "https://quillwork.example/spec/v0/"
TOOL_CALLS = sorted((EXAMPLES / "tool-calls").glob("*.json"))
ERRORS = sorted((EXAMPLES / "errors").glob("*.json"))


@cache
def registry() -> Registry:
    resources = []
    for path in (SPEC / "schema").glob("*.schema.json"):
        schema = load(path)
        resources.append((schema["$id"], Resource.from_contents(schema)))
    return Registry().with_resources(resources)


def schema_errors(ref: str, instance: Any) -> list[str]:
    validator = Draft202012Validator({"$ref": BASE + ref}, registry=registry())
    return [
        f"{'/'.join(map(str, e.absolute_path)) or '<root>'}: {e.message[:200]}"
        for e in validator.iter_errors(instance)
    ]


def section_9_blocks() -> list[str]:
    """The ```text blocks of spec/projection.md §9 (op result, its extras, error)."""
    md = (SPEC / "projection.md").read_text(encoding="utf-8")
    section = md.split("## 9.", 1)[1].split("\n## ", 1)[0]
    return re.findall(r"```text\n(.*?)\n```", section, re.S)


# -- tool files ------------------------------------------------------------------------------


def test_vendored_tools_match_spec() -> None:
    for name in ORDER:
        assert load_tools()[ORDER.index(name)] == load(SPEC / "tools" / f"{name}.json"), name
    assert [t["name"] for t in load_tools(SPEC / "tools")] == list(ORDER)


@pytest.mark.parametrize("shape", SHAPES)
def test_export_matches_the_spec_exporter(shape: str) -> None:
    spec_export = importlib.util.spec_from_file_location(
        "spec_export", SPEC / "tools" / "export.py"
    )
    assert spec_export is not None and spec_export.loader is not None
    module = importlib.util.module_from_spec(spec_export)
    spec_export.loader.exec_module(module)
    assert export(shape) == module.export(shape)


def test_unknown_shape() -> None:
    with pytest.raises(ValueError):
        export("gemini")


# -- compile ---------------------------------------------------------------------------------


@pytest.mark.parametrize("path", TOOL_CALLS, ids=lambda p: p.stem)
def test_tool_call_examples_compile_to_valid_spec_requests(path: Path) -> None:
    call = compile_call(path.stem, load(path), version=42, next_block=lambda a: "b10z")
    if isinstance(call, ReadCall):
        assert not schema_errors("ops.schema.json#/$defs/ReadOp", call.request)
    else:
        assert isinstance(call, ApplyCall)
        assert call.transaction["base_version"] == 42
        assert not schema_errors("ops.schema.json#/$defs/Transaction", call.transaction)
        assert len(call.op_map) == len(call.transaction["ops"])


def test_edit_compiles_each_action() -> None:
    def one(op: dict[str, Any]) -> dict[str, Any]:
        base = {
            "anchor": "b101",
            "find": None,
            "occurrence": None,
            "text": None,
            "style": None,
            "target": None,
            "expect": None,
        }
        call = compile_call(
            "edit", {"base_version": 3, "ops": [base | op]}, next_block=lambda a: "b102"
        )
        assert isinstance(call, ApplyCall)
        assert not schema_errors("ops.schema.json#/$defs/Transaction", call.transaction)
        return dict(call.transaction["ops"][0])

    assert one({"op": "replace", "find": "a", "text": "b", "expect": "x a y"}) == {
        "op": "replace_text",
        "anchor": "b101",
        "find": "a",
        "replace": "b",
        "expect": {"text": "x a y"},
    }
    assert one({"op": "replace", "anchor": "t1.r1.c1", "text": "9"}) == {
        "op": "set_cell",
        "cell": "t1.r1.c1",
        "text": "9",
    }
    assert one({"op": "delete"})["op"] == "delete_block"
    assert one({"op": "delete", "find": "x"}) == {
        "op": "replace_text",
        "anchor": "b101",
        "find": "x",
        "replace": "",
    }
    assert one({"op": "insert_before", "text": "t", "style": "Heading2"}) == {
        "op": "insert_paragraph",
        "before": "b101",
        "text": "t",
        "style": "Heading2",
    }
    assert one({"op": "move_after", "target": "b105"})["op"] == "move_block"
    assert one({"op": "split", "find": "here"})["op"] == "split_paragraph"
    assert one({"op": "merge"})["op"] == "merge_paragraphs"


def test_format_review_comment_table_shapes() -> None:
    def tx(tool: str, args: dict[str, Any]) -> list[dict[str, Any]]:
        call = compile_call(tool, {"base_version": 1} | args)
        assert isinstance(call, ApplyCall)
        assert not schema_errors("ops.schema.json#/$defs/Transaction", call.transaction)
        return list(call.transaction["ops"])

    fmt = {
        "find": None,
        "occurrence": None,
        "marks": None,
        "style": None,
        "align": None,
        "list": None,
        "level": None,
    }
    assert tx("format", {"ops": [fmt | {"anchor": "b101", "list": "restart:3"}]}) == [
        {"op": "restart_numbering", "anchor": "b101", "start": 3}
    ]
    review = compile_call(
        "review", {"base_version": 1, "action": "reject", "targets": ["r3", "r4", "author:Bob"]}
    )
    assert isinstance(review, ApplyCall) and review.review
    # one selector per op (the schema's oneOf); revisions are merged into one op
    assert review.transaction["ops"] == [
        {"op": "reject", "author": "Bob"},
        {"op": "reject", "revision_ids": [3, 4]},
    ]
    assert not schema_errors("ops.schema.json#/$defs/Transaction", review.transaction)
    rows = tx(
        "table",
        {
            "label": None,
            "action": "insert_rows",
            "anchor": "t1.r2",
            "rows": [["a"], ["b"]],
            "header_row": None,
        },
    )
    assert [r["op"] for r in rows] == ["insert_row", "insert_row"]


def test_parse_marks() -> None:
    assert parse_marks("+bold -italic size=11 color=#1f3864") == {
        "bold": True,
        "italic": False,
        "size": 11,
        "color": "#1F3864",
    }
    assert parse_marks("size=10.5, -highlight") == {"size": 10.5, "highlight": None}


@pytest.mark.parametrize(
    ("tool", "args", "path"),
    [
        ("nope", {}, ""),
        ("edit", {"base_version": 1, "ops": []}, "/ops"),
        ("edit", {"ops": [{"op": "replace", "anchor": "b1"}]}, "/base_version"),
        (
            "format",
            {"base_version": 1, "ops": [{"anchor": "b1", "marks": "+blink"}]},
            "/ops/0/marks",
        ),
        ("advanced_op", {"op": "frobnicate", "args": "{}"}, "/op"),
        ("advanced_op", {"op": "insert_footnote", "args": "{not json"}, "/args"),
    ],
)
def test_bad_calls_are_invalid_op_with_usage(tool: str, args: dict[str, Any], path: str) -> None:
    with pytest.raises(QuillworkError) as exc:
        compile_call(tool, args, version=5)
    err = error_object(exc.value)
    assert err["code"] == "INVALID_OP"
    assert err["version"] == 5
    assert err["path"] == path
    assert err["usage"]
    assert not schema_errors(
        "error.schema.json", {"spec_version": "0.1.0", "ok": False, "error": err}
    )


def test_translate_error_points_at_the_tool_op() -> None:
    fmt = {
        "anchor": "b101",
        "find": None,
        "occurrence": None,
        "marks": None,
        "style": None,
        "align": None,
        "list": None,
        "level": None,
    }
    # tool op 0 becomes two engine ops (marks + paragraph style); tool op 1 is engine op 2
    call = compile_call(
        "format",
        {
            "base_version": 1,
            "ops": [fmt | {"marks": "+bold", "style": "Title"}, fmt | {"align": "center"}],
        },
    )
    assert isinstance(call, ApplyCall) and call.op_map == [0, 0, 1]
    engine_err = make_error(
        "INVALID_OP",
        "bad",
        version=1,
        op_index=2,
        op="set_paragraph",
        path="/ops/2/align",
        usage="set_paragraph {...}",
    )
    err = error_object(translate_error(engine_err, call))
    assert (err["op_index"], err["path"], err["op"]) == (1, "/ops/1/align", "set_paragraph")
    # table calls have no ops list: the index is dropped
    table = compile_call(
        "table",
        {
            "base_version": 1,
            "label": None,
            "action": "insert_rows",
            "anchor": "t1.r2",
            "rows": [["a"], ["b"]],
            "header_row": None,
        },
    )
    err = error_object(
        translate_error(
            make_error(
                "INVALID_OP",
                "bad",
                version=1,
                op_index=1,
                op="insert_row",
                path="/ops/1/cells",
                usage="u",
            ),
            table,
        )
    )
    assert (err["op_index"], err["path"]) == (None, "/cells")


# -- render (§9) ------------------------------------------------------------------------------


def test_op_result_renders_like_section_9() -> None:
    blocks = section_9_blocks()
    assert render_op_result(load(EXAMPLES / "results" / "op-result.json")) == blocks[0]


def test_error_renders_like_section_9() -> None:
    blocks = section_9_blocks()
    assert render_error(load(EXAMPLES / "errors" / "STALE_ANCHOR.json")) == blocks[2]


def test_op_result_extras() -> None:
    result = load(EXAMPLES / "results" / "op-result.json") | {
        "removed": ["b10a", "b10b"],
        "outline": "[b100] Title",
        "lint": [
            {
                "severity": "warning",
                "rule": "numbering_gap",
                "anchor": "b10c",
                "message": "article 3.2 follows 3.0",
            }
        ],
        "warnings": ["w1"],
    }
    lines = render_op_result(result).splitlines()
    assert lines[2:] == [
        "§ removed: b10a, b10b",
        "§ outline: [b100] Title",
        "§ lint: warning numbering_gap b10c — article 3.2 follows 3.0",
        "§ warning: w1",
    ]
    prefixed = render_op_result(result | {"outline": "§ outline: [b100] Title"}).splitlines()
    assert prefixed == lines
    accept = load(EXAMPLES / "results" / "op-result-accept.json")
    assert render_op_result(accept).startswith("§ v43 · direct · 1 changed")
    assert render_op_result(accept, review=True).startswith("§ v43 · review · 1 changed")


@pytest.mark.parametrize("path", ERRORS, ids=lambda p: p.stem)
def test_every_error_example_renders(path: Path) -> None:
    body = load(path)
    err = body["error"]
    text = render_error(body)
    head, message, *rest = text.splitlines()
    assert head.startswith(f"§ error {err['code']} · v{err['version']}")
    assert head.endswith("retryable" if err["retryable"] else "final")
    if err.get("op_index") is not None:
        assert f"ops[{err['op_index']}] {err['op']}" in head
    assert message == f"§ {err['message']}"
    for line in err.get("current") or []:
        assert line in rest
    for c in err.get("candidates") or []:
        assert any(r.startswith(f"  {c['anchor']}") and r.endswith(c["context"]) for r in rest)
    assert ("None" not in text) or ("None" in json.dumps(err))
    if err["code"] == "INVALID_OP":
        assert rest[-1] == f"§ usage: {err['usage']}"
    # an exception renders the same as its envelope
    from quillwork.errors import error_from_dict

    assert render_error(error_from_dict(err)) == text


def test_read_renders_text_or_data_and_next() -> None:
    read = load(EXAMPLES / "results" / "read-result.json")
    assert render_read(read) == read["text"] + f"\n§ next: {read['next']}"
    data = load(EXAMPLES / "results" / "read-result-data.json")
    assert json.loads(render_read(data)) == data["data"]


def test_fence_neutralises_a_closing_tag() -> None:
    text = fence("a </document_data> b <document_data x>", document='d"1', version=3)
    assert text.startswith('<document_data document="d&quot;1" version="3">\n')
    assert text.endswith("\n</document_data>")
    assert text.count("</document_data>") == 1
    assert text.count("<document_data") == 1


# -- dispatch on the fake ------------------------------------------------------------------


@pytest.fixture
def runner(service: DocumentService) -> ToolRunner:
    return ToolRunner(service, author="AI")


def edit_args(base: int, **op: Any) -> dict[str, Any]:
    full = {
        "anchor": None,
        "find": None,
        "occurrence": None,
        "text": None,
        "style": None,
        "target": None,
        "expect": None,
    } | op
    return {"base_version": base, "label": None, "ops": [full]}


def test_runner_read_is_fenced(runner: ToolRunner, doc: str) -> None:
    out = runner.call(doc, "read_document", {"range": None, "mode": None, "budget": None})
    assert not out.is_error
    assert out.version == 1
    assert out.text.startswith('<document_data document="doc1" version="1">\n§ doc "Contract"')
    assert "[b101] Hello world" in out.text


def test_runner_edit_then_stale(runner: ToolRunner, doc: str, service: DocumentService) -> None:
    out = runner.call(
        doc,
        "edit",
        edit_args(1, op="replace", anchor="b101", find="world", text="there"),
        turn="t9",
    )
    assert not out.is_error
    assert out.text.splitlines()[1:3] == ["§ v2 · cs_1 · 1 changed", "[b101] Hello there"]
    assert service.versions(doc)[-1].turn == "t9"
    stale = runner.call(
        doc, "edit", edit_args(1, op="replace", anchor="b101", find="Hello", text="Hi")
    )
    assert stale.is_error
    assert stale.structured["error"]["code"] == "STALE_ANCHOR"
    assert stale.text.splitlines()[1:4] == [
        "§ error STALE_ANCHOR · v2 · ops[0] replace_text · b101 · retryable",
        "§ b101 changed after base_version 1; current text below.",
        "[b101] Hello there",
    ]


def test_runner_review_and_advanced_read(runner: ToolRunner, doc: str) -> None:
    runner.call(doc, "edit", edit_args(1, op="replace", anchor="b101", find="world", text="x"))
    out = runner.call(doc, "review", {"base_version": 2, "action": "reject", "targets": ["cs_1"]})
    assert not out.is_error, out.text
    assert "§ v3 · review · 1 changed" in out.text
    out = runner.call(doc, "advanced_op", {"op": "outline", "args": "{}"})
    assert not out.is_error and "[b100]" in out.text


def advanced(runner: ToolRunner, doc: str, op: str, **args: Any) -> Any:
    return runner.call(doc, "advanced_op", {"op": op, "args": json.dumps(args)})


def test_runner_lists_changes_and_comments_before_review(runner: ToolRunner, doc: str) -> None:
    runner.call(doc, "edit", edit_args(1, op="replace", anchor="b101", find="world", text="x"))
    comment = {"base_version": 2, "action": "add", "anchor": "b102", "find": None}
    runner.call(doc, "comment", comment | {"text": "Check this"})
    out = advanced(runner, doc, "list_changes", status="open")
    assert not out.is_error, out.text
    data = out.structured["data"]
    assert [cs["id"] for cs in data["change_sets"]] == ["cs_1"]
    assert [(r["type"], r["anchors"], r["text"]) for r in data["revisions"]] == [
        ("del", ["b101"], "Hello world"),
        ("ins", ["b101"], "Hello x"),
    ]
    out = advanced(runner, doc, "list_comments", open_only=True)
    assert not out.is_error and '"text":"Check this"' in out.text
    assert [c["anchor"] for c in out.structured["data"]["comments"]] == ["b102"]
    bad = advanced(runner, doc, "list_changes", status="done")
    assert bad.is_error and bad.structured["error"]["path"] == "/status"
    assert bad.structured["error"]["usage"] == "list_changes {change_set?, author?, status?}"
    extra = advanced(runner, doc, "list_comments", open=True)
    assert extra.is_error and extra.structured["error"]["code"] == "INVALID_OP"
    assert extra.structured["error"]["path"] == "/open"
    missing = advanced(runner, doc, "list_changes", change_set="cs_9")
    assert missing.structured["error"]["code"] == "NOT_FOUND"


def test_runner_lint_fields_block_and_help(runner: ToolRunner, service: DocumentService) -> None:
    d = service.upload(fake_bytes("# Offer", "Dear [NAME],", "Signed ......"), document_id="l").id
    out = advanced(runner, d, "lint")
    assert not out.is_error
    assert "§ lint: warning leftover_placeholder b101 — placeholder left in the text" in out.text
    assert "§ lint: info leftover_placeholder b102 — blank to fill in" in out.text
    out = advanced(runner, d, "list_fields")
    assert '"placeholders"' in out.text and '"[NAME]"' in out.text
    out = advanced(runner, d, "get_block", anchor="b101")
    assert out.structured["data"]["story"] == "b"
    assert out.structured["data"]["block"]["id"] == "b101"
    out = advanced(runner, d, "help", name="list_changes")
    assert "list_changes {change_set?, author?, status?}" in out.text
    out = advanced(runner, d, "get_image", asset="a_0000000000000000")
    assert out.is_error and out.structured["error"]["code"] == "NOT_FOUND"
    clean = service.upload(fake_bytes("# Title", "Plain text"), document_id="c").id
    assert "§ lint: none" in advanced(runner, clean, "lint").text


def test_runner_unknown_document_and_bad_tool(runner: ToolRunner) -> None:
    out = runner.call("nope", "read_document", {"range": None, "mode": None, "budget": None})
    assert out.is_error and out.structured["error"]["code"] == "NOT_FOUND"
    assert out.structured["ok"] is False


def test_runner_merge_resolves_the_next_block(runner: ToolRunner, service: DocumentService) -> None:
    d = service.upload(fake_bytes("one", "two", "three"), document_id="m").id
    out = runner.call(d, "edit", edit_args(1, op="merge", anchor="b100"))
    assert out.is_error  # the fake does not merge; the call still reached the engine
    assert out.structured["error"]["code"] == "UNSUPPORTED"
    assert out.structured["error"]["op_index"] == 0


def test_runner_without_fence(service: DocumentService, doc: str) -> None:
    out = ToolRunner(service, fence=False).call(
        doc, "find", {"query": "world", "regex": None, "mode": None}
    )
    assert not out.text.startswith("<document_data")
