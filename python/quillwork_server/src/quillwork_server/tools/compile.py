"""Tool call → one ReadOp or one Transaction (spec/README.md, "Two layers: ops and tools").

Pure: no engine, no I/O. The one thing a tool call can't say by itself - the block after an
anchor, for ``edit`` ``merge`` - comes from a ``next_block`` callback the dispatcher supplies.

Value checks the tool schemas can't express come back as INVALID_OP, with ``path`` pointing into
the *tool call* (``/ops/1/target``), not into the compiled transaction.
"""

from __future__ import annotations

import json
import re
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any, Final

from quillwork.errors import QuillworkError

from ..core.errors import make_error
from ..core.fake import MUTATING_OPS, READ_OPS
from ..core.types import JSON

CELL: Final = re.compile(r"^t[1-9][0-9]*\.r[1-9][0-9]*\.c[1-9][0-9]*$")
REVISION: Final = re.compile(r"^r([0-9]+)$")
CHANGE_SET: Final = re.compile(r"^cs_[0-9a-z]+$")
MARK_FLAGS: Final = (
    "bold",
    "italic",
    "underline",
    "strike",
    "superscript",
    "subscript",
    "caps",
    "small_caps",
)
MARK_VALUES: Final = ("highlight", "color", "size", "font", "underline")

#: Signatures for INVALID_OP ``usage`` (what advanced_op ``help`` would print for the tool).
USAGE: Final[dict[str, str]] = {
    "read_document": "read_document {range: null|'a..b'|'a..'|'a,b', mode: null|accepted|markup}",
    "find": "find {query, regex: null|bool}",
    "edit": "edit {base_version, label, ops: [{op, anchor, find, occurrence, text, style, target, "
    "expect}]}",
    "format": "format {base_version, label, ops: [{anchor, find, occurrence, marks: '+bold -italic "
    "size=11 color=#1F3864 font=Arial highlight=yellow', style, align, list, level}]}",
    "comment": "comment {base_version, action: add|reply|resolve|reopen|delete, anchor, find, "
    "occurrence, text}",
    "table": "table {base_version, label, action: insert_table|insert_rows|delete_row, anchor, "
    "rows, header_row}",
    "review": "review {base_version, action: accept|reject, targets: [cs_7 | r12 | author:<name> "
    "| all]}",
    "advanced_op": "advanced_op {op, args: '<JSON object>', base_version} — op 'help' lists ops",
}


@dataclass(frozen=True, slots=True)
class ReadCall:
    """A tool call that compiled to one ReadOp."""

    tool: str
    request: JSON


@dataclass(frozen=True, slots=True)
class ApplyCall:
    """A tool call that compiled to one Transaction.

    ``op_map[i]`` is the index of the tool-call op that compiled op ``i`` came from (``None``
    for tools without an ``ops`` array), so an engine error's ``op_index`` can be reported
    against what the agent sent.
    """

    tool: str
    transaction: JSON
    op_map: list[int | None] = field(default_factory=list)
    review: bool = False


Compiled = ReadCall | ApplyCall
NextBlock = Callable[[str], str | None]


class _Invalid(Exception):
    def __init__(self, message: str, path: str, op_index: int | None = None) -> None:
        super().__init__(message)
        self.message = message
        self.path = path
        self.op_index = op_index


def compile_call(
    tool: str,
    args: JSON,
    *,
    version: int = 1,
    next_block: NextBlock | None = None,
) -> Compiled:
    """Compile one tool call. ``version`` is the document's current version (for errors).

    Raises QuillworkError (INVALID_OP) when the call can't become ops.
    """
    try:
        return _compile(tool, args, next_block)
    except _Invalid as bad:
        raise make_error(
            "INVALID_OP",
            bad.message,
            version=version,
            op_index=bad.op_index,
            op=tool,
            path=bad.path,
            usage=USAGE.get(tool, USAGE["advanced_op"]),
        ) from None


def _compile(tool: str, args: Any, next_block: NextBlock | None) -> Compiled:
    if not isinstance(args, dict):
        raise _Invalid("tool arguments must be a JSON object", "")
    if tool == "read_document":
        req: JSON = {"op": "read"}
        _put(req, "range", _opt_str(args, "range"))
        _put(req, "mode", _opt_str(args, "mode"))
        return ReadCall(tool, req)
    if tool == "find":
        query = _req_str(args, "query")
        req = {"op": "find", "query": query}
        _put(req, "regex", _opt_bool(args, "regex"))
        return ReadCall(tool, req)
    if tool == "edit":
        return _edit(args, next_block)
    if tool == "format":
        return _format(args)
    if tool == "comment":
        return _comment(args)
    if tool == "table":
        return _table(args)
    if tool == "review":
        return _review(args)
    if tool == "advanced_op":
        return _advanced(args)
    raise _Invalid(f"unknown tool {tool!r}", "")


# -- argument helpers ------------------------------------------------------------------------


def _put(target: JSON, key: str, value: Any) -> None:
    if value is not None:
        target[key] = value


def _opt_str(args: JSON, key: str, path: str = "") -> str | None:
    value = args.get(key)
    if value is None:
        return None
    if not isinstance(value, str):
        raise _Invalid(f"{key} must be a string or null", f"{path}/{key}")
    return value


def _req_str(args: JSON, key: str, path: str = "", op_index: int | None = None) -> str:
    value = args.get(key)
    if not isinstance(value, str) or not value:
        raise _Invalid(f"{key} is required", f"{path}/{key}", op_index)
    return value


def _opt_bool(args: JSON, key: str, path: str = "") -> bool | None:
    value = args.get(key)
    if value is None:
        return None
    if not isinstance(value, bool):
        raise _Invalid(f"{key} must be true, false or null", f"{path}/{key}")
    return value


def _opt_int(args: JSON, key: str, path: str = "", op_index: int | None = None) -> int | None:
    value = args.get(key)
    if value is None:
        return None
    if not isinstance(value, int) or isinstance(value, bool):
        raise _Invalid(f"{key} must be an integer or null", f"{path}/{key}", op_index)
    return value


def _base_version(args: JSON) -> int:
    value = args.get("base_version")
    if not isinstance(value, int) or isinstance(value, bool) or value < 1:
        raise _Invalid(
            "base_version must be the version of your last read or edit", "/base_version"
        )
    return value


def _tx(args: JSON, ops: list[JSON]) -> JSON:
    tx: JSON = {"base_version": _base_version(args), "ops": ops}
    _put(tx, "label", _opt_str(args, "label"))
    return tx


def _ops(args: JSON) -> list[Any]:
    ops = args.get("ops")
    if not isinstance(ops, list) or not ops:
        raise _Invalid("ops must be a non-empty array", "/ops")
    return ops


# -- edit ------------------------------------------------------------------------------------


def _edit(args: JSON, next_block: NextBlock | None) -> ApplyCall:
    out: list[JSON] = []
    op_map: list[int | None] = []
    base = _base_version(args)
    for i, raw in enumerate(_ops(args)):
        if not isinstance(raw, dict):
            raise _Invalid("each op must be an object", f"/ops/{i}", i)
        out.append(_edit_op(i, raw, next_block))
        op_map.append(i)
    tx = {"base_version": base, "ops": out}
    _put(tx, "label", _opt_str(args, "label"))
    return ApplyCall("edit", tx, op_map)


def _edit_op(i: int, op: JSON, next_block: NextBlock | None) -> JSON:
    p = f"/ops/{i}"
    kind = _req_str(op, "op", p, i)
    anchor = _req_str(op, "anchor", p, i)
    find = _opt_str(op, "find", p)
    occurrence = _opt_int(op, "occurrence", p, i)
    text = _opt_str(op, "text", p)
    style = _opt_str(op, "style", p)
    target = _opt_str(op, "target", p)
    expect = _opt_str(op, "expect", p)

    def need_text() -> str:
        if text is None:
            raise _Invalid(f"{kind} needs text", f"{p}/text", i)
        return text

    out: JSON
    if kind == "replace":
        if CELL.match(anchor) and find is None:
            out = {"op": "set_cell", "cell": anchor, "text": need_text()}
        else:
            out = {"op": "replace_text", "anchor": anchor}
            _put(out, "find", find)
            _put(out, "occurrence", occurrence)
            out["replace"] = need_text()
    elif kind in ("insert_after", "insert_before"):
        out = {"op": "insert_paragraph", kind.removeprefix("insert_"): anchor, "text": need_text()}
        _put(out, "style", style)
    elif kind == "delete":
        if find is None:
            out = {"op": "delete_block", "anchor": anchor}
        else:
            out = {"op": "replace_text", "anchor": anchor, "find": find}
            _put(out, "occurrence", occurrence)
            out["replace"] = ""
    elif kind in ("move_after", "move_before"):
        if target is None:
            raise _Invalid(f"{kind} needs target (the destination anchor)", f"{p}/target", i)
        out = {"op": "move_block", "anchor": anchor, kind.removeprefix("move_"): target}
    elif kind == "split":
        if find is None:
            raise _Invalid(
                "split needs find (the text the second paragraph starts with)", f"{p}/find", i
            )
        out = {"op": "split_paragraph", "anchor": anchor, "at_text": find}
        _put(out, "occurrence", occurrence)
    elif kind == "merge":
        second = next_block(anchor) if next_block is not None else None
        if second is None:
            raise _Invalid(f"{anchor} has no next block to merge with", f"{p}/anchor", i)
        out = {"op": "merge_paragraphs", "first": anchor, "second": second}
    else:
        raise _Invalid(f"unknown edit op {kind!r}", f"{p}/op", i)
    if expect is not None:
        out["expect"] = {"text": expect}
    return out


# -- format ----------------------------------------------------------------------------------


def parse_marks(spec: str) -> JSON:
    """``'+bold -italic size=11 color=#1f3864'`` → the ``marks`` object of set_text_format."""
    marks: JSON = {}
    for token in spec.replace(",", " ").split():
        if token[0] in "+-" and token[1:] in MARK_FLAGS:
            marks[token[1:]] = token[0] == "+"
        elif token[0] == "-" and token[1:] in MARK_VALUES:
            marks[token[1:]] = None  # clear direct formatting
        elif "=" in token and token.split("=", 1)[0] in MARK_VALUES:
            key, value = token.split("=", 1)
            if key == "size":
                try:
                    marks[key] = float(value) if "." in value else int(value)
                except ValueError:
                    raise ValueError(f"size must be a number of points, not {value!r}") from None
            elif key == "color":
                color = value.upper() if value.startswith("#") else "#" + value.upper()
                if not re.fullmatch(r"#[0-9A-F]{6}", color):
                    raise ValueError(f"color must be #RRGGBB, not {value!r}")
                marks[key] = color
            else:
                marks[key] = value
        else:
            raise ValueError(f"unknown mark {token!r}")
    if not marks:
        raise ValueError("marks is empty")
    return marks


def _format(args: JSON) -> ApplyCall:
    out: list[JSON] = []
    op_map: list[int | None] = []
    for i, raw in enumerate(_ops(args)):
        p = f"/ops/{i}"
        if not isinstance(raw, dict):
            raise _Invalid("each op must be an object", p, i)
        anchor = _req_str(raw, "anchor", p, i)
        find = _opt_str(raw, "find", p)
        occurrence = _opt_int(raw, "occurrence", p, i)
        marks = _opt_str(raw, "marks", p)
        style = _opt_str(raw, "style", p)
        align = _opt_str(raw, "align", p)
        lst = _opt_str(raw, "list", p)
        level = _opt_int(raw, "level", p, i)
        before = len(out)
        if marks is not None:
            try:
                parsed = parse_marks(marks)
            except ValueError as exc:
                raise _Invalid(str(exc), f"{p}/marks", i) from None
            op: JSON = {"op": "set_text_format", "anchor": anchor}
            _put(op, "find", find)
            _put(op, "occurrence", occurrence)
            op["marks"] = parsed
            out.append(op)
        elif find is not None:
            raise _Invalid("find only applies to marks", f"{p}/find", i)
        if style is not None or align is not None:
            op = {"op": "set_paragraph", "anchor": anchor}
            _put(op, "style", style)
            _put(op, "align", align)
            out.append(op)
        if lst is not None and lst.startswith("restart:"):
            try:
                start = int(lst.removeprefix("restart:"))
            except ValueError:
                raise _Invalid("restart needs a number: restart:1", f"{p}/list", i) from None
            out.append({"op": "restart_numbering", "anchor": anchor, "start": start})
            if level is not None:
                out.append({"op": "change_level", "anchor": anchor, "level": level})
        elif lst is not None:
            op = {"op": "set_list", "anchor": anchor, "list": lst}
            _put(op, "level", level)
            out.append(op)
        elif level is not None:
            out.append({"op": "change_level", "anchor": anchor, "level": level})
        if len(out) == before:
            raise _Invalid("nothing to change: set marks, style, align, list or level", p, i)
        op_map.extend([i] * (len(out) - before))
    return ApplyCall("format", _tx(args, out), op_map)


# -- comment, table, review ------------------------------------------------------------------


def _comment(args: JSON) -> ApplyCall:
    action = _req_str(args, "action")
    anchor = _req_str(args, "anchor")
    text = _opt_str(args, "text")

    def need_text() -> str:
        if not text:
            raise _Invalid(f"{action} needs text", "/text")
        return text

    op: JSON
    if action == "add":
        op = {"op": "add_comment", "anchor": anchor}
        _put(op, "find", _opt_str(args, "find"))
        _put(op, "occurrence", _opt_int(args, "occurrence"))
        op["text"] = need_text()
    elif action == "reply":
        op = {"op": "reply_comment", "comment": anchor, "text": need_text()}
    elif action in ("resolve", "reopen"):
        op = {"op": "resolve_comment", "comment": anchor, "done": action == "resolve"}
    elif action == "delete":
        op = {"op": "delete_comment", "comment": anchor}
    else:
        raise _Invalid(f"unknown comment action {action!r}", "/action")
    return ApplyCall("comment", _tx(args, [op]), [None])


def _rows(args: JSON) -> list[list[str]]:
    rows = args.get("rows")
    if not isinstance(rows, list) or not rows:
        raise _Invalid("rows must be a non-empty array of rows of cell texts", "/rows")
    for r, row in enumerate(rows):
        if not isinstance(row, list) or not all(isinstance(c, str) for c in row):
            raise _Invalid("each row must be an array of strings", f"/rows/{r}")
    return rows


def _table(args: JSON) -> ApplyCall:
    action = _req_str(args, "action")
    anchor = _req_str(args, "anchor")
    ops: list[JSON]
    if action == "insert_table":
        rows = _rows(args)
        cols = max(len(r) for r in rows)
        if cols < 1:
            raise _Invalid("a table needs at least one column", "/rows/0")
        op: JSON = {"op": "insert_table", "after": anchor, "rows": len(rows), "cols": cols}
        _put(op, "header_row", _opt_bool(args, "header_row"))
        op["data"] = rows
        ops = [op]
    elif action == "insert_rows":
        # Each insertion goes immediately after the same reference row. Apply them from
        # last to first so their final document order matches the caller's rows.
        ops = [{"op": "insert_row", "after": anchor, "cells": row} for row in reversed(_rows(args))]
    elif action == "delete_row":
        ops = [{"op": "delete_row", "row": anchor}]
    else:
        raise _Invalid(f"unknown table action {action!r}", "/action")
    return ApplyCall("table", _tx(args, ops), [None] * len(ops))


def _review(args: JSON) -> ApplyCall:
    action = _req_str(args, "action")
    if action not in ("accept", "reject"):
        raise _Invalid("action must be accept or reject", "/action")
    targets = args.get("targets")
    if not isinstance(targets, list) or not targets:
        raise _Invalid("targets must name at least one change", "/targets")
    ops: list[JSON] = []
    revisions: list[int] = []
    for t, target in enumerate(targets):
        if not isinstance(target, str):
            raise _Invalid("targets are strings", f"/targets/{t}")
        rev = REVISION.match(target)
        if target == "all":
            ops.append({"op": action, "all": True})
        elif CHANGE_SET.match(target):
            ops.append({"op": action, "change_set": target})
        elif rev:
            revisions.append(int(rev.group(1)))
        elif target.startswith("author:") and len(target) > len("author:"):
            ops.append({"op": action, "author": target.removeprefix("author:")})
        else:
            raise _Invalid(
                f"{target!r} is not cs_<id>, r<n>, author:<name> or all", f"/targets/{t}"
            )
    if revisions:
        ops.append({"op": action, "revision_ids": revisions})
    tx: JSON = {"base_version": _base_version(args), "ops": ops}
    return ApplyCall("review", tx, [None] * len(ops), review=True)


# -- advanced_op -----------------------------------------------------------------------------


def _advanced(args: JSON) -> Compiled:
    name = _req_str(args, "op")
    raw = args.get("args")
    if raw is None or raw == "":
        parsed: Any = {}
    elif isinstance(raw, dict):
        parsed = raw  # lenient: some clients send the object itself
    elif isinstance(raw, str):
        try:
            parsed = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise _Invalid(f"args is not valid JSON: {exc.msg}", "/args") from None
    else:
        raise _Invalid("args must be a JSON object encoded as a string", "/args")
    if not isinstance(parsed, dict):
        raise _Invalid("args must encode a JSON object", "/args")
    if "op" in parsed and parsed["op"] != name:
        raise _Invalid("args.op disagrees with op", "/args")
    op: JSON = {"op": name, **{k: v for k, v in parsed.items() if k != "op"}}
    if name in READ_OPS:
        return ReadCall("advanced_op", op)
    if name not in MUTATING_OPS:
        raise _Invalid(f"unknown op {name!r}; op 'help' lists them", "/op")
    base = args.get("base_version")
    if not isinstance(base, int) or isinstance(base, bool) or base < 1:
        raise _Invalid(f"{name} changes the document: give base_version", "/base_version")
    tx: JSON = {"base_version": base, "ops": [op]}
    review = name in ("accept", "reject")
    return ApplyCall("advanced_op", tx, [None], review=review)


def translate_error(exc: QuillworkError, call: Compiled) -> QuillworkError:
    """Point an engine error's ``op_index`` at the tool call's op (or drop it).

    The rest of the error is the engine's, unchanged.
    """
    if not isinstance(call, ApplyCall) or exc.op_index is None:
        return exc
    error = dict(exc.error)
    idx = error.get("op_index")
    mapped = call.op_map[idx] if isinstance(idx, int) and 0 <= idx < len(call.op_map) else None
    error["op_index"] = mapped
    if "path" in error and isinstance(idx, int) and isinstance(error["path"], str):
        error["path"] = re.sub(
            r"^/ops/\d+", f"/ops/{mapped}" if mapped is not None else "", error["path"]
        )
    from quillwork.errors import error_from_dict

    return error_from_dict(error)
