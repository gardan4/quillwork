"""Conformance tests for the Quillwork spec itself.

    uv venv && uv pip install jsonschema tiktoken pytest
    .venv/bin/pytest spec/tests -s     # -s prints the token report

Checks: schemas against the 2020-12 metaschema, every example against its schema, op and error
coverage, the agent tools against OpenAI strict mode and Anthropic's rules, and token budgets.
"""
from __future__ import annotations

import json
import re
import sys
from functools import lru_cache
from pathlib import Path

import pytest
import tiktoken
from jsonschema import Draft202012Validator
from referencing import Registry, Resource

SPEC = Path(__file__).resolve().parents[1]
SCHEMA_DIR = SPEC / "schema"
EXAMPLES = SPEC / "examples"
TOOLS_DIR = SPEC / "tools"
BASE = "https://quillwork.example/spec/v0/"

sys.path.insert(0, str(TOOLS_DIR))
import export  # noqa: E402

ENC = tiktoken.get_encoding("o200k_base")
TOOL_TOKEN_BUDGET = 2000
MAX_TOOLS = 8
# Page-equivalent for the projection report: 1,800 characters of plain text. Calibrated so the
# "plain lines" row in DESIGN §1 (447 tokens/page, measured on a private 46-page document) lands
# near its measured value for Dutch prose (~4 characters per o200k token).
PAGE_CHARS = 1800


def tokens(text: str) -> int:
    return len(ENC.encode(text))


def compact(obj) -> str:
    return json.dumps(obj, separators=(",", ":"), ensure_ascii=False)


# ---------------------------------------------------------------- schemas

SCHEMA_FILES = sorted(SCHEMA_DIR.glob("*.schema.json"))


@lru_cache
def schemas() -> dict[str, dict]:
    return {p.name: json.loads(p.read_text(encoding="utf-8")) for p in SCHEMA_FILES}


@lru_cache
def registry() -> Registry:
    return Registry().with_resources(
        (s["$id"], Resource.from_contents(s)) for s in schemas().values()
    )


def validator(ref: str) -> Draft202012Validator:
    """ref: 'ops.schema.json' or 'ops.schema.json#/$defs/OpResult'."""
    return Draft202012Validator({"$ref": BASE + ref}, registry=registry())


def errors_of(ref: str, instance) -> list[str]:
    return [
        f"{'/'.join(map(str, e.absolute_path)) or '<root>'}: {e.message[:300]}"
        for e in validator(ref).iter_errors(instance)
    ]


@pytest.mark.parametrize("path", SCHEMA_FILES, ids=lambda p: p.name)
def test_schema_is_valid_2020_12(path):
    schema = json.loads(path.read_text(encoding="utf-8"))
    assert schema["$schema"] == "https://json-schema.org/draft/2020-12/schema"
    assert schema["$id"] == BASE + path.name
    Draft202012Validator.check_schema(schema)


def test_all_refs_resolve():
    """Walk every $ref in every schema and resolve it through the registry."""
    resolver = registry().resolver()

    def walk(node, base):
        if isinstance(node, dict):
            if "$ref" in node:
                resolver.lookup(_join(base, node["$ref"]))
            for v in node.values():
                walk(v, base)
        elif isinstance(node, list):
            for v in node:
                walk(v, base)

    for name, s in schemas().items():
        walk(s, s["$id"])


def _join(base: str, ref: str) -> str:
    if ref.startswith("#"):
        return base + ref
    return BASE + ref


# ---------------------------------------------------------------- examples

def load(p: Path):
    return json.loads(p.read_text(encoding="utf-8"))


def example_targets() -> list[tuple[Path, str]]:
    """Every JSON example and the schema it must validate against (README: directory convention)."""
    out: list[tuple[Path, str]] = [
        (EXAMPLES / "view-model.json", "view-model.schema.json"),
        (EXAMPLES / "patch.json", "patch.schema.json"),
    ]
    out += [(p, "ops.schema.json") for p in sorted((EXAMPLES / "ops").glob("*.json"))]
    out += [(p, "error.schema.json") for p in sorted((EXAMPLES / "errors").glob("*.json"))]
    for p in sorted((EXAMPLES / "results").glob("*.json")):
        kind = "OpResult" if p.name.startswith("op-result") else "ReadResult"
        out.append((p, f"ops.schema.json#/$defs/{kind}"))
    return out


@pytest.mark.parametrize("path,ref", example_targets(), ids=lambda x: x.name if isinstance(x, Path) else x)
def test_example_validates(path, ref):
    errs = errors_of(ref, load(path))
    assert not errs, "\n".join(errs)


def test_no_orphan_examples():
    known = {p for p, _ in example_targets()}
    known |= set((EXAMPLES / "tool-calls").glob("*.json"))
    known |= {EXAMPLES / "anchors.json", EXAMPLES / "projection-accepted.txt", EXAMPLES / "projection-markup.txt", PROSE}
    present = {p for p in EXAMPLES.rglob("*") if p.is_file()}
    assert present == known, f"orphans: {sorted(map(str, present - known))}; missing: {sorted(map(str, known - present))}"


def op_names(kind: str) -> list[str]:
    defs = schemas()["ops.schema.json"]["$defs"]
    return [defs[r["$ref"].split("/")[-1]]["properties"]["op"]["const"] for r in defs[kind]["oneOf"]]


def test_every_op_has_an_example():
    seen = set()
    for p in (EXAMPLES / "ops").glob("*.json"):
        doc = load(p)
        for op in doc.get("ops", [doc]):
            seen.add(op["op"])
        if not p.name.startswith("_"):
            ops = doc.get("ops", [doc])
            assert ops[0]["op"] == p.stem, f"{p.name} should hold op {p.stem}"
    every = set(op_names("MutatingOp")) | set(op_names("ReadOp"))
    assert every - seen == set(), f"ops without an example: {sorted(every - seen)}"


def test_op_names_unique():
    names = op_names("MutatingOp") + op_names("ReadOp")
    assert len(names) == len(set(names))


def test_every_error_code_has_an_example():
    codes = schemas()["error.schema.json"]["$defs"]["Code"]["enum"]
    files = {p.stem: load(p) for p in (EXAMPLES / "errors").glob("*.json")}
    assert sorted(files) == sorted(codes)
    for stem, doc in files.items():
        assert doc["error"]["code"] == stem


def test_error_contract_rejects_missing_details():
    bad = {"spec_version": "0.1.0", "ok": False,
           "error": {"code": "STALE_ANCHOR", "message": "x", "retryable": True, "version": 3, "op_index": 0}}
    assert errors_of("error.schema.json", bad), "STALE_ANCHOR without anchor/current must fail"
    bad["error"].update(code="AMBIGUOUS", anchor="b100",
                        candidates=[{"anchor": "b100", "occurrence": 1, "context": "x"}])
    assert errors_of("error.schema.json", bad), "AMBIGUOUS needs at least two candidates"


def test_ops_reject_bad_guards():
    tx = load(EXAMPLES / "ops" / "replace_text.json")
    for mutate in (
        lambda t: t.pop("base_version"),
        lambda t: t["ops"][0].update(expect={"text": "a", "hash": "sha256:000000000000"}),
        lambda t: t["ops"][0].update(mode="sometimes"),
        lambda t: t["ops"][0].update(colour="red"),
        lambda t: t.update(ops=[]),
    ):
        t = json.loads(json.dumps(tx))
        mutate(t)
        assert errors_of("ops.schema.json", t), "mutated transaction should be rejected"
    one_dest = load(EXAMPLES / "ops" / "insert_paragraph.json")
    one_dest["ops"][0]["before"] = "b100"
    assert errors_of("ops.schema.json", one_dest), "after and before together must be rejected"


def test_anchor_examples():
    data = load(EXAMPLES / "anchors.json")
    for kind, values in data["valid"].items():
        for v in values:
            assert not errors_of(f"common.schema.json#/$defs/{kind}", v), f"{kind}: {v!r} should be valid"
    for kind, values in data["invalid"].items():
        for v in values:
            assert errors_of(f"common.schema.json#/$defs/{kind}", v), f"{kind}: {v!r} should be invalid"


def test_semver_and_document_counter_definitions():
    for version in ["0.0.0", "0.1.42", "1.0.0", "0.1.0-alpha.1", "0.1.0-0", "0.1.0--",
                    "0.1.0+build.001", "0.0.9-rc.2+build-a.01", "0.1.999999999999999999999999999999"]:
        assert not errors_of("common.schema.json#/$defs/SpecVersion", version), version
    for version in [None, True, 1, "", "nonsense", "0.1", "v0.1.0", "0.1.0.0", "00.1.0",
                    "0.01.0", "0.1.00", " 0.1.0", "0.1.0\n", "0.1.0-", "0.1.0+", "0.1.0-01",
                    "0.1.0-alpha..1", "0.1.0-a_1", "0.1.0+build..1", "0.1.0+build+other", "0.1.0-é"]:
        assert errors_of("common.schema.json#/$defs/SpecVersion", version), version
    for counter in [0, 1, 2, 2**64 - 1]:
        assert not errors_of("common.schema.json#/$defs/Version", counter)
    for counter in [-1, 0.5, "0", None, True]:
        assert errors_of("common.schema.json#/$defs/Version", counter)


# ---------------------------------------------------------------- projection

PROJECTIONS = [EXAMPLES / "projection-accepted.txt", EXAMPLES / "projection-markup.txt"]
# Prose-heavy body text only (no view model): the representative sample for the per-page numbers.
PROSE = EXAMPLES / "projection-prose.txt"
LINE_RE = re.compile(schemas()["common.schema.json"]["$defs"]["ProjectionLine"]["pattern"])
BLOCK_RE = re.compile(r"^ *\[([^\s\]]+)( [^\]]*)?\] ?(.*)$")


def block_ids(vm) -> set[str]:
    out = set()

    def walk(blocks):
        for b in blocks:
            out.add(b["id"])
            for r in b.get("rows", []):
                out.add(r["id"])
                for c in r["cells"]:
                    out.add(c["id"])
                    walk(c["blocks"])

    walk(vm["blocks"])
    for s in vm["stories"].values():
        walk(s["blocks"])
    for c in vm["comments"]:
        out.add(c["id"])
        walk(c["blocks"])
    return out


@pytest.mark.parametrize("path", PROJECTIONS, ids=lambda p: p.name)
def test_projection_lines_and_anchors(path):
    vm = load(EXAMPLES / "view-model.json")
    ids = block_ids(vm)
    lines = path.read_text(encoding="utf-8").splitlines()
    assert lines[0].startswith("§ doc ") and f"· v{vm['version']} ·" in lines[0]
    for line in lines:
        if not line:
            continue
        assert LINE_RE.match(line), f"not a projection line: {line!r}"
        m = BLOCK_RE.match(line)
        if m and not line.startswith("§"):
            assert m.group(1) in ids, f"unknown anchor {m.group(1)} in {line!r}"
    top = [b["id"] for b in vm["blocks"]]
    shown = [BLOCK_RE.match(l).group(1) for l in lines if l.startswith("[")]
    assert top == [a for a in shown if a in top], "body blocks must appear once each, in order"


def test_result_lines_come_from_the_projection():
    acc = set(PROJECTIONS[0].read_text(encoding="utf-8").splitlines())
    mk = set(PROJECTIONS[1].read_text(encoding="utf-8").splitlines())
    for p in list((EXAMPLES / "results").glob("*.json")) + list((EXAMPLES / "errors").glob("*.json")):
        doc = load(p)
        lines = doc.get("changed") or doc.get("error", {}).get("current") or []
        if doc.get("text"):
            lines = doc["text"].splitlines()
        for line in lines:
            assert line in acc | mk, f"{p.name}: line not in the example projection: {line!r}"


def strip_markup(text: str) -> str:
    text = re.sub(r"\{--.*?--\}", "", text)
    text = re.sub(r"\{\+\+(.*?)\+\+\}", r"\1", text)
    text = re.sub(r"\{>>c\d+<<\}", "", text)
    text = re.sub(r"⟨[^⟩]*⟩", "", text)
    text = re.sub(r"⟦[^⟧]*⟧.*?⟦/⟧", "", text)
    text = re.sub(r'⟪[^⟫]*?→ "([^"]*)"⟫', r"\1", text)
    text = re.sub(r"\*\*(.*?)\*\*", r"\1", text)
    return text


def _label(attrs: str | None) -> str:
    m = re.search(r" #(\S+)", attrs or "")
    return m.group(1) + " " if m else ""


def projection_report(path: Path) -> dict:
    lines = [l for l in path.read_text(encoding="utf-8").splitlines()]
    body = [l for l in lines if BLOCK_RE.match(l) and not l.startswith("§")]
    # "Line projection, no anchors" keeps what a reader sees (list and heading labels), so the
    # overhead measured is the handle and kind only.
    no_anchor = [_label(BLOCK_RE.match(l).group(2)) + BLOCK_RE.match(l).group(3) for l in body]
    plain = [strip_markup(l) for l in no_anchor]
    full_t = tokens("\n".join(lines))
    body_t, noa_t, plain_t = (tokens("\n".join(x)) for x in (body, no_anchor, plain))
    pages = sum(len(l) for l in plain) / PAGE_CHARS
    return {
        "file": path.name,
        "page_equivalents": round(pages, 2),
        "tokens_full": full_t,
        "tokens_per_page_full": round(full_t / pages),
        "tokens_per_page_body": round(body_t / pages),
        "tokens_per_page_plain": round(plain_t / pages),
        "anchor_overhead": round((body_t - noa_t) / body_t, 3),
        "ratio_vs_plain": round(body_t / plain_t, 3),
    }


def test_projection_token_report():
    for line in PROSE.read_text(encoding="utf-8").splitlines():
        assert not line or LINE_RE.match(line), f"not a projection line: {line!r}"
    for p in [*PROJECTIONS, PROSE]:
        r = projection_report(p)
        print("\nprojection", json.dumps(r, ensure_ascii=False))
        assert r["tokens_full"] <= 8000, "first call must fit the 8k budget"


# ---------------------------------------------------------------- tools

TOOL_FILES = sorted(p for p in TOOLS_DIR.glob("*.json"))
NAME_RE = re.compile(r"^[a-zA-Z0-9_-]{1,64}$")
# Keywords every target accepts in strict mode. No format/pattern/min*/max*/default: OpenAI strict
# mode rejects or ignores them, and dropping them keeps the schemas small.
ALLOWED = {"type", "properties", "required", "additionalProperties", "items", "enum", "const",
           "description", "anyOf", "$ref", "$defs"}
COMBINATORS = {"anyOf", "oneOf", "allOf", "not", "if", "then", "else"}


def tools() -> list[dict]:
    return export.load_tools()


def test_tool_count_and_names():
    ts = tools()
    assert len(ts) <= MAX_TOOLS
    assert len({t["name"] for t in ts}) == len(ts)
    for t in ts:
        assert set(t) == {"name", "description", "parameters"}
        assert NAME_RE.match(t["name"])
        assert 0 < len(t["description"]) <= 1024


def _walk_strict(node, path, depth, problems, defs_seen):
    if not isinstance(node, dict):
        problems.append(f"{path}: schema node is not an object")
        return
    for k in node:
        if k not in ALLOWED:
            problems.append(f"{path}: keyword {k!r} not allowed in strict tool schemas")
    if "oneOf" in node or "allOf" in node:
        problems.append(f"{path}: use anyOf, not oneOf/allOf")
    if "$ref" in node:
        ref = node["$ref"]
        if not ref.startswith("#/$defs/"):
            problems.append(f"{path}: only local $defs refs")
        elif ref in defs_seen:
            problems.append(f"{path}: recursive $ref {ref}")
    types = node.get("type")
    types = types if isinstance(types, list) else [types] if types else []
    if "enum" in node and "null" in types and None not in node["enum"]:
        problems.append(f"{path}: nullable enum must list null")
    if "object" in types:
        if depth > 5:
            problems.append(f"{path}: nested deeper than 5 levels")
        props = node.get("properties", {})
        if node.get("additionalProperties") is not False:
            problems.append(f"{path}: additionalProperties must be false")
        if sorted(node.get("required", [])) != sorted(props):
            problems.append(f"{path}: every property must be required (use a nullable type for optional)")
        for k, v in props.items():
            _walk_strict(v, f"{path}.{k}", depth + 1, problems, defs_seen)
    if "array" in types:
        if "items" not in node:
            problems.append(f"{path}: array without items")
        else:
            _walk_strict(node["items"], f"{path}[]", depth + 1, problems, defs_seen)
    for v in node.get("anyOf", []):
        _walk_strict(v, f"{path}|", depth, problems, defs_seen)
    if not types and "anyOf" not in node and "$ref" not in node and "const" not in node:
        problems.append(f"{path}: no type")


@pytest.mark.parametrize("path", TOOL_FILES, ids=lambda p: p.stem)
def test_tool_is_strict_mode_and_anthropic_compatible(path):
    tool = load(path)
    params = tool["parameters"]
    Draft202012Validator.check_schema(params)
    # Anthropic: input_schema must be an object schema; OpenAI: root must be an object, not a combinator.
    assert params.get("type") == "object"
    assert not (COMBINATORS & set(params)), "no top-level combinators"
    assert "enum" not in params and "not" not in params
    problems: list[str] = []
    _walk_strict(params, tool["name"], 1, problems, set())
    assert not problems, "\n".join(problems)


def test_tool_call_examples_validate():
    by_name = {t["name"]: t for t in tools()}
    files = {p.stem: load(p) for p in (EXAMPLES / "tool-calls").glob("*.json")}
    assert sorted(files) == sorted(by_name), "one tool-call example per tool"
    for name, call in files.items():
        errs = [e.message for e in Draft202012Validator(by_name[name]["parameters"]).iter_errors(call)]
        assert not errs, f"{name}: {errs}"
    args = json.loads(files["advanced_op"]["args"])
    assert isinstance(args, dict)


def test_advanced_op_args_follow_the_op_schema():
    call = load(EXAMPLES / "tool-calls" / "advanced_op.json")
    tx = {"base_version": call["base_version"], "ops": [{"op": call["op"], **json.loads(call["args"])}]}
    errs = errors_of("ops.schema.json", tx)
    assert not errs, errs


@pytest.mark.parametrize("shape", export.SHAPES)
def test_export_shapes(shape):
    out = export.export(shape)
    assert len(out) == len(tools())
    for item in out:
        if shape == "anthropic":
            assert set(item) == {"name", "description", "input_schema"}
            assert item["input_schema"]["type"] == "object"
        elif shape == "openai":
            assert item["type"] == "function" and item["function"]["strict"] is True
            assert set(item["function"]) == {"name", "description", "parameters", "strict"}
        elif shape == "openai-responses":
            assert item["type"] == "function" and item["strict"] is True
        elif shape == "mcp":
            assert set(item) == {"name", "description", "inputSchema"}
            assert item["inputSchema"]["type"] == "object"


def test_tool_token_budget():
    ts = tools()
    per_tool = {t["name"]: tokens(compact(t)) for t in ts}
    neutral = tokens(compact(ts))
    shapes = {s: tokens(compact(export.export(s))) for s in export.SHAPES}
    print("\ntool tokens (o200k_base, compact JSON):", json.dumps(per_tool))
    print(f"tool tokens total: neutral={neutral} sum_per_tool={sum(per_tool.values())} "
          + " ".join(f"{k}={v}" for k, v in shapes.items()))
    assert neutral <= TOOL_TOKEN_BUDGET
    for s, n in shapes.items():
        assert n <= TOOL_TOKEN_BUDGET, f"{s}: {n} tokens"
