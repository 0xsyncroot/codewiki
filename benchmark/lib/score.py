#!/usr/bin/env python3
"""score.py — deterministic recall/precision/verdict scorer for the savings harness.

NO LLM judge. Given a tool's raw text output and a frozen oracle, computes:

  recall    = (# required elements PRESENT in output) / (# required elements)
              An element is "present" if its name appears as a whole-word/substring
              token in the output. Symbol names and file basenames are scored together
              (the required set is required_symbols ∪ required_files).

  precision_proxy
            = required_present / max(1, distinct_path_lines_in_output)
              A coarse "signal vs. noise" proxy: how much of the output's structural
              footprint is on-oracle. Not a true precision (we don't have a full
              relevant-set), hence "proxy". Lower for verbose dumps, higher for
              focused answers.

  wrong_primary (correctness gate)
            = True if the output names a DIFFERENT same-name symbol as its primary
              result than the oracle's expected definition file (locate archetype),
              i.e. it confidently pointed at the wrong same-name symbol. This is the
              hard correctness gate: a wrong-primary answer is FAIL regardless of recall.

  verdict   = PASS    if recall >= PASS_THRESHOLD and not wrong_primary
              PARTIAL if 0 < recall < PASS_THRESHOLD and not wrong_primary
              FAIL    if recall == 0 or wrong_primary

Usage (as a library):
  from score import score_output
  r = score_output(text, oracle_dict)          # -> dict with recall/precision/verdict/...

CLI:
  score.py --oracle oracle/python_locate.json --output captured.txt
  score.py --oracle ... --output - < captured.txt        # read output from stdin
"""
import argparse
import json
import os
import re
import sys

PASS_THRESHOLD = 0.8


def _norm(s):
    return s.lower()


def _present(needle, haystack_lower):
    """Whole-token-ish substring match: the needle appears bounded by non-identifier
    chars (so `parse` doesn't spuriously match `parseInt`, but `Flask` matches
    `class Flask`). Falls back to plain substring for names containing punctuation
    (file basenames like `app.py`, route names like `GET /x`)."""
    n = needle.lower()
    if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", needle):
        return re.search(r"(?<![A-Za-z0-9_])" + re.escape(n) + r"(?![A-Za-z0-9_])",
                         haystack_lower) is not None
    return n in haystack_lower


# A path line: any line that contains a file path (has a '/' followed by a name, or
# ends in a known source extension). Used for the precision proxy denominator.
_PATH_RE = re.compile(r"[\w./-]+\.(py|rs|js|ts|java|cpp|hpp|h|cc|go|cs|cshtml|rb|php|swift|c)\b")


def _path_lines(text):
    seen = set()
    for line in text.splitlines():
        for m in _PATH_RE.finditer(line):
            seen.add(m.group(0))
    return seen


def detect_wrong_primary(text, oracle):
    """locate-only correctness gate. The oracle's first required_file is the expected
    definition file. If the output's FIRST path line names the symbol in a file whose
    basename is NOT any oracle file, the tool confidently pointed at a wrong same-name
    symbol -> wrong_primary."""
    if oracle.get("archetype") != "locate":
        return False
    req_files = [f.lower() for f in oracle.get("required_files", [])]
    if not req_files:
        return False
    sym = oracle.get("symbol", "")
    text_lower = text.lower()
    # First path line that mentions the symbol — that is the tool's primary pick.
    for line in text.splitlines():
        ll = line.lower()
        if sym and not _present(sym, ll):
            continue
        m = _PATH_RE.search(ll)
        if not m:
            continue
        base = os.path.basename(m.group(0))
        # Primary pick found. PASS the gate only if its file is in the oracle set.
        return base not in req_files
    # Symbol never appears next to a path at all -> not a confident wrong pick;
    # recall will catch the miss. Don't fail the correctness gate here.
    return False


def score_output(text, oracle):
    text_lower = _norm(text)
    required = list(dict.fromkeys(
        list(oracle.get("required_symbols", [])) + list(oracle.get("required_files", []))
    ))
    if not required:
        # Defensive: an empty oracle can't be scored meaningfully.
        return {
            "recall": 0.0, "precision_proxy": 0.0, "verdict": "UNSCORABLE",
            "required_total": 0, "required_present": 0, "missing": [],
            "wrong_primary": False, "path_lines": len(_path_lines(text)),
        }

    present, missing = [], []
    for el in required:
        if _present(el, text_lower):
            present.append(el)
        else:
            missing.append(el)

    recall = len(present) / len(required)
    npaths = len(_path_lines(text))
    precision_proxy = len(present) / max(1, npaths)
    if precision_proxy > 1.0:
        precision_proxy = 1.0
    wrong_primary = detect_wrong_primary(text, oracle)

    if recall == 0 or wrong_primary:
        verdict = "FAIL"
    elif recall >= PASS_THRESHOLD:
        verdict = "PASS"
    else:
        verdict = "PARTIAL"

    return {
        "recall": round(recall, 4),
        "precision_proxy": round(precision_proxy, 4),
        "verdict": verdict,
        "required_total": len(required),
        "required_present": len(present),
        "missing": missing,
        "wrong_primary": wrong_primary,
        "path_lines": npaths,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--oracle", required=True)
    ap.add_argument("--output", required=True, help="file with tool output, or - for stdin")
    ap.add_argument("--json", action="store_true", help="emit JSON")
    args = ap.parse_args()

    oracle = json.load(open(args.oracle))
    if args.output == "-":
        text = sys.stdin.read()
    else:
        text = open(args.output, encoding="utf-8", errors="replace").read()

    r = score_output(text, oracle)
    if args.json:
        print(json.dumps(r))
    else:
        print(f"{oracle['case_id']}: {r['verdict']}  recall={r['recall']} "
              f"({r['required_present']}/{r['required_total']})  "
              f"precision_proxy={r['precision_proxy']}  "
              f"wrong_primary={r['wrong_primary']}")
        if r["missing"]:
            print(f"  missing: {', '.join(r['missing'])}")


if __name__ == "__main__":
    main()
