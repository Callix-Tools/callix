"""Checks an LSIF dump of a real project against the sources it describes.

    python3 check_lsif.py DUMP MIN_RANGES URI_PREFIX HOST_PREFIX

Every range must lie inside the document it is filed under. A reference placed
by the wrong file — what happened when several definitions shared one qualified
name — lands beyond the end of the file or on text that is not there, which a
structural validator cannot see.

URI_PREFIX is how the dump spells the project root (``file:///workspace/gin``
when it was written inside a container) and HOST_PREFIX is where the same
directory is on this machine.
"""

import json
import sys
import urllib.parse


def main() -> int:
    dump, minimum, uri_prefix, host_prefix = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
    elements = [json.loads(line) for line in open(dump, encoding="utf-8")]

    documents = {e["id"]: e for e in elements if e.get("label") == "document"}
    document_of = {}
    for e in elements:
        if e.get("label") == "contains" and e["outV"] in documents:
            for range_id in e["inVs"]:
                document_of[range_id] = e["outV"]

    sources: dict[str, list[str]] = {}
    ranges = beyond = empty = 0
    for e in elements:
        if e.get("label") != "range":
            continue
        document = document_of.get(e["id"])
        if document is None:
            continue
        uri = documents[document]["uri"]
        if not uri.startswith(uri_prefix):
            print(f"check_lsif: {uri} is outside {uri_prefix}", file=sys.stderr)
            return 1
        path = host_prefix + urllib.parse.unquote(uri[len(uri_prefix):])
        if path not in sources:
            with open(path, encoding="utf-8", errors="replace") as f:
                sources[path] = f.read().split("\n")
        lines = sources[path]

        ranges += 1
        start, end = e["start"], e["end"]
        if start["line"] >= len(lines):
            beyond += 1
        elif start == end:
            empty += 1
        elif start["line"] == end["line"]:
            text = lines[start["line"]].encode("utf-16-le")[start["character"] * 2 : end["character"] * 2]
            if not text:
                empty += 1

    print(f"check_lsif: {ranges} ranges in {len(sources)} files; beyond end of file: {beyond}; empty: {empty}")
    if ranges < minimum:
        print(f"check_lsif: expected at least {minimum} ranges", file=sys.stderr)
        return 1
    if beyond or empty:
        print("check_lsif: ranges that do not lie inside their document", file=sys.stderr)
        return 1
    return 0


sys.exit(main())
