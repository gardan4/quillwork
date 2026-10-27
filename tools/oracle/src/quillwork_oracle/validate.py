"""OOXML validity oracle: wraps Docxodus `verify_deliverable` (Open XML SDK schema + semantic validation, package
and relationship checks).

Docxodus is an untrusted downloaded package, so it runs in a separate interpreter started with `-I` and imported
from a directory outside this repo: `$QUILLWORK_DOCXODUS` (the directory that contains `docx_scalpel/`), else the
first `quillwork-tools-cache/docxodus` found next to the repo or one of its parents.

By default only validity findings count (categories package/openXml/relationship/structure at severity error).
`policy=True` also counts Docxodus's delivery-policy checks (placeholders, editorial markers, ...).
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

VALIDITY_CATEGORIES = {"package", "openXml", "relationship", "structure"}
RUNNER = Path(__file__).with_name("_validate_runner.py")


def docxodus_dir() -> Path:
    env = os.environ.get("QUILLWORK_DOCXODUS")
    if env:
        p = Path(env).expanduser()
        if not (p / "docx_scalpel").is_dir():
            raise FileNotFoundError("QUILLWORK_DOCXODUS=%s has no docx_scalpel/ directory" % p)
        return p
    for parent in Path(__file__).resolve().parents:
        cand = parent / "quillwork-tools-cache" / "docxodus"
        if (cand / "docx_scalpel").is_dir():
            return cand
    raise FileNotFoundError("Docxodus not found: set QUILLWORK_DOCXODUS to the directory containing docx_scalpel/")


def _precheck(path: str) -> dict | None:
    """Errors Docxodus would report less clearly: not a zip, no content types."""
    try:
        with zipfile.ZipFile(path) as z:
            names = set(z.namelist())
    except (zipfile.BadZipFile, OSError) as e:
        return {"code": "oracle.not_a_zip", "category": "package", "severity": "error", "message": str(e)}
    if "[Content_Types].xml" not in names:
        return {"code": "oracle.no_content_types", "category": "package", "severity": "error",
                "message": "zip has no [Content_Types].xml"}
    return None


def _counts(f: dict, policy: bool) -> bool:
    if f.get("severity") != "error" and not f.get("blocksDelivery"):
        return False
    if f.get("disposition") in ("preExisting", "resolved"):
        return False  # only with a baseline: errors already present in the original
    return policy or f.get("category") in VALIDITY_CATEGORIES


def _slim(f: dict) -> dict:
    keep = ("code", "category", "severity", "disposition", "message", "owningPartUri", "xPath")
    return {k: f[k] for k in keep if f.get(k) not in (None, "")}


def validate(paths: list[str], *, policy: bool = False, baselines: list[str | None] | None = None,
             timeout: float = 1800) -> list[dict]:
    """Validate files. Returns one record per path:
    {"path", "ok", "decision", "errors": [finding...], "warnings": n, "seconds"}."""
    baselines = baselines or [None] * len(paths)
    results: dict[str, dict] = {}
    todo = []
    for p, b in zip(paths, baselines):
        pre = _precheck(p)
        if pre:
            results[p] = {"path": p, "ok": False, "decision": "notEvaluated", "errors": [pre], "warnings": 0}
        else:
            todo.append({"path": os.path.abspath(p), "baseline": os.path.abspath(b) if b else None, "orig": p})
    if todo:
        python = os.environ.get("QUILLWORK_DOCXODUS_PYTHON", sys.executable)
        with tempfile.TemporaryDirectory(prefix="qw-validate-") as td:
            req = Path(td) / "request.json"
            req.write_text(json.dumps({"files": [{"path": t["path"], "baseline": t["baseline"]} for t in todo]}))
            proc = subprocess.run([python, "-I", str(RUNNER), str(docxodus_dir()), str(req)], capture_output=True,
                                  text=True, timeout=timeout, cwd=td)
        lines = [json.loads(x) for x in proc.stdout.splitlines() if x.startswith("{")]
        by_path = {x["path"]: x for x in lines}
        for t in todo:
            x = by_path.get(t["path"])
            rec: dict = {"path": t["orig"]}
            if x is None:
                rec.update(ok=False, decision="notEvaluated", warnings=0, errors=[{
                    "code": "oracle.runner_failed", "category": "package", "severity": "error",
                    "message": (proc.stderr or "no output")[-2000:]}])
            elif "exception" in x:
                rec.update(ok=False, decision="notEvaluated", warnings=0, seconds=x["seconds"], errors=[{
                    "code": "oracle.docxodus_exception", "category": "package", "severity": "error",
                    "message": x["exception"]}])
            else:
                r = x["result"]
                findings = r.get("findings", [])
                errors = [_slim(f) for f in findings if _counts(f, policy)]
                decision = r.get("decision")
                if decision == "notEvaluated" and not errors:
                    errors.append({"code": "oracle.not_evaluated", "category": "package", "severity": "error",
                                   "message": "Docxodus could not evaluate the file"})
                rec.update(ok=not errors, decision=decision, errors=errors, seconds=x["seconds"],
                           warnings=sum(1 for f in findings if f.get("severity") == "warning"))
            results[t["orig"]] = rec
    return [results[p] for p in paths]
