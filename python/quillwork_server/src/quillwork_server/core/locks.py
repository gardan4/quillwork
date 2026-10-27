"""One writer per document, and wake-ups for live viewers.

Both are protocols a host replaces:

- :class:`LockProvider` - a Postgres host can wrap ``pg_advisory_lock(key(document_id))``
  (session-level, never the ``_xact_`` variant: the store commits in the middle);
- :class:`Notifier` - a Postgres host can use ``LISTEN``/``NOTIFY`` so a viewer connected to one
  web server hears about a commit made by another.

The in-process defaults are :class:`LocalLocks` and :class:`LocalNotifier`.
"""

from __future__ import annotations

import threading
import time
from collections.abc import Iterator
from contextlib import AbstractContextManager, contextmanager
from typing import Protocol, runtime_checkable


class LockTimeout(Exception):
    """The document's writer lock was not free within the timeout."""

    def __init__(self, document_id: str, timeout: float) -> None:
        super().__init__(f"document {document_id!r} is locked by another writer ({timeout:g}s)")
        self.document_id = document_id
        self.timeout = timeout


@runtime_checkable
class LockProvider(Protocol):
    def lock(self, document_id: str, timeout: float) -> AbstractContextManager[None]:
        """Hold the document's writer lock for the ``with`` block.

        Raises :class:`LockTimeout` when it cannot be taken within ``timeout`` seconds.
        """
        ...


class LocalLocks:
    """A ``threading.Lock`` per document id, for a single process."""

    def __init__(self) -> None:
        self._guard = threading.Lock()
        self._locks: dict[str, threading.Lock] = {}

    def _get(self, document_id: str) -> threading.Lock:
        with self._guard:
            lock = self._locks.get(document_id)
            if lock is None:
                lock = self._locks[document_id] = threading.Lock()
            return lock

    @contextmanager
    def lock(self, document_id: str, timeout: float) -> Iterator[None]:
        lock = self._get(document_id)
        if not lock.acquire(timeout=timeout):
            raise LockTimeout(document_id, timeout)
        try:
            yield
        finally:
            lock.release()


@runtime_checkable
class Notifier(Protocol):
    def publish(self, document_id: str, version: int) -> None:
        """Announce that ``version`` is now the document's head."""
        ...

    def wait(self, document_id: str, after: int, timeout: float) -> int | None:
        """Block until a version newer than ``after`` is announced; return it, or ``None``."""
        ...


class LocalNotifier:
    """Condition-variable fan-out inside one process."""

    def __init__(self) -> None:
        self._cond = threading.Condition()
        self._latest: dict[str, int] = {}

    def publish(self, document_id: str, version: int) -> None:
        with self._cond:
            if version > self._latest.get(document_id, 0):
                self._latest[document_id] = version
            self._cond.notify_all()

    def wait(self, document_id: str, after: int, timeout: float) -> int | None:
        deadline = time.monotonic() + timeout
        with self._cond:
            while True:
                latest = self._latest.get(document_id, 0)
                if latest > after:
                    return latest
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return None
                self._cond.wait(remaining)
