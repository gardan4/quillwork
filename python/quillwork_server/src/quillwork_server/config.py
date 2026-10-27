"""Settings shared by the two front ends (``quillwork-mcp``, ``quillwork-server``).

Every setting has an environment variable (``QUILLWORK_*``) and a command-line flag; the flag
wins. Library users skip this module and build a :class:`~.core.DocumentService` themselves.
"""

from __future__ import annotations

import argparse
import os
from collections.abc import Mapping
from dataclasses import dataclass, replace

from .core import (
    DocumentService,
    Engine,
    FakeEngine,
    FileStore,
    MemoryStore,
    QuillworkEngine,
    ServiceConfig,
    Store,
)

_DEFAULTS = ServiceConfig()


@dataclass(frozen=True, slots=True)
class Settings:
    #: Directory for :class:`~.core.FileStore`; empty keeps documents in memory.
    store: str = ""
    #: ``quillwork`` (the real binding) or ``fake`` (plain text; for demos and tests).
    engine: str = "quillwork"
    #: Author on every transaction an agent makes through these front ends.
    author: str = "AI"
    #: HTTP: required ``Authorization: Bearer <key>`` (or ``X-API-Key``); empty disables auth.
    api_key: str = ""
    max_upload_bytes: int = _DEFAULTS.max_upload_bytes
    max_ops_per_transaction: int = _DEFAULTS.max_ops_per_transaction
    host: str = "127.0.0.1"
    port: int = 8765

    @classmethod
    def from_env(cls, env: Mapping[str, str] | None = None) -> Settings:
        env = os.environ if env is None else env
        base = cls()
        return cls(
            store=env.get("QUILLWORK_STORE", base.store),
            engine=env.get("QUILLWORK_ENGINE", base.engine),
            author=env.get("QUILLWORK_AUTHOR", base.author),
            api_key=env.get("QUILLWORK_API_KEY", base.api_key),
            max_upload_bytes=int(env.get("QUILLWORK_MAX_UPLOAD_BYTES", base.max_upload_bytes)),
            max_ops_per_transaction=int(env.get("QUILLWORK_MAX_OPS", base.max_ops_per_transaction)),
            host=env.get("QUILLWORK_HOST", base.host),
            port=int(env.get("QUILLWORK_PORT", base.port)),
        )

    def service(self) -> DocumentService:
        engine: Engine = (
            FakeEngine(author=self.author) if self.engine == "fake" else QuillworkEngine()
        )
        store: Store = FileStore(self.store) if self.store else MemoryStore()
        config = ServiceConfig(
            max_upload_bytes=self.max_upload_bytes,
            max_ops_per_transaction=self.max_ops_per_transaction,
            default_author=self.author,
        )
        return DocumentService(engine, store, config=config)


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--store", help="directory to keep documents in (default: memory)")
    parser.add_argument("--engine", choices=("quillwork", "fake"))
    parser.add_argument("--author", help="author name on agent edits (default: AI)")
    parser.add_argument("--host")
    parser.add_argument("--port", type=int)


def from_args(args: argparse.Namespace, base: Settings | None = None) -> Settings:
    settings = base or Settings.from_env()
    changes = {
        key: getattr(args, key)
        for key in ("store", "engine", "author", "host", "port")
        if getattr(args, key, None) is not None
    }
    return replace(settings, **changes)
