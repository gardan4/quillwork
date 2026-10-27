"""Plain data types shared by the store, the service and the front ends."""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any, Literal

#: A JSON object as the spec defines it (ops, results, patches, view models).
JSON = dict[str, Any]

#: How a version came to be.
VersionKind = Literal["upload", "apply", "undo", "import", "restore"]


@dataclass(frozen=True, slots=True)
class VersionRecord:
    """Metadata of one stored version. ``version`` 1 is the upload; every later one has a parent."""

    document_id: str
    version: int
    parent: int | None
    kind: VersionKind
    created_at: float
    change_set: str | None = None
    author: str | None = None
    label: str | None = None
    turn: str | None = None
    #: The view-model patch from ``parent`` to ``version`` (``None`` when there is none, e.g. an
    #: upload or a re-import, after which a viewer reloads the view model).
    patch: JSON | None = None

    def to_dict(self) -> JSON:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: JSON) -> VersionRecord:
        return cls(
            document_id=str(data["document_id"]),
            version=int(data["version"]),
            parent=None if data.get("parent") is None else int(data["parent"]),
            kind=data["kind"],
            created_at=float(data["created_at"]),
            change_set=data.get("change_set"),
            author=data.get("author"),
            label=data.get("label"),
            turn=data.get("turn"),
            patch=data.get("patch"),
        )

    def summary(self) -> JSON:
        """The record without its patch (what version listings show)."""
        out = self.to_dict()
        out.pop("patch", None)
        return out


@dataclass(frozen=True, slots=True)
class DocumentInfo:
    id: str
    name: str
    head: int
    created_at: float

    def to_dict(self) -> JSON:
        return asdict(self)


@dataclass(frozen=True, slots=True)
class StoredVersion:
    """One version as the store keeps it: the full .docx, the engine sidecar and the record."""

    record: VersionRecord
    data: bytes
    sidecar: JSON | None = None


@dataclass(frozen=True, slots=True)
class Asset:
    data: bytes
    mime: str


@dataclass(frozen=True, slots=True)
class PatchFeed:
    """What a live viewer needs to move from ``since`` to ``version``.

    ``reload`` is true when the chain is broken (a re-import, or ``since`` is older than the
    retained history); the viewer then fetches the view model again instead of patching.
    """

    since: int
    version: int
    patches: list[JSON] = field(default_factory=list)
    reload: bool = False

    def to_dict(self) -> JSON:
        return asdict(self)
