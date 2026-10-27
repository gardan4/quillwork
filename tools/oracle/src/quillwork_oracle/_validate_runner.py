"""Runs inside an isolated interpreter (python -I): imports docx_scalpel from a directory given on argv and calls
verify_deliverable for each file. Kept free of imports from the oracle package on purpose.

argv: <docxodus_dir> <request.json>   request: {"files": [{"path":..., "baseline": path|null}]}
stdout: one JSON object per file, one per line.
"""
import base64
import json
import sys
import time


def main():
    sys.path.insert(0, sys.argv[1])
    from docx_scalpel._transport import call  # noqa: E402

    with open(sys.argv[2], encoding="utf-8") as f:
        req = json.load(f)
    for item in req["files"]:
        t0 = time.monotonic()
        out = {"path": item["path"]}
        try:
            with open(item["path"], "rb") as f:
                args = {"docxB64": base64.b64encode(f.read()).decode("ascii")}
            if item.get("baseline"):
                with open(item["baseline"], "rb") as f:
                    args["baselineB64"] = base64.b64encode(f.read()).decode("ascii")
            out["result"] = call("verify_deliverable", args)
        except Exception as e:  # report and continue with the next file
            out["exception"] = "%s: %s" % (type(e).__name__, e)
        out["seconds"] = round(time.monotonic() - t0, 3)
        sys.stdout.write(json.dumps(out) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
