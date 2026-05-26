#!/usr/bin/env python3
"""build_oracle_extra.py — the canonical deterministic oracle generator.

Reads the canonical 8-column benchmark/cases.tsv whose final column is a globally-unique
`case_id`, and writes one frozen oracle per case as oracle/<lang>_<case_id>.json. Because
every case (including the base 6-archetype suite) now carries a case_id, this single
builder covers ALL 157 cases — there is no separate base/extra split.

It reuses the derivation logic in build_oracle.py verbatim (same SQL, same TOP_N, same
prefer_nontest, same impl-def grep fallback) by importing that module, so the two stay in
lock-step; build_oracle.py is kept only as that shared library (it is not run directly).

Oracle schema (per case):
  {"case_id":"python_base_loc_flask","lang":"python","repo":"flask","archetype":"locate",
   "symbol":"Flask","required_symbols":[...],"required_files":[...],"source":"...",
   "notes":"...","min_required":N}

Usage:
  build_oracle_extra.py --bench-root /tmp/bench --cases benchmark/cases.tsv --out benchmark/oracle
  build_oracle_extra.py --report --cases benchmark/cases.tsv --out benchmark/oracle
"""
import argparse
import json
import os
import sqlite3
import sys
from pathlib import Path

# Reuse the frozen derivation logic verbatim — same SQL, same TOP_N, same prefer_nontest.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_oracle as bo  # noqa: E402


def read_extra_cases(path):
    """Like bo.read_cases but keeps an 8th case_id column."""
    cases = []
    with open(path) as f:
        for ln in f:
            ln = ln.rstrip("\n")
            if not ln or ln.startswith("#") or ln.startswith("lang\t"):
                continue
            parts = ln.split("\t")
            while len(parts) < 8:
                parts.append("")
            # lang, repo, archetype, symbol, feature_query, baseline_regex, anchors, case_id
            cases.append(parts[:8])
    return cases


def build(args):
    cases = read_extra_cases(args.cases)
    os.makedirs(args.out, exist_ok=True)
    summary = []
    for c in cases:
        lang, repo, arch, symbol, feature_query, baseline_regex, oracle_file, case_id = c
        if not case_id:
            print(f"SKIP (no case_id): {lang} {arch} {symbol}", file=sys.stderr)
            continue
        dbp = bo.db_path(args.bench_root, repo)
        if not os.path.exists(dbp):
            print(f"SKIP {lang}_{case_id}: no index at {dbp}", file=sys.stderr)
            continue
        repo_dir = os.path.join(args.bench_root, repo)
        con = sqlite3.connect(dbp)
        oracle_case_id = f"{lang}_{case_id}"
        req_syms, req_files, source, notes = [], [], "", ""

        if arch == "locate":
            rows = bo.prefer_nontest(bo.oracle_locate(con, symbol))
            req_syms = [symbol]
            req_files = sorted({bo.basename(r[1]) for r in rows[:bo.TOP_N]})
            source = "graph:nodes(name)"
            notes = f"{len(rows)} non-test def site(s); answer must name {symbol} at its source file"
        elif arch == "callers":
            rows = bo.prefer_nontest(bo.oracle_callers(con, symbol))
            top = rows[:bo.TOP_N]
            req_syms = sorted({r[0] for r in top})
            req_files = sorted({bo.basename(r[1]) for r in top})
            source = "graph:calls+references (target=symbol)"
            notes = f"{len(rows)} distinct callers total; froze top {len(top)} by degree/name order"
        elif arch == "callees":
            rows = bo.prefer_nontest(bo.oracle_callees(con, symbol))
            top = rows[:bo.TOP_N]
            req_syms = sorted({r[0] for r in top})
            req_files = sorted({bo.basename(r[1]) for r in top})
            source = "graph:calls (source=symbol)"
            notes = f"{len(rows)} distinct callees total; froze top {len(top)}"
        elif arch == "impl":
            rows = bo.prefer_nontest(bo.oracle_impl(con, symbol))
            top = rows[:bo.TOP_N]
            req_syms = sorted({r[0] for r in top})
            req_files = sorted({bo.basename(r[1]) for r in top})
            if not rows:
                pat = bo.impl_def_pattern(lang, symbol)
                req_files = bo.grep_files(repo_dir, pat, bo.LANG_GLOBS.get(lang, []),
                                          exclude_tests=True)[:bo.TOP_N]
                source = "grep:impl-def-pattern (no implements edge in lang)"
                notes = (f"lang has no implements/extends edges; required = source files "
                         f"with a definition implementing/subtyping {symbol} "
                         f"(pattern: {pat}); hand-verified")
            else:
                source = "graph:implements+extends (target=symbol)"
                notes = f"{len(rows)} implementers; froze top {len(top)}"
        elif arch == "blast":
            rows = bo.prefer_nontest(bo.oracle_blast(con, symbol))
            top = rows[:bo.TOP_N]
            req_syms = sorted({r[0] for r in top})
            req_files = sorted({bo.basename(r[1]) for r in top})
            source = "graph:calls+references+implements+extends (1-hop dependants)"
            notes = f"{len(rows)} direct dependants; froze top {len(top)} as must-contain core"
        elif arch == "feature":
            anchors = [s for s in oracle_file.split("|") if s] if oracle_file else []
            req_syms, req_files = bo.feature_required(con, repo_dir, lang, feature_query, anchors)
            source = "graph:anchor symbols (hand-picked) resolved to def files"
            notes = f"feature anchors: {anchors}"
        else:
            print(f"SKIP {oracle_case_id}: unknown archetype {arch}", file=sys.stderr)
            con.close()
            continue
        con.close()

        oracle = {
            "case_id": oracle_case_id,
            "lang": lang,
            "repo": repo,
            "archetype": arch,
            "symbol": symbol,
            "feature_query": feature_query,
            "required_symbols": req_syms,
            "required_files": req_files,
            "source": source,
            "notes": notes,
            "min_required": max(1, len(req_syms) + len(req_files)),
        }
        outp = os.path.join(args.out, f"{oracle_case_id}.json")
        with open(outp, "w") as f:
            json.dump(oracle, f, indent=2)
            f.write("\n")
        summary.append((oracle_case_id, len(req_syms), len(req_files), source))
        print(f"WROTE {outp}: {len(req_syms)} syms, {len(req_files)} files [{source}]")

    print("\n=== EXTRA ORACLE SUMMARY ===")
    for cid, ns, nf, src in summary:
        print(f"  {cid:34s} syms={ns:2d} files={nf:2d}  {src}")


def report(args):
    cases = read_extra_cases(args.cases)
    ids = [f"{c[0]}_{c[7]}" for c in cases if c[7]]
    print(f"{'CASE':34s} {'SYMS':>4s} {'FILES':>5s}  SOURCE")
    for cid in ids:
        fp = Path(args.out) / f"{cid}.json"
        if not fp.exists():
            print(f"{cid:34s}  MISSING")
            continue
        o = json.loads(fp.read_text())
        print(f"{o['case_id']:34s} {len(o['required_symbols']):>4d} "
              f"{len(o['required_files']):>5d}  {o['source']}")
        print(f"    symbols: {', '.join(o['required_symbols']) or '-'}")
        print(f"    files:   {', '.join(o['required_files']) or '-'}")
        print(f"    note:    {o['notes']}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bench-root", default=os.environ.get("BENCH_ROOT", "/tmp/bench"))
    ap.add_argument("--cases",
                    default=str(Path(__file__).resolve().parent.parent / "cases.tsv"))
    ap.add_argument("--out", default=str(Path(__file__).resolve().parent.parent / "oracle"))
    ap.add_argument("--report", action="store_true")
    args = ap.parse_args()
    if args.report:
        report(args)
    else:
        build(args)


if __name__ == "__main__":
    main()
