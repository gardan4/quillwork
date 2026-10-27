"""Token meter: how many tokens does a text cost a model?

The default counter is tiktoken `o200k_base`, which runs locally and is a stable yardstick for comparing two
representations of the same document (it is not Claude's tokenizer; expect Claude counts to differ by a roughly
constant factor). `anthropic_count` asks the Anthropic count_tokens endpoint and is opt-in: it needs
ANTHROPIC_API_KEY and sends the text to Anthropic, so never use it on client documents.
"""
from __future__ import annotations

import json
import os
import urllib.request
from functools import lru_cache

ENCODING = "o200k_base"


@lru_cache(maxsize=1)
def _enc():
    import tiktoken

    return tiktoken.get_encoding(ENCODING)


def count(text: str) -> int:
    return len(_enc().encode(text, disallowed_special=()))


def anthropic_available() -> bool:
    return bool(os.environ.get("ANTHROPIC_API_KEY"))


def anthropic_count(text: str, model: str) -> int:
    if not anthropic_available():
        raise RuntimeError("ANTHROPIC_API_KEY is not set")
    body = json.dumps({"model": model, "messages": [{"role": "user", "content": text}]}).encode()
    req = urllib.request.Request(
        "https://api.anthropic.com/v1/messages/count_tokens", data=body, method="POST",
        headers={"x-api-key": os.environ["ANTHROPIC_API_KEY"], "anthropic-version": "2023-06-01",
                 "content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return int(json.load(r)["input_tokens"])


def measure(text: str, *, anthropic_model: str | None = None) -> dict:
    """anthropic_model set -> also ask the Anthropic count_tokens endpoint for that model."""
    out = {"chars": len(text), "bytes": len(text.encode("utf-8")), ENCODING: count(text)}
    if anthropic_model:
        out["anthropic"] = anthropic_count(text, anthropic_model)
    return out
