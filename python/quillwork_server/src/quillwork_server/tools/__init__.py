"""The agent tool layer: the 8 spec tools, compiled to ops, rendered per spec/projection.md §9."""

from .compile import ApplyCall, Compiled, ReadCall, compile_call, parse_marks, translate_error
from .dispatch import ToolResult, ToolRunner
from .render import DATA_NOTICE, fence, render_error, render_op_result, render_read
from .spec import ORDER, SHAPES, export, load_tools, to_shape

__all__ = [
    "DATA_NOTICE",
    "ORDER",
    "SHAPES",
    "ApplyCall",
    "Compiled",
    "ReadCall",
    "ToolResult",
    "ToolRunner",
    "compile_call",
    "export",
    "fence",
    "load_tools",
    "parse_marks",
    "render_error",
    "render_op_result",
    "render_read",
    "to_shape",
    "translate_error",
]
