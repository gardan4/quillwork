"""A tiny engine on plain text, for tests and for hosts wiring the service up before the real
engine's op seam lands.

The "file" is UTF-8 text, one paragraph per line; a line starting with ``# `` is a level-1
heading. It implements a small subset of the spec with the real shapes:

- reads: ``read`` (header, outline, windows, ranges, selections, accepted/markup), ``find``,
  ``outline``, ``get_block``, ``list_changes``, ``list_comments``, ``list_images`` (none),
  ``get_image`` (NOT_FOUND), ``list_fields`` (placeholders only), ``lint`` (leftover
  placeholders), ``help``, with the data shapes of the real engine (spec/projection.md §8);
- writes: ``replace_text``, ``insert_paragraph``, ``delete_block``, ``add_comment``,
  ``reply_comment``, ``resolve_comment``, ``delete_comment``, ``accept``, ``reject``,
  ``set_track_changes``;
- errors: INVALID_OP, LIMIT, BAD_VERSION, NOT_FOUND, STALE_ANCHOR (with ``expect``), AMBIGUOUS,
  UNSUPPORTED (every other catalogue op), BAD_PACKAGE (not UTF-8).

Simplifications: a block holds at most one open change; a second change set on the same block
takes it over (rejecting it restores the text from before the first). ``save()`` writes the
accepted text, so pending changes live only in the sidecar, as the real engine's change-set
metadata does.
"""

from __future__ import annotations

import copy
import hashlib
import re
from collections.abc import Callable
from datetime import UTC, datetime
from typing import Any, Final

from quillwork.errors import QuillworkError

from .engine import EngineDocument
from .errors import SPEC_VERSION, make_error
from .types import JSON, Asset

B32: Final = "0123456789abcdefghjkmnpqrstvwxyz"

#: Every mutating op in spec/schema/ops.schema.json.
MUTATING_OPS: Final = frozenset(
    {
        "replace_text",
        "insert_paragraph",
        "delete_block",
        "move_block",
        "split_paragraph",
        "merge_paragraphs",
        "set_text_format",
        "set_paragraph",
        "set_list",
        "restart_numbering",
        "change_level",
        "add_comment",
        "reply_comment",
        "resolve_comment",
        "delete_comment",
        "insert_table",
        "insert_row",
        "delete_row",
        "insert_column",
        "delete_column",
        "set_cell",
        "merge_cells",
        "split_cell",
        "insert_image",
        "replace_image",
        "delete_image",
        "resize_image",
        "set_alt_text",
        "anchor_image",
        "insert_break",
        "insert_footnote",
        "set_hyperlink",
        "insert_field",
        "fill",
        "set_properties",
        "set_track_changes",
        "accept",
        "reject",
        "compare",
    }
)
#: Every read op in spec/schema/ops.schema.json.
READ_OPS: Final = frozenset(
    {
        "outline",
        "read",
        "find",
        "get_block",
        "list_changes",
        "list_comments",
        "list_images",
        "get_image",
        "lint",
        "list_fields",
        "help",
    }
)

_FAKE_WRITES: Final = frozenset(
    {
        "replace_text",
        "insert_paragraph",
        "delete_block",
        "add_comment",
        "reply_comment",
        "resolve_comment",
        "delete_comment",
        "accept",
        "reject",
        "set_track_changes",
    }
)
_FAKE_READS: Final = READ_OPS
#: The keys each read op takes besides ``op`` (and ``mode``, which every read ignores or uses).
_READ_KEYS: Final[dict[str, frozenset[str]]] = {
    "outline": frozenset(),
    "read": frozenset({"range", "budget"}),
    "find": frozenset({"query", "regex"}),
    "get_block": frozenset({"anchor"}),
    "list_changes": frozenset({"change_set", "author", "status"}),
    "list_comments": frozenset({"open_only"}),
    "list_images": frozenset(),
    "get_image": frozenset({"image", "asset"}),
    "lint": frozenset(),
    "list_fields": frozenset(),
    "help": frozenset({"name"}),
}
_PLACEHOLDER: Final = re.compile(
    r"\{[^\W\d][\w .\-]{0,40}\}|\[[A-Z][A-Z0-9_ .\-]{1,40}\]|[.…_]{5,}"
)
_CHANGE_STATUS: Final = ("open", "accepted", "rejected", "partial")
_TX_KEYS: Final = frozenset(
    {"spec_version", "base_version", "ops", "mode", "label", "author", "turn", "projection_mode"}
)
_USAGE: Final[dict[str, str]] = {
    "transaction": "apply {base_version, ops[1..], mode?, label?, author?, turn?}",
    "replace_text": "replace_text {anchor, find?, occurrence?, replace, expect?}",
    "insert_paragraph": "insert_paragraph {after|before, text, style?}",
    "delete_block": "delete_block {anchor, expect?}",
    "add_comment": "add_comment {anchor, find?, occurrence?, text, author?}",
    "reply_comment": "reply_comment {comment, text, author?}",
    "resolve_comment": "resolve_comment {comment, done}",
    "delete_comment": "delete_comment {comment}",
    "accept": "accept {change_set|revision_ids|author|all:true}",
    "reject": "reject {change_set|revision_ids|author|all:true}",
    "set_track_changes": "set_track_changes {on}",
    "read": "read {range?, mode?, budget?}",
    "find": "find {query, regex?, mode?}",
    "get_block": "get_block {anchor}",
    "outline": "outline {}",
    "list_changes": "list_changes {change_set?, author?, status?}",
    "list_comments": "list_comments {open_only?}",
    "list_images": "list_images {}",
    "get_image": "get_image {image|asset}",
    "lint": "lint {}",
    "list_fields": "list_fields {}",
    "help": "help {name?}",
}
DEFAULT_BUDGET: Final = 8000
MIN_BUDGET: Final = 500


def serial(n: int, width: int = 3) -> str:
    """Crockford base32, lowercase, zero-padded to ``width`` (spec/anchors.md)."""
    digits = ""
    while n:
        n, r = divmod(n, 32)
        digits = B32[r] + digits
    return digits.rjust(width, "0")


def _now() -> str:
    return datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def _tokens(line: str) -> int:
    return max(1, len(line) // 4)


class FakeEngine:
    name = "fake"

    def __init__(
        self,
        *,
        author: str = "AI",
        max_ops: int = 200,
        clock: Callable[[], str] = _now,
    ) -> None:
        self.author = author
        self.max_ops = max_ops
        self.clock = clock
        #: Count of documents opened (tests use it to see the service's engine cache work).
        self.opened = 0

    def open(self, data: bytes, *, version: int = 1, sidecar: JSON | None = None) -> EngineDocument:
        self.opened += 1
        return FakeDocument(self, bytes(data), version, sidecar)

    def asset(self, data: bytes, asset_id: str) -> Asset | None:
        return None


class FakeDocument:
    def __init__(self, engine: FakeEngine, data: bytes, version: int, sidecar: JSON | None) -> None:
        self._engine = engine
        self._original = data
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError:
            raise make_error(
                "BAD_PACKAGE", "The file is not a document this engine can read.", version=1
            ) from None
        fingerprint = hashlib.sha256(data).hexdigest()
        if sidecar and sidecar.get("fingerprint") == fingerprint:
            self._s: JSON = copy.deepcopy(sidecar)
        else:
            self._s = self._fresh(text, sidecar)
            self._s["fingerprint"] = fingerprint
        self._s["version"] = version
        self._dirty = False

    @staticmethod
    def _fresh(text: str, previous: JSON | None) -> JSON:
        # Serials are never reused: continue the old counter after an outside edit.
        start = int(previous["next_serial"]) if previous else 1024
        blocks = []
        lines = text.split("\n") if text else []
        if lines and lines[-1] == "":
            lines.pop()
        for i, line in enumerate(lines):
            heading = line.startswith("# ")
            blocks.append(
                {
                    "id": "b" + serial(start + i),
                    "text": line[2:] if heading else line,
                    "style": "Heading1" if heading else "Normal",
                    "changed": 0,
                    "old": None,
                    "ins": None,
                    "del": None,
                    "cs": None,
                    "rev": None,
                }
            )
        return {
            "engine": "fake",
            "version": 1,
            "next_serial": start + len(blocks),
            "next_cs": int(previous["next_cs"]) if previous else 1,
            "next_rev": int(previous["next_rev"]) if previous else 1,
            "next_comment": int(previous["next_comment"]) if previous else 1,
            "track": True,
            "blocks": blocks,
            "change_sets": {},
            "comments": [],
        }

    # -- EngineDocument ----------------------------------------------------------------------

    @property
    def version(self) -> int:
        return int(self._s["version"])

    def save(self) -> bytes:
        if not self._dirty:
            return self._original
        lines = [("# " if b["style"] == "Heading1" else "") + b["text"] for b in self._visible()]
        return ("\n".join(lines) + "\n").encode("utf-8") if lines else b""

    def sidecar(self) -> JSON | None:
        state = copy.deepcopy(self._s)
        state["fingerprint"] = hashlib.sha256(self.save()).hexdigest()
        return state

    def view_model(self) -> JSON:
        s = self._s
        return {
            "spec_version": SPEC_VERSION,
            "version": self.version,
            "meta": {
                "title": self._title(),
                "lang": "en",
                "trackChanges": bool(s["track"]),
                "labels": [],
                "pagesEstimate": 1,
                "readOnly": None,
                "flags": [],
            },
            "sections": [
                {
                    "id": "s1",
                    "page": {
                        "w": 11906,
                        "h": 16838,
                        "orient": "portrait",
                        "margins": {"top": 1440, "right": 1440, "bottom": 1440, "left": 1440},
                    },
                    "headers": {"default": None, "first": None, "even": None},
                    "footers": {"default": None, "first": None, "even": None},
                    "titlePage": False,
                }
            ],
            "blocks": [self._vm_block(b) for b in s["blocks"]],
            "stories": {},
            "comments": [],
            "changeSets": [self._cs_out(cs) for cs in s["change_sets"].values()],
            "assets": {},
        }

    def read_op(self, request: JSON) -> JSON:
        op = request.get("op")
        if not isinstance(op, str):
            raise self._invalid("read op needs an op name", "/op", "read")
        if op not in READ_OPS:
            raise self._invalid(f"{op} is not a read op", "/op", "read")
        extra = sorted(set(request) - {"op", "mode", "spec_version"} - _READ_KEYS[op])
        if extra:
            raise self._invalid(f"{op} has no {extra[0]}", f"/{extra[0]}", op)
        mode = request.get("mode") or "accepted"
        if op == "read":
            return self._read(request.get("range"), mode, request.get("budget"))
        if op == "outline":
            return self._read_result(self._outline_line(), None)
        if op == "find":
            return self._find(request, mode)
        if op == "get_block":
            anchor = request.get("anchor")
            if not isinstance(anchor, str) or not anchor:
                raise self._invalid("get_block needs an anchor", "/anchor", op)
            block = self._block(anchor, op=op, op_index=None)
            return self._read_result(None, {"story": "b", "block": self._vm_block(block)})
        if op == "list_changes":
            return self._read_result(None, self._list_changes(request))
        if op == "list_comments":
            open_only = request.get("open_only")
            if open_only is not None and not isinstance(open_only, bool):
                raise self._invalid("open_only is true or false", "/open_only", op)
            comments = [c for c in self._s["comments"] if not (open_only and c["done"])]
            return self._read_result(None, {"comments": copy.deepcopy(comments)})
        if op == "list_images":
            return self._read_result(None, {"images": []})
        if op == "get_image":
            return self._get_image(request)
        if op == "list_fields":
            return self._read_result(None, {"controls": [], "placeholders": self._placeholders()})
        if op == "lint":
            return self._read_result(self._lint(), None)
        return self._help(request.get("name"))

    def _list_changes(self, request: JSON) -> JSON:
        args: dict[str, str | None] = {}
        for key in ("change_set", "author", "status"):
            value = request.get(key)
            if value is not None and (not isinstance(value, str) or not value):
                raise self._invalid(f"{key} is a non-empty string", f"/{key}", "list_changes")
            args[key] = value
        cs_id, author, status = args["change_set"], args["author"], args["status"]
        sets = self._s["change_sets"]
        if cs_id is not None and cs_id not in sets:
            raise make_error(
                "NOT_FOUND",
                f"no change set {cs_id} in this session's log",
                version=self.version,
                op="list_changes",
                what="change_set",
            )
        if status is not None and status not in _CHANGE_STATUS:
            raise self._invalid(
                "status is open, accepted, rejected or partial", "/status", "list_changes"
            )

        def keep(cs: JSON) -> bool:
            return (
                (cs_id is None or cs["id"] == cs_id)
                and (author is None or cs["author"] == author)
                and (status is None or cs["status"] == status)
            )

        revisions: list[JSON] = []
        for b in self._s["blocks"]:
            if b["rev"] is None or not keep(sets[b["cs"]]):
                continue
            base = {
                "id": b["rev"],
                "author": b.get("author") or self._engine.author,
                "agent": True,
                "date": b.get("date") or self._engine.clock(),
                "change_set": b["cs"],
                "story": "b",
                "anchors": [b["id"]],
            }
            if b["old"] is not None and not b["del"] and not b["ins"]:
                revisions.append(base | {"type": "del", "text": str(b["old"])[:200]})
            kind = "del" if b["del"] else "ins"
            revisions.append(base | {"type": kind, "text": str(b["text"])[:200]})
        change_sets = [self._cs_out(cs) for cs in sets.values() if keep(cs)]
        return {"revisions": revisions, "change_sets": change_sets}

    def _get_image(self, request: JSON) -> JSON:
        given = [k for k in ("image", "asset") if request.get(k) is not None]
        if len(given) == 2:
            raise self._invalid("give image or asset, not both", "/asset", "get_image")
        if not given:
            raise self._invalid("image (img3) or asset (a_…) is required", "/image", "get_image")
        key = given[0]
        raise make_error(
            "NOT_FOUND",
            f"{request[key]} is not in the document.",
            version=self.version,
            op="get_image",
            anchor=request[key] if key == "image" else None,
            what="anchor" if key == "image" else "asset",
        )

    def _placeholders(self) -> list[JSON]:
        return [
            {"anchor": b["id"], "text": m.group(0)}
            for b in self._visible()
            for m in _PLACEHOLDER.finditer(str(b["text"]))
        ]

    def _lint(self) -> str:
        lines = []
        for p in self._placeholders():
            blank = p["text"][0] in ".…_"
            sev, what = (
                ("info", "blank to fill in")
                if blank
                else ("warning", "placeholder left in the text")
            )
            lines.append(
                f'§ lint: {sev} leftover_placeholder {p["anchor"]} — {what}: "{p["text"]}"'
            )
        return "\n".join(lines) or "§ lint: none"

    def _help(self, name: Any) -> JSON:
        if name is None:
            reads = [_USAGE.get(n, n) for n in sorted(READ_OPS)]
            writes = [_USAGE.get(n, n) for n in sorted(_FAKE_WRITES)]
            return self._read_result("\n".join(["§ read ops", *reads, "§ edit ops", *writes]), None)
        if not isinstance(name, str) or not name:
            raise self._invalid(
                "name is an op name (replace_text, list_changes, …)", "/name", "help"
            )
        if name not in READ_OPS and name not in MUTATING_OPS:
            raise self._invalid(
                f"unknown op {name!r}; help with no name lists them all", "/name", "help"
            )
        line = _USAGE.get(name, name)
        if name not in READ_OPS and name not in _FAKE_WRITES:
            line += " (not implemented)"
        return self._read_result(line, None)

    def apply(self, transaction: JSON) -> JSON:
        tx = transaction
        ops = tx.get("ops")
        if isinstance(ops, list) and len(ops) > self._engine.max_ops:
            raise make_error(
                "LIMIT",
                f"A transaction may hold at most {self._engine.max_ops} ops.",
                version=self.version,
                limit={
                    "name": "ops_per_transaction",
                    "max": self._engine.max_ops,
                    "actual": len(ops),
                },
            )
        extra = sorted(set(tx) - _TX_KEYS)
        if extra:
            raise self._invalid(
                f"unknown transaction field {extra[0]}", f"/{extra[0]}", "transaction"
            )
        base = tx.get("base_version")
        if not isinstance(base, int) or isinstance(base, bool) or base < 1:
            raise self._invalid(
                "base_version must be a version number", "/base_version", "transaction"
            )
        if not isinstance(ops, list) or not ops:
            raise self._invalid("a transaction needs at least one op", "/ops", "transaction")
        for i, op in enumerate(ops):
            name = op.get("op") if isinstance(op, dict) else None
            if name not in MUTATING_OPS:
                raise self._invalid(
                    f"{name!r} is not an op that changes the document",
                    f"/ops/{i}/op",
                    "transaction",
                    op_index=i,
                )
        if base > self.version:
            raise make_error(
                "BAD_VERSION",
                f"base_version {base} is newer than the document (v{self.version}).",
                version=self.version,
            )
        for i, op in enumerate(ops):
            if op["op"] not in _FAKE_WRITES:
                raise self._unsupported(op["op"], i)

        before = copy.deepcopy(self._s)
        changed_before = {b["id"]: b["changed"] for b in self._s["blocks"]}
        try:
            result = _Tx(self, tx, changed_before).run()
        except BaseException:
            self._s = before
            raise
        self._dirty = True
        return result

    # -- helpers used by reads and _Tx --------------------------------------------------------

    def _title(self) -> str | None:
        for b in self._s["blocks"]:
            if b["style"] == "Heading1" and not b["del"]:
                return str(b["text"])
        return None

    def _visible(self) -> list[JSON]:
        return [b for b in self._s["blocks"] if not b["del"]]

    def _blocks(self, mode: str) -> list[JSON]:
        return list(self._s["blocks"]) if mode == "markup" else self._visible()

    def _index(self, anchor: str, mode: str = "markup") -> int | None:
        for i, b in enumerate(self._blocks(mode)):
            if b["id"] == anchor:
                return i
        return None

    def _block(self, anchor: str, *, op: str | None, op_index: int | None) -> JSON:
        for b in self._s["blocks"]:
            if b["id"] == anchor and not b["del"]:
                return dict(b)
        raise make_error(
            "NOT_FOUND",
            f"{anchor} is not in the document.",
            version=self.version,
            op_index=op_index,
            op=op,
            anchor=anchor,
            what="anchor",
        )

    def line(self, block: JSON, mode: str = "accepted") -> str:
        tag = block["id"] + (" H1" if block["style"] == "Heading1" else "")
        text = str(block["text"])
        if mode == "markup":
            if block["del"]:
                text = "{--" + text + "--}"
            elif block["ins"]:
                text = "{++" + text + "++}"
            elif block["old"] is not None:
                text = "{--" + str(block["old"]) + "--}{++" + text + "++}"
            if block["rev"] is not None:
                text += f"⟨r{block['rev']} {block.get('author') or self._engine.author}⟩"
        return f"[{tag}] {text}"

    def _outline_line(self) -> str:
        heads = [f"[{b['id']}] {b['text']}" for b in self._visible() if b["style"] == "Heading1"]
        return "§ outline: " + (" · ".join(heads) if heads else "(no headings)")

    def _read_result(self, text: str | None, data: JSON | None, nxt: str | None = None) -> JSON:
        return {
            "spec_version": SPEC_VERSION,
            "ok": True,
            "version": self.version,
            "text": text,
            "data": data,
            "next": nxt,
        }

    def _read(self, selection: Any, mode: str, budget: Any) -> JSON:
        limit = budget if isinstance(budget, int) and budget >= MIN_BUDGET else DEFAULT_BUDGET
        blocks = self._blocks(mode)
        if not selection:
            lines, nxt = self._window(blocks, 0, limit, mode)
            last = len(lines)
            first_id = blocks[0]["id"] if blocks else "-"
            last_id = blocks[last - 1]["id"] if last else "-"
            s = self._s
            open_cs = [cs for cs in s["change_sets"].values() if cs["status"] == "open"]
            revisions = sum(len(cs["revisions"]) for cs in open_cs)
            open_comments = sum(1 for c in s["comments"] if not c["done"])
            header = (
                f'§ doc "{self._title() or ""}" · v{self.version} · 1 pages (est.) · en · '
                f"tracked changes: {'on' if s['track'] else 'off'} · "
                f"{open_comments} open comments · {revisions} open revisions · view: {mode}"
            )
            window = f"§ window {first_id}..{last_id} (blocks 1–{last} of {len(blocks)})"
            text = "\n".join([header, self._outline_line(), window, "", *lines])
            return self._read_result(text, None, nxt)
        out: list[str] = []
        nxt = None
        items = str(selection).split(",")
        for item in items:
            if ".." in item:
                start, _, end = item.partition("..")
                i = self._require(start, mode)
                if end:
                    j = self._require(end, mode)
                    if j < i:
                        raise self._invalid(f"{start} comes after {end}", "/range", "read")
                    out.extend(self.line(b, mode) for b in blocks[i : j + 1])
                else:
                    lines, nxt = self._window(blocks, i, limit, mode)
                    out.extend(lines)
            else:
                out.append(self.line(blocks[self._require(item, mode)], mode))
        return self._read_result("\n".join(out), None, nxt)

    def _window(
        self, blocks: list[JSON], start: int, budget: int, mode: str
    ) -> tuple[list[str], str | None]:
        lines: list[str] = []
        used = 0
        for k in range(start, len(blocks)):
            line = self.line(blocks[k], mode)
            cost = _tokens(line)
            if lines and used + cost > budget:
                return lines, f"{blocks[k]['id']}.."
            lines.append(line)
            used += cost
        return lines, None

    def _require(self, anchor: str, mode: str) -> int:
        i = self._index(anchor, mode)
        if i is None:
            raise make_error(
                "NOT_FOUND",
                f"{anchor} is not in the document.",
                version=self.version,
                anchor=anchor,
                what="anchor",
            )
        return i

    def _find(self, request: JSON, mode: str) -> JSON:
        query = request.get("query")
        if not isinstance(query, str) or not query:
            raise self._invalid("find needs a query", "/query", "find")
        if request.get("regex"):
            try:
                pattern = re.compile(query, re.IGNORECASE)
            except re.error as exc:
                raise self._invalid(f"bad regex: {exc}", "/query", "find") from None
            hit: Callable[[str], bool] = lambda t: pattern.search(t) is not None  # noqa: E731
        else:
            needle = query.casefold()
            hit = lambda t: needle in t.casefold()  # noqa: E731
        lines = [self.line(b, mode) for b in self._blocks(mode) if hit(str(b["text"]))]
        return self._read_result("\n".join(lines), None)

    def _vm_block(self, b: JSON) -> JSON:
        rev = None
        if b["rev"] is not None:
            rev = {
                "id": b["rev"],
                "author": b.get("author") or self._engine.author,
                "date": b.get("date") or self._engine.clock(),
                "cs": b["cs"],
            }
        if b["del"] and rev:
            inlines = [{"t": b["text"], "rev": {**rev, "type": "del"}}]
        elif b["ins"] and rev:
            inlines = [{"t": b["text"], "rev": {**rev, "type": "ins"}}]
        elif b["old"] is not None and rev:
            inlines = [
                {"t": b["old"], "rev": {**rev, "type": "del"}},
                {"t": b["text"], "rev": {**rev, "type": "ins"}},
            ]
        else:
            inlines = [{"t": b["text"]}] if b["text"] else []
        return {
            "id": b["id"],
            "kind": "p",
            "section": "s1",
            "style": b["style"],
            "heading": 1 if b["style"] == "Heading1" else None,
            "inlines": inlines,
        }

    @staticmethod
    def _cs_out(cs: JSON) -> JSON:
        return {
            k: copy.deepcopy(cs[k])
            for k in (
                "id",
                "label",
                "author",
                "turn",
                "version",
                "revisions",
                "untracked",
                "status",
            )
        }

    def _invalid(
        self,
        message: str,
        path: str,
        usage: str,
        *,
        op_index: int | None = None,
        op: str | None = None,
    ) -> QuillworkError:
        return make_error(
            "INVALID_OP",
            message,
            version=self.version,
            op_index=op_index,
            op=op,
            path=path,
            usage=_USAGE.get(usage, usage),
        )

    def _unsupported(self, op: str, op_index: int | None) -> QuillworkError:
        return make_error(
            "UNSUPPORTED",
            f"The fake engine does not implement {op}.",
            version=self.version,
            op_index=op_index,
            op=op,
            reason="preserve_only",
            retryable=False,
        )


class _Tx:
    """One transaction against a FakeDocument's state (rolled back by the caller on error)."""

    def __init__(self, doc: FakeDocument, tx: JSON, changed_before: dict[str, int]) -> None:
        self.doc = doc
        self.s = doc._s
        self.tx = tx
        self.base = int(tx["base_version"])
        self.new_version = doc.version + 1
        self.author = str(tx.get("author") or doc._engine.author)
        self.date = doc._engine.clock()
        self.changed_before = changed_before
        self.touched: list[str] = []
        self.removed: list[str] = []
        self.cs: JSON | None = None
        self.review_cs: dict[str, JSON] = {}

    # -- plumbing ----------------------------------------------------------------------------

    def run(self) -> JSON:
        for i, op in enumerate(self.tx["ops"]):
            getattr(self, "op_" + op["op"])(i, op)
        self.s["version"] = self.new_version
        mode = self.tx.get("projection_mode") or "accepted"
        blocks = self.doc._blocks(mode)
        changed = [self.doc.line(b, mode) for b in blocks if b["id"] in self.touched]
        removed = [a for a in self.removed if self.doc._index(a, mode) is None]
        all_ids = {b["id"] for b in self.s["blocks"]}
        upserts = []
        for k, b in enumerate(self.s["blocks"]):
            if b["id"] in self.touched:
                after = self.s["blocks"][k - 1]["id"] if k else None
                upserts.append({"story": "b", "after": after, "block": self.doc._vm_block(b)})
        change_sets = list(self.review_cs.values())
        if self.cs is not None:
            change_sets.append(self.cs)
        patch: JSON = {
            "spec_version": SPEC_VERSION,
            "base_version": self.new_version - 1,
            "version": self.new_version,
            "removes": [a for a in dict.fromkeys(self.removed) if a not in all_ids],
            "moves": [],
            "upserts": upserts,
        }
        if change_sets:
            patch["changeSets"] = [FakeDocument._cs_out(cs) for cs in change_sets]
        return {
            "spec_version": SPEC_VERSION,
            "ok": True,
            "base_version": self.new_version - 1,
            "version": self.new_version,
            "change_set": self.cs["id"] if self.cs is not None else None,
            "changed": changed,
            "removed": list(dict.fromkeys(removed)),
            "outline": None,
            "patch": patch,
            "lint": [],
            "warnings": [],
        }

    def tracked(self, op: JSON) -> bool:
        return (op.get("mode") or self.tx.get("mode") or "track") == "track"

    def change_set(self) -> JSON:
        if self.cs is None:
            cs_id = "cs_" + _base36(int(self.s["next_cs"]))
            self.s["next_cs"] += 1
            self.cs = {
                "id": cs_id,
                "label": self.tx.get("label"),
                "author": self.author,
                "turn": self.tx.get("turn"),
                "version": self.new_version,
                "revisions": [],
                "untracked": [],
                "status": "open",
            }
            self.s["change_sets"][cs_id] = self.cs
        return self.cs

    def revise(self, block: JSON) -> None:
        """Attach ``block`` to this transaction's change set as one revision."""
        cs = self.change_set()
        rev = int(self.s["next_rev"])
        self.s["next_rev"] += 1
        block.update(rev=rev, cs=cs["id"], author=self.author, date=self.date)
        cs["revisions"].append(rev)

    def touch(self, block: JSON) -> None:
        block["changed"] = self.new_version
        if block["id"] not in self.touched:
            self.touched.append(block["id"])

    def target(self, i: int, op: JSON, key: str = "anchor") -> JSON:
        anchor = op.get(key)
        if not isinstance(anchor, str):
            raise self.doc._invalid(
                f"{op['op']} needs {key}", f"/ops/{i}/{key}", op["op"], op_index=i, op=op["op"]
            )
        for b in self.s["blocks"]:
            if b["id"] == anchor and not b["del"]:
                return b  # type: ignore[no-any-return]
        raise make_error(
            "NOT_FOUND",
            f"{anchor} is not in the document.",
            version=self.doc.version,
            op_index=i,
            op=op["op"],
            anchor=anchor,
            what="anchor",
        )

    def guard(self, i: int, op: JSON, block: JSON) -> None:
        """STALE_ANCHOR unless the block is unchanged since base_version or ``expect`` matches."""
        own = op.get("base_version")
        base: int = own if isinstance(own, int) else self.base
        if self.changed_before.get(block["id"], 0) <= base:
            return
        expect = op.get("expect")
        if isinstance(expect, dict):
            text = str(block["text"])
            if expect.get("text") == text:
                return
            digest = "sha256:" + hashlib.sha256(text.encode()).hexdigest()
            h = expect.get("hash")
            if isinstance(h, str) and len(h) >= 19 and digest.startswith(h):
                return
        raise make_error(
            "STALE_ANCHOR",
            f"{block['id']} changed after base_version {base}; current text below.",
            version=self.doc.version,
            op_index=i,
            op=op["op"],
            anchor=block["id"],
            current=[self.doc.line(block)],
        )

    def locate(self, i: int, op: JSON, block: JSON, find: str) -> int:
        text = str(block["text"])
        starts = [m.start() for m in re.finditer(re.escape(find), text)]
        if not starts:
            raise make_error(
                "NOT_FOUND",
                f"{find!r} is not in {block['id']}.",
                version=self.doc.version,
                op_index=i,
                op=op["op"],
                anchor=block["id"],
                what="text",
                current=[self.doc.line(block)],
            )
        occurrence = op.get("occurrence")
        if occurrence is None:
            if len(starts) > 1:
                raise make_error(
                    "AMBIGUOUS",
                    f"{find!r} occurs {len(starts)} times in {block['id']}; pass occurrence.",
                    version=self.doc.version,
                    op_index=i,
                    op=op["op"],
                    anchor=block["id"],
                    candidates=[
                        {
                            "anchor": block["id"],
                            "occurrence": k + 1,
                            "context": _context(text, s, len(find)),
                        }
                        for k, s in enumerate(starts[:20])
                    ],
                )
            return starts[0]
        if not isinstance(occurrence, int) or not 1 <= occurrence <= len(starts):
            raise self.doc._invalid(
                f"occurrence must be 1..{len(starts)}",
                f"/ops/{i}/occurrence",
                op["op"],
                op_index=i,
                op=op["op"],
            )
        return starts[occurrence - 1]

    def new_block(self, text: str, style: str) -> JSON:
        n = int(self.s["next_serial"])
        self.s["next_serial"] += 1
        return {
            "id": "b" + serial(n),
            "text": text,
            "style": style,
            "changed": 0,
            "old": None,
            "ins": None,
            "del": None,
            "cs": None,
            "rev": None,
        }

    def require_comment(self, i: int, op: JSON) -> JSON:
        cid = op.get("comment")
        for c in self.s["comments"]:
            if c["id"] == cid:
                return c  # type: ignore[no-any-return]
        raise make_error(
            "NOT_FOUND",
            f"There is no comment {cid}.",
            version=self.doc.version,
            op_index=i,
            op=op["op"],
            what="comment",
        )

    def text_arg(self, i: int, op: JSON, key: str) -> str:
        value = op.get(key)
        if not isinstance(value, str):
            raise self.doc._invalid(
                f"{op['op']} needs {key}", f"/ops/{i}/{key}", op["op"], op_index=i, op=op["op"]
            )
        return value

    # -- ops ---------------------------------------------------------------------------------

    def op_replace_text(self, i: int, op: JSON) -> None:
        block = self.target(i, op)
        replace = self.text_arg(i, op, "replace")
        self.guard(i, op, block)
        find = op.get("find")
        text = str(block["text"])
        if find:
            at = self.locate(i, op, block, find)
            new = text[:at] + replace + text[at + len(find) :]
        else:
            new = replace
        if self.tracked(op):
            if block["ins"] is None and block["old"] is None:
                block["old"] = text
            self.revise(block)
        block["text"] = new
        self.touch(block)

    def op_insert_paragraph(self, i: int, op: JSON) -> None:
        text = self.text_arg(i, op, "text")
        ref_key = "after" if op.get("after") else "before"
        ref = self.target(i, op, ref_key)
        style = op.get("style")
        style = "Heading1" if style in ("H1", "Heading1") else "Normal"
        blocks = self.s["blocks"]
        at = blocks.index(ref) + (1 if ref_key == "after" else 0)
        for k, line in enumerate(text.split("\n")):
            block = self.new_block(line, style)
            if self.tracked(op):
                block["ins"] = self.change_set()["id"]
                self.revise(block)
            blocks.insert(at + k, block)
            self.touch(block)

    def op_delete_block(self, i: int, op: JSON) -> None:
        block = self.target(i, op)
        self.guard(i, op, block)
        if self.tracked(op) and block["ins"] is None:
            block["del"] = self.change_set()["id"]
            self.revise(block)
            self.touch(block)
        else:
            self.s["blocks"].remove(block)
        self.removed.append(block["id"])

    def op_add_comment(self, i: int, op: JSON) -> None:
        block = self.target(i, op)
        text = self.text_arg(i, op, "text")
        on = op.get("find")
        if on:
            self.locate(i, op, block, on)
        cid = f"c{self.s['next_comment']}"
        self.s["next_comment"] += 1
        self.s["comments"].append(
            {
                "id": cid,
                "author": op.get("author") or self.author,
                "done": False,
                "on": on or block["text"],
                "anchor": block["id"],
                "text": text,
                "replies": [],
            }
        )
        self.touch(block)

    def op_reply_comment(self, i: int, op: JSON) -> None:
        c = self.require_comment(i, op)
        text = self.text_arg(i, op, "text")
        c["replies"].append({"author": op.get("author") or self.author, "text": text})

    def op_resolve_comment(self, i: int, op: JSON) -> None:
        self.require_comment(i, op)["done"] = bool(op.get("done", True))

    def op_delete_comment(self, i: int, op: JSON) -> None:
        c = self.require_comment(i, op)
        self.s["comments"].remove(c)

    def op_set_track_changes(self, i: int, op: JSON) -> None:
        self.s["track"] = bool(op.get("on"))

    def op_accept(self, i: int, op: JSON) -> None:
        self.review(i, op, accept=True)

    def op_reject(self, i: int, op: JSON) -> None:
        self.review(i, op, accept=False)

    def review(self, i: int, op: JSON, *, accept: bool) -> None:
        sets = self.s["change_sets"]
        if op.get("change_set"):
            cs = sets.get(op["change_set"])
            if cs is None:
                raise make_error(
                    "NOT_FOUND",
                    f"There is no change set {op['change_set']}.",
                    version=self.doc.version,
                    op_index=i,
                    op=op["op"],
                    what="change_set",
                )
            chosen = [cs]
        elif op.get("all") is True:
            chosen = list(sets.values())
        elif op.get("author"):
            chosen = [cs for cs in sets.values() if cs["author"] == op["author"]]
        elif op.get("revision_ids"):
            raise self.doc._unsupported(op["op"] + " by revision_ids", i)
        else:
            raise self.doc._invalid(
                f"{op['op']} needs a target", f"/ops/{i}", op["op"], op_index=i, op=op["op"]
            )
        for cs in chosen:
            if cs["status"] != "open":
                continue
            for block in list(self.s["blocks"]):
                if block["cs"] != cs["id"]:
                    continue
                self.settle(block, accept=accept)
            cs["status"] = "accepted" if accept else "rejected"
            self.review_cs[cs["id"]] = cs

    def settle(self, block: JSON, *, accept: bool) -> None:
        gone = (accept and block["del"]) or (not accept and block["ins"])
        if gone:
            self.s["blocks"].remove(block)
            self.removed.append(block["id"])
            return
        if not accept and block["old"] is not None:
            block["text"] = block["old"]
        block.update(old=None, ins=None, cs=None, rev=None)
        block["del"] = None
        self.touch(block)


def _base36(n: int) -> str:
    digits = "0123456789abcdefghijklmnopqrstuvwxyz"
    out = ""
    while True:
        n, r = divmod(n, 36)
        out = digits[r] + out
        if not n:
            return out


def _context(text: str, start: int, length: int) -> str:
    lo, hi = max(0, start - 30), min(len(text), start + length + 30)
    return ("…" if lo else "") + text[lo:hi] + ("…" if hi < len(text) else "")


def fake_bytes(*paragraphs: str) -> bytes:
    """A FakeEngine "file" with these paragraphs (``# `` makes a heading)."""
    return ("\n".join(paragraphs) + "\n").encode("utf-8")


__all__ = ["MUTATING_OPS", "READ_OPS", "FakeDocument", "FakeEngine", "fake_bytes", "serial"]
