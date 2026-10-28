"""Preserving edits and template generation for existing PowerPoint presentations.

The original deck is the design. Text edits use anchored DrawingML paragraphs and
produce an application-owned candidate report, with hashes identifying exact bytes.
This API is separate from Word's tracked-change operation language and viewer.
"""

from __future__ import annotations

import json
import re
from collections.abc import Mapping
from typing import Any

from . import _native
from .errors import InvalidOpError, UnsupportedError

JsonObject = dict[str, Any]
_TOKEN = re.compile(r"\{\{([A-Za-z_][A-Za-z0-9_.-]*)\}\}")


class Presentation:
    """An existing .pptx or a branded deck used to generate a new candidate.

    ``version`` is local to this object. Include ``base_sha256`` from ``describe``
    in persisted requests to detect stale bytes across reopen and host processes.
    One object must not be called concurrently. Source bytes are never overwritten.
    """

    __slots__ = ("_inner",)

    def __init__(self, inner: _native.JsonPresentation) -> None:
        self._inner = inner

    @classmethod
    def open(
        cls, data: bytes | bytearray, limits: _native.Limits | None = None
    ) -> Presentation:
        """Open PPTX bytes with package/XML safety limits; no external links are fetched."""
        return cls(_native.JsonPresentation.open(data, limits))

    @property
    def version(self) -> int:
        return self._inner.version

    def read(self) -> str:
        """Compact anchored text, grouped by slide; document contents are untrusted data."""
        return self._inner.read()

    def describe(self) -> JsonObject:
        """Slides, shapes, paragraphs, restrictions, and current/source SHA256 digests."""
        result: JsonObject = json.loads(self._inner.describe_json())
        return result

    def apply(self, transaction: JsonObject | str) -> JsonObject:
        """Apply a guarded batch atomically; return changed paragraphs and candidate identity.

        The transaction requires an exact ``base_version``. For cross-session
        protection, also pass ``base_sha256`` from the last ``describe()`` call.
        Each ``replace_text`` targets one paragraph and supports a full-text
        ``expect`` guard and a one-based ``occurrence`` for repeated matches.
        """
        raw = (
            transaction
            if isinstance(transaction, str)
            else json.dumps(transaction, ensure_ascii=False)
        )
        result: JsonObject = json.loads(self._inner.apply_json(raw))
        return result

    def save(self) -> bytes:
        """Candidate PPTX bytes. A no-op save returns the original input exactly."""
        return self._inner.save()

    def review(self) -> JsonObject:
        """Before/after paragraphs and hashes; this is a report, not an approval decision.

        The host stores the original and candidate bytes and binds approval to
        this candidate's digest. Rejecting a candidate means retaining its original.
        Render affected slides separately to check layout and text fit.
        """
        result: JsonObject = json.loads(self._inner.review_json())
        return result

    def fill(
        self,
        values: Mapping[str, str],
        *,
        expected_counts: Mapping[str, int] | None = None,
    ) -> JsonObject:
        """Fill literal ``{{name}}`` slots atomically without evaluating template code.

        Slots may span runs, and replacement values are never parsed as slots.
        Every discovered slot must have a value and every provided key must occur.
        Optional counts must cover exactly the provided keys and match occurrences.
        Unsupported paragraphs are refused before mutation. The maximum batch is
        200 replacements; larger templates should be explicitly partitioned.
        """
        if not values:
            raise InvalidOpError(
                "fill needs at least one slot value", code="INVALID_OP"
            )
        for key, value in values.items():
            if not isinstance(key, str) or _TOKEN.fullmatch("{{" + key + "}}") is None:
                raise InvalidOpError(
                    "slot names must be identifiers such as client or client.name",
                    code="INVALID_OP",
                )
            if not isinstance(value, str):
                raise InvalidOpError("slot values must be strings", code="INVALID_OP")
        if expected_counts is not None and (
            set(expected_counts) != set(values)
            or any(type(n) is not int or n < 1 for n in expected_counts.values())
        ):
            raise InvalidOpError(
                "expected_counts must give a positive integer for every provided slot",
                code="INVALID_OP",
            )

        model = self.describe()
        counts = dict.fromkeys(values, 0)
        ops: list[JsonObject] = []
        for slide in model["slides"]:
            for shape in [*slide["shapes"], *slide["notes"]]:
                for paragraph in shape["paragraphs"]:
                    text: str = paragraph["text"]
                    matches = list(_TOKEN.finditer(text))
                    if not matches:
                        continue
                    if not paragraph["editable"]:
                        raise UnsupportedError(
                            f"slot in unsupported paragraph {paragraph['anchor']}",
                            code="UNSUPPORTED",
                        )
                    for match in matches:
                        key = match.group(1)
                        if key not in values:
                            raise InvalidOpError(
                                f"missing value for slot {key}", code="INVALID_OP"
                            )
                        counts[key] += 1
                    current = text
                    for match in reversed(matches):
                        token, key = match.group(0), match.group(1)
                        ops.append(
                            {
                                "op": "replace_text",
                                "anchor": paragraph["anchor"],
                                "find": token,
                                "replace": values[key],
                                "expect": current,
                                "occurrence": text.count(token, 0, match.start()) + 1,
                            }
                        )
                        current = (
                            current[: match.start()]
                            + values[key]
                            + current[match.end() :]
                        )
        missing = [key for key, count in counts.items() if count == 0]
        if missing:
            raise InvalidOpError(
                "slots not present in template: " + ", ".join(missing),
                code="INVALID_OP",
            )
        if expected_counts is not None and counts != dict(expected_counts):
            raise InvalidOpError(
                f"slot counts do not match: {counts}", code="INVALID_OP"
            )
        return self.apply(
            {
                "base_version": model["version"],
                "base_sha256": model["sha256"],
                "ops": ops,
            }
        )
