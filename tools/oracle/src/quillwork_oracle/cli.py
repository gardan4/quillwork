"""`oracle` command line. Every subcommand prints JSON with --json and exits non-zero when its check fails."""
from __future__ import annotations

import argparse
import difflib
import json
import sys
from pathlib import Path


def _dump(obj) -> None:
    print(json.dumps(obj, indent=1, ensure_ascii=False, default=str))


def cmd_validate(a) -> int:
    from .validate import validate

    baselines = [a.baseline] * len(a.files) if a.baseline else None
    res = validate(a.files, policy=a.policy, baselines=baselines)
    if a.json:
        _dump(res)
    else:
        for r in res:
            print("%-6s %s  errors=%d warnings=%d  (%s, %.1fs)" % (
                "ok" if r["ok"] else "FAIL", r["path"], len(r["errors"]), r["warnings"], r["decision"],
                r.get("seconds") or 0))
            if not a.quiet:
                for e in r["errors"]:
                    print("    %s  %s" % (e["code"], e.get("message", "")[:160]))
    return 0 if all(r["ok"] for r in res) else 1


def cmd_render(a) -> int:
    from . import render

    try:
        if a.against:
            res = render.render_diff(a.file, a.against, dpi=a.dpi)
            ok = res["pages_a"] == res["pages_b"] and res["min_ssim"] >= a.min_ssim
        else:
            if a.out:
                paths = render.render(a.file, a.out, dpi=a.dpi)
                res = {"pages": len(paths), "png": [str(p) for p in paths]}
            else:
                res = {"pages": render.page_count(render.to_pdf(a.file))}
            ok = True
    except render.RenderError as e:
        res, ok = {"error": e.code, "message": str(e)}, False
    _dump(res) if a.json or a.against else print(res.get("pages", res.get("message")))
    return 0 if ok else 1


def _view_lines(path, mode, story):
    from . import text
    from .ooxml import Package

    pkg = Package.open(path)
    views = text.texts(pkg, mode)
    if story == "all":
        return ["[%s] %s" % (s, t) for s, ts in views.items() for t in ts]
    return views.get(story or pkg.main_part(), [])


def cmd_expect(a) -> int:
    from . import expect

    bad = 0
    for f in a.files:
        problems = expect.check(f)
        bad += bool(problems)
        print("%-4s %s" % ("ok" if not problems else "FAIL", f))
        for p in problems:
            print("    " + p)
    return 1 if bad else 0


def cmd_textdiff(a) -> int:
    from . import text

    if a.facts:
        _dump(text.facts(a.a))
        return 0
    la = _view_lines(a.a, a.mode, a.story)
    if not a.b:
        if a.json:
            _dump(la)
        else:
            print("\n".join(la))
        return 0
    lb = _view_lines(a.b, a.mode, a.story)
    diff = list(difflib.unified_diff(la, lb, a.a, a.b, lineterm="", n=1))
    print("\n".join(diff))
    return 1 if diff else 0


def cmd_locality(a) -> int:
    from .locality import DEFAULT_ALLOWED_PARTS, locality

    intended = [x for item in a.intended for x in item.split(",") if x]
    allowed = DEFAULT_ALLOWED_PARTS + a.allow if not a.strict else a.allow
    res = locality(a.original, a.edited, intended=intended, allowed_parts=allowed)
    if a.json:
        _dump(res)
    else:
        p = res["parts"]
        print("%s  parts changed=%d added=%d removed=%d recompressed=%d" % (
            "ok" if res["ok"] else "FAIL", len(p["changed"]), len(p["added"]), len(p["removed"]),
            len(p["recompressed"])))
        for part, rec in res["paragraphs"].items():
            print("  %s: %d -> %d paragraphs, changed %d, inserted %d, removed %d" % (
                part, rec["paragraphs_before"], rec["paragraphs_after"], len(rec["changed"]), len(rec["inserted"]),
                len(rec["removed"])))
        for s in res["intended"]:
            print("  intended %s#%s: %s" % (s["part"], s["index"], {k: v for k, v in s.items()
                                                                   if k not in ("part", "index", "new_index")}))
        for v in res["violations"]:
            print("  VIOLATION %s" % v)
    return 0 if res["ok"] else 1


def cmd_tokens(a) -> int:
    from . import text, tokens

    if a.anthropic_model and not tokens.anthropic_available():
        print("ANTHROPIC_API_KEY is not set; skipping the Anthropic count", file=sys.stderr)
        a.anthropic_model = None
    rows = []
    for f in a.files:
        if f.lower().endswith((".docx", ".docm")):
            if a.raw:
                from .ooxml import Package

                pkg = Package.open(f)
                body = pkg.data[pkg.main_part()].decode("utf-8", "replace")
            else:
                body = "\n".join(text.texts(f, a.mode)[_main(f)])
        else:
            body = Path(f).read_text(encoding="utf-8")
        rows.append({"file": f, **tokens.measure(body, anthropic_model=a.anthropic_model)})
    if a.json:
        _dump(rows)
    else:
        for r in rows:
            print("%8d tokens  %8d chars  %s" % (r[tokens.ENCODING], r["chars"], r["file"])
                  + ("  anthropic=%d" % r["anthropic"] if "anthropic" in r else ""))
    return 0


def _main(path) -> str:
    from .ooxml import Package

    return Package.open(path).main_part()


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="oracle", description="Quillwork verification oracles")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("validate", help="Docxodus validity check (errors only; policy findings with --policy)")
    p.add_argument("files", nargs="+")
    p.add_argument("--policy", action="store_true", help="also count workflow/delta/render/artifact findings")
    p.add_argument("--baseline", help="original document, for delta findings")
    p.add_argument("--json", action="store_true")
    p.add_argument("-q", "--quiet", action="store_true", help="counts only, no finding messages")
    p.set_defaults(fn=cmd_validate)

    p = sub.add_parser("render", help="LibreOffice render via gotenberg: page count, PNGs, or page diff")
    p.add_argument("file")
    p.add_argument("--out", help="write page-NNN.png here")
    p.add_argument("--against", help="second document: print per-page SSIM / diff ratio")
    p.add_argument("--dpi", type=int, default=96)
    p.add_argument("--min-ssim", type=float, default=0.99, help="with --against: fail below this")
    p.add_argument("--json", action="store_true")
    p.set_defaults(fn=cmd_render)

    p = sub.add_parser("textdiff", help="text views (markup/accept/reject) of one document, or diff of two")
    p.add_argument("a")
    p.add_argument("b", nargs="?")
    p.add_argument("--mode", choices=["markup", "accept", "reject"], default="markup")
    p.add_argument("--story", help="part name (default main document), or 'all'")
    p.add_argument("--facts", action="store_true", help="print revisions/comments/fields/... of A")
    p.add_argument("--json", action="store_true")
    p.set_defaults(fn=cmd_textdiff)

    p = sub.add_parser("expect", help="check fixtures against their .expect.json sidecars (text oracle)")
    p.add_argument("files", nargs="+")
    p.set_defaults(fn=cmd_expect)

    p = sub.add_parser("locality", help="did the edit change only the intended paragraphs?")
    p.add_argument("original")
    p.add_argument("edited")
    p.add_argument("-i", "--intended", action="append", default=[],
                   help="paraId, index, or part:index (repeat or comma-separate)")
    p.add_argument("--allow", action="append", default=[], help="extra zip entries allowed to change (glob)")
    p.add_argument("--strict", action="store_true", help="allow only --allow parts (drop the default list)")
    p.add_argument("--json", action="store_true")
    p.set_defaults(fn=cmd_locality)

    p = sub.add_parser("tokens", help="token count (tiktoken o200k_base) of a text file or a docx text view")
    p.add_argument("files", nargs="+")
    p.add_argument("--mode", choices=["markup", "accept", "reject"], default="markup")
    p.add_argument("--raw", action="store_true", help="count the raw document.xml instead of the text view")
    p.add_argument("--anthropic-model", help="also call Anthropic count_tokens (needs ANTHROPIC_API_KEY; "
                                              "sends the text to Anthropic, never use on client documents)")
    p.add_argument("--json", action="store_true")
    p.set_defaults(fn=cmd_tokens)

    a = ap.parse_args(argv)
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
