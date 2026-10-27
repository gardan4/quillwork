"""Run a tool call against a document: compile, apply or read, render.

:class:`ToolRunner` is what an agent host calls with the model's tool-use blocks. It is the
same for every front end: the MCP server wraps it, an embedding host calls it in-process.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Final

from quillwork.errors import QuillworkError

from ..core.errors import envelope
from ..core.service import DocumentService
from ..core.types import JSON
from .compile import Compiled, ReadCall, compile_call, translate_error
from .render import fence, render_error, render_op_result, render_read
from .spec import ORDER

_TOP_LEVEL: Final = re.compile(r"^\[([^\s\]]+)", re.MULTILINE)


@dataclass(frozen=True, slots=True)
class ToolResult:
    """What goes back to the model: ``text`` (rendered, fenced) and the spec JSON."""

    text: str
    structured: JSON
    is_error: bool
    version: int | None


class ToolRunner:
    """Runs the 8 spec tools on a :class:`DocumentService`.

    ``author`` signs every transaction (the agent's display name; the host decides it, not the
    model). ``fence`` wraps results in the untrusted-data block (DESIGN §12); turn it off only
    when the host fences tool results itself.
    """

    tools: Final = ORDER

    def __init__(
        self,
        service: DocumentService,
        *,
        author: str | None = None,
        fence: bool = True,
    ) -> None:
        self.service = service
        self.author = author
        self.fence = fence

    def call(
        self,
        document_id: str,
        tool: str,
        args: JSON,
        *,
        turn: str | None = None,
    ) -> ToolResult:
        call: Compiled | None = None
        try:
            head = self.service.head(document_id)
            call = compile_call(
                tool, args, version=head, next_block=lambda a: self._next_block(document_id, a)
            )
            if isinstance(call, ReadCall):
                result = self.service.read_op(document_id, call.request)
                text = render_read(result)
            else:
                result = self.service.apply(
                    document_id, call.transaction, author=self.author, turn=turn
                )
                text = render_op_result(result, review=call.review)
        except QuillworkError as exc:
            if call is not None:
                exc = translate_error(exc, call)
            body = envelope(exc)
            return self._out(document_id, render_error(exc), body, True, exc.version)
        version = result.get("version")
        return self._out(
            document_id, text, result, False, version if isinstance(version, int) else None
        )

    def _out(
        self, document_id: str, text: str, structured: JSON, is_error: bool, version: int | None
    ) -> ToolResult:
        if self.fence:
            text = fence(text, document=document_id, version=version)
        return ToolResult(text, structured, is_error, version)

    def _next_block(self, document_id: str, anchor: str) -> str | None:
        """The top-level block after ``anchor`` (for ``edit`` ``merge``)."""
        result = self.service.read_op(
            document_id, {"op": "read", "range": f"{anchor}..", "mode": "accepted", "budget": 500}
        )
        anchors = _TOP_LEVEL.findall(result.get("text") or "")
        if anchors and anchors[0] == anchor and len(anchors) > 1:
            return str(anchors[1])
        return None
