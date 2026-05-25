#!/usr/bin/env python3
"""run.py — context-relevance fixture runner (replaces check-context-quality.sh).

Drives `codewiki_context` over the MCP stdio path for each case, parses the
"Entry Points" (roots) and "Related Symbols" (+ Entry Points) sections, and computes:

  roots@k precision = (# expected_roots found among the top-k surfaced roots)
                      / min(k, # surfaced roots)
        How clean the top-k roots are: penalises off-topic roots (e.g. the BEFORE
        binary surfacing `NetworkSession` as a root for every query).

  roots@k recall    = (# expected_roots found among the top-k surfaced roots)
                      / (# expected_roots)
        Did the right roots make it into the top-k at all.

  nodes recall      = (# expected_anywhere found anywhere in the output)
                      / (# expected_anywhere)
        Coverage of the relevant symbol set across Entry Points + Related Symbols.

Aggregates mean precision / roots-recall / nodes-recall per corpus and checks the
GATING corpus (synthetic-120) against parity/thresholds.toml [context] floors.

Matching is by symbol NAME (file-kind 'roots' like `graph.rs` are ignored — a file is
not a root symbol). Synthetic-120 names are domain-unique so name matching is exact.

Usage:
  CW=/tmp/codewiki-before-opt parity/context-relevance/run.py            # human report
  CW=/tmp/codewiki-before-opt parity/context-relevance/run.py --json     # machine output
  CW=... parity/context-relevance/run.py --record before.json            # snapshot results
The exit code is non-zero iff a GATING corpus violates a [context] threshold floor.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
MCP = str(REPO / "benchmark" / "lib" / "mcp_call.py")


def mcp_context(binary, project_path, task, max_nodes=12):
    out = subprocess.run(
        ["python3", MCP, "--bin", binary, "--path", project_path,
         "--tool", "codewiki_context",
         "--args", json.dumps({"task": task, "maxNodes": max_nodes})],
        capture_output=True, text=True, timeout=90,
    )
    return out.stdout


_SYM_RE = re.compile(r"\*\*(.+?)\*\*\s+\((\w[\w ]*)\)")


def parse_sections(text):
    """Return (roots, anywhere): roots = ordered non-file symbol names in Entry Points;
    anywhere = set of all non-file symbol names in Entry Points + Related Symbols."""
    roots, anywhere = [], set()
    section = None
    for line in text.splitlines():
        s = line.strip()
        if s.startswith("### Entry Points"):
            section = "roots"; continue
        if s.startswith("### Related Symbols"):
            section = "related"; continue
        if s.startswith("### Key Code") or s.startswith("## "):
            section = None; continue
        m = _SYM_RE.search(line)
        if not m:
            continue
        name, kind = m.group(1).strip(), m.group(2).strip()
        if kind == "file":
            continue
        if section == "roots":
            if name not in roots:
                roots.append(name)
            anywhere.add(name)
        elif section == "related":
            anywhere.add(name)
    return roots, anywhere


def score_case(roots, anywhere, case, k):
    exp_roots = case["expected_roots"]
    exp_any = case["expected_anywhere"]
    topk = roots[:k]
    root_hits = [r for r in exp_roots if r in topk]
    denom_p = min(k, len(topk)) if topk else 0
    precision = (len(root_hits) / denom_p) if denom_p else 0.0
    roots_recall = (len(root_hits) / len(exp_roots)) if exp_roots else 1.0
    any_hits = [a for a in exp_any if a in anywhere]
    nodes_recall = (len(any_hits) / len(exp_any)) if exp_any else 1.0
    return {
        "id": case["id"],
        "precision": round(precision, 4),
        "roots_recall": round(roots_recall, 4),
        "nodes_recall": round(nodes_recall, 4),
        "topk_roots": topk,
        "expected_roots": exp_roots,
        "missing_roots": [r for r in exp_roots if r not in topk],
        "missing_nodes": [a for a in exp_any if a not in anywhere],
    }


def load_thresholds():
    tp = REPO / "parity" / "thresholds.toml"
    if not tp.exists():
        return {}
    with open(tp, "rb") as f:
        return tomllib.load(f).get("context", {})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.environ.get("CW", "codewiki"))
    ap.add_argument("--cases", default=str(HERE / "cases.json"))
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--record", help="write full results JSON to this path")
    args = ap.parse_args()

    spec = json.load(open(args.cases))
    k = spec.get("k", 5)
    thr = load_thresholds()
    results = {"k": k, "binary": args.bin, "corpora": {}}
    gate_failures = []

    for cname, corpus in spec["corpora"].items():
        if cname.startswith("_"):
            continue
        cpath = str((REPO / corpus["path"]).resolve())
        gating = corpus.get("gating", False)
        cases_out = []
        if not (Path(cpath) / ".codewiki" / "codewiki.db").exists():
            results["corpora"][cname] = {"skipped": f"no index at {cpath}"}
            continue
        for case in corpus["cases"]:
            text = mcp_context(args.bin, cpath, case["task"])
            roots, anywhere = parse_sections(text)
            cases_out.append(score_case(roots, anywhere, case, k))
        n = len(cases_out)
        agg = {
            "n": n,
            "gating": gating,
            "mean_precision": round(sum(c["precision"] for c in cases_out) / n, 4) if n else 0,
            "mean_roots_recall": round(sum(c["roots_recall"] for c in cases_out) / n, 4) if n else 0,
            "mean_nodes_recall": round(sum(c["nodes_recall"] for c in cases_out) / n, 4) if n else 0,
            "cases": cases_out,
        }
        results["corpora"][cname] = agg

        if gating and thr:
            if "roots_precision_min" in thr and agg["mean_precision"] < thr["roots_precision_min"]:
                gate_failures.append(
                    f"{cname}: mean roots@{k} precision {agg['mean_precision']} "
                    f"< floor {thr['roots_precision_min']}")
            if "roots_recall_min" in thr and agg["mean_roots_recall"] < thr["roots_recall_min"]:
                gate_failures.append(
                    f"{cname}: mean roots@{k} recall {agg['mean_roots_recall']} "
                    f"< floor {thr['roots_recall_min']}")
            if "nodes_recall_min" in thr and agg["mean_nodes_recall"] < thr["nodes_recall_min"]:
                gate_failures.append(
                    f"{cname}: mean nodes recall {agg['mean_nodes_recall']} "
                    f"< floor {thr['nodes_recall_min']}")

    results["gate_failures"] = gate_failures

    if args.record:
        Path(args.record).write_text(json.dumps(results, indent=2) + "\n")

    if args.json:
        print(json.dumps(results, indent=2))
    else:
        print(f"=== Context-relevance fixture (roots@{k}) — binary: {args.bin} ===\n")
        for cname, agg in results["corpora"].items():
            if "skipped" in agg:
                print(f"[{cname}] SKIPPED — {agg['skipped']}\n")
                continue
            tag = "GATING" if agg["gating"] else "advisory"
            print(f"[{cname}] ({tag}, n={agg['n']})  "
                  f"roots@{k} precision={agg['mean_precision']}  "
                  f"roots@{k} recall={agg['mean_roots_recall']}  "
                  f"nodes recall={agg['mean_nodes_recall']}")
            for c in agg["cases"]:
                flag = "" if (c["precision"] >= 0.5 and c["nodes_recall"] >= 0.6) else "  <-- weak"
                print(f"    {c['id']:<26} P={c['precision']:.2f} "
                      f"rootsR={c['roots_recall']:.2f} nodesR={c['nodes_recall']:.2f}{flag}")
                if c["missing_roots"]:
                    print(f"        missing roots: {', '.join(c['missing_roots'])}")
            print()
        if thr:
            print(f"[context] thresholds: {thr}")
        if gate_failures:
            print("\nGATE: FAIL")
            for f in gate_failures:
                print(f"  - {f}")
        elif thr:
            print("\nGATE: PASS")

    sys.exit(1 if gate_failures else 0)


if __name__ == "__main__":
    main()
