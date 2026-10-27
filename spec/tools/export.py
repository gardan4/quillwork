#!/usr/bin/env python3
"""Emit the neutral tool definitions in spec/tools/*.json as provider shapes.

    python spec/tools/export.py anthropic        # Messages API `tools`
    python spec/tools/export.py openai           # Chat Completions `tools`, strict mode
    python spec/tools/export.py openai-responses # Responses API `tools`, strict mode
    python spec/tools/export.py mcp              # MCP `tools/list` result items

Standard library only. The neutral format is {name, description, parameters}; parameters is
already a strict-mode JSON Schema object, so every shape carries it unchanged.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

TOOLS_DIR = Path(__file__).resolve().parent

# Order the tools are presented in; it is part of the stable prompt prefix.
ORDER = ["read_document", "find", "edit", "format", "comment", "table", "review", "advanced_op"]

SHAPES = ("anthropic", "openai", "openai-responses", "mcp")


def load_tools() -> list[dict]:
    found = {p.stem: json.loads(p.read_text(encoding="utf-8")) for p in TOOLS_DIR.glob("*.json")}
    missing = [n for n in ORDER if n not in found]
    extra = sorted(set(found) - set(ORDER))
    if missing or extra:
        raise SystemExit(f"tool files out of sync with ORDER: missing={missing} extra={extra}")
    for name, tool in found.items():
        if tool.get("name") != name:
            raise SystemExit(f"{name}.json declares name {tool.get('name')!r}")
    return [found[n] for n in ORDER]


def to_shape(tool: dict, shape: str) -> dict:
    name, desc, params = tool["name"], tool["description"], tool["parameters"]
    if shape == "anthropic":
        return {"name": name, "description": desc, "input_schema": params}
    if shape == "openai":
        return {"type": "function",
                "function": {"name": name, "description": desc, "parameters": params, "strict": True}}
    if shape == "openai-responses":
        return {"type": "function", "name": name, "description": desc, "parameters": params, "strict": True}
    if shape == "mcp":
        return {"name": name, "description": desc, "inputSchema": params}
    raise ValueError(f"unknown shape {shape!r}; expected one of {', '.join(SHAPES)}")


def export(shape: str) -> list[dict]:
    return [to_shape(t, shape) for t in load_tools()]


def main(argv: list[str]) -> int:
    if len(argv) != 2 or argv[1] not in SHAPES:
        print(f"usage: {Path(argv[0]).name} {{{'|'.join(SHAPES)}}}", file=sys.stderr)
        return 2
    json.dump(export(argv[1]), sys.stdout, indent=2, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
