"""The 8 agent tools (spec/tools/*.json) and their provider shapes.

Same logic as ``spec/tools/export.py``; the JSON files ship inside this package (a test keeps
them identical to the spec's), or load from a directory with ``load_tools(path)``.
"""

from __future__ import annotations

import json
from importlib import resources
from pathlib import Path
from typing import Final, Literal

from ..core.types import JSON

#: Order the tools are presented in; it is part of the stable prompt prefix.
ORDER: Final = (
    "read_document",
    "find",
    "edit",
    "format",
    "comment",
    "table",
    "review",
    "advanced_op",
)
SHAPES: Final = ("anthropic", "openai", "openai-responses", "mcp")
Shape = Literal["anthropic", "openai", "openai-responses", "mcp"]


def _read_dir(path: Path | None) -> dict[str, JSON]:
    if path is not None:
        return {
            p.stem: json.loads(p.read_text(encoding="utf-8")) for p in Path(path).glob("*.json")
        }
    root = resources.files("quillwork_server") / "spec" / "tools"
    return {
        entry.name.removesuffix(".json"): json.loads(entry.read_text(encoding="utf-8"))
        for entry in root.iterdir()
        if entry.name.endswith(".json")
    }


def load_tools(path: str | Path | None = None) -> list[JSON]:
    """The neutral tool definitions ``{name, description, parameters}``, in :data:`ORDER`."""
    found = _read_dir(Path(path) if path is not None else None)
    missing = [n for n in ORDER if n not in found]
    extra = sorted(set(found) - set(ORDER))
    if missing or extra:
        raise ValueError(f"tool files out of sync with ORDER: missing={missing} extra={extra}")
    for name, tool in found.items():
        if tool.get("name") != name:
            raise ValueError(f"{name}.json declares name {tool.get('name')!r}")
    return [found[n] for n in ORDER]


def to_shape(tool: JSON, shape: Shape | str) -> JSON:
    name, desc, params = tool["name"], tool["description"], tool["parameters"]
    if shape == "anthropic":
        return {"name": name, "description": desc, "input_schema": params}
    if shape == "openai":
        return {
            "type": "function",
            "function": {"name": name, "description": desc, "parameters": params, "strict": True},
        }
    if shape == "openai-responses":
        return {
            "type": "function",
            "name": name,
            "description": desc,
            "parameters": params,
            "strict": True,
        }
    if shape == "mcp":
        return {"name": name, "description": desc, "inputSchema": params}
    raise ValueError(f"unknown shape {shape!r}; expected one of {', '.join(SHAPES)}")


def export(shape: Shape | str, path: str | Path | None = None) -> list[JSON]:
    """All 8 tools in a provider's shape.

    ``shape`` is ``anthropic``, ``openai``, ``openai-responses`` or ``mcp``.
    """
    return [to_shape(t, shape) for t in load_tools(path)]
