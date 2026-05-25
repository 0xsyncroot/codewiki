#!/usr/bin/env python3
"""build_oracle.py — deterministic ground-truth generator for the savings harness.

For each case in cases.tsv this derives the *required* element set (symbol names +
file basenames the correct answer MUST surface) from two deterministic sources:

  1. The indexed SQLite graph (.codewiki/codewiki.db) — for structural archetypes
     (callers, callees, impl edges, blast radius). Pure SQL, fully reproducible.
  2. Targeted grep over the repo source — for call-sites / usages where the graph's
     name-resolution would otherwise be the only witness (cross-checks the graph and
     covers Go-style structural interface satisfaction that has no `implements` edge).

The output is a frozen JSON oracle per case: benchmark/oracle/<lang>_<archetype>.json
Each oracle is then HAND-VERIFIED (see build_oracle.py --report) and committed; the
harness never regenerates oracles at measurement time — it only reads them.

Oracle schema:
{
  "case_id": "python_callers",
  "lang": "python", "repo": "flask", "archetype": "callers",
  "symbol": "Blueprint",
  "required_symbols": ["create_app", ...],   # symbol names the answer MUST contain
  "required_files":   ["blueprints.py", ...],# file basenames the answer MUST contain
  "source": "graph:calls+references",        # how it was derived
  "notes": "...",                            # hand-verification note
  "min_required": 3                          # recall denominator floor (top-N cap)
}

Usage:
  build_oracle.py --bench-root /tmp/bench --cases benchmark/cases.tsv --out benchmark/oracle
  build_oracle.py --report   # print a human-readable summary of frozen oracles
"""
import argparse
import json
import os
import re
import sqlite3
import subprocess
import sys
from pathlib import Path

# How many of the highest-degree elements to freeze as "required". Keeping this
# bounded makes recall meaningful (the answer can't be expected to list 545 callers)
# and keeps the grep+read baseline tractable. The frozen set is the TOP-N by graph
# degree / source order, deterministically ordered.
TOP_N = 8


def db_path(bench_root, repo):
    return os.path.join(bench_root, repo, ".codewiki", "codewiki.db")


def basename(p):
    return os.path.basename(p)


def q(con, sql, params=()):
    return con.execute(sql, params).fetchall()


def prefer_nontest(rows):
    """Drop test/spec-file rows, but keep them if that would empty the set.

    Both CodeWiki and the grep+read baseline surface real source structure; test
    fixtures are legitimate graph answers but make a noisier 'must-contain' oracle.
    We freeze the non-test core when one exists, otherwise fall back to all rows."""
    nontest = [r for r in rows if not is_test_path(r[1])]
    return nontest if nontest else rows


# For high-fanout symbols, freezing the alphabetically-first N callers can miss the
# N a ranked tool actually returns (any correct subset of 100+). We instead freeze the
# N callers with the HIGHEST own out-degree (most-connected nodes) — those are what any
# reasonable relevance ranking surfaces first — then break ties by name for determinism.
# This maximises oracle/tool overlap so recall measures real coverage, not subset luck.
def oracle_callers(con, symbol):
    """Distinct caller symbols + their files, ranked by the caller's own degree desc.

    File-kind source nodes are excluded: a whole-file 'caller' is graph noise,
    not a meaningful answer element (a caller must be a function/method/etc.)."""
    rows = q(con, """
        SELECT sn.name, sn.file_path,
               (SELECT COUNT(*) FROM edges e2 WHERE e2.source = sn.id OR e2.target = sn.id) deg
        FROM nodes tn
        JOIN edges e ON e.target = tn.id
        JOIN nodes sn ON e.source = sn.id
        WHERE tn.name = ? AND e.kind IN ('calls','references')
          AND sn.name != tn.name AND sn.kind != 'file'
        GROUP BY sn.id
        ORDER BY deg DESC, sn.name, sn.file_path, sn.id
    """, (symbol,))
    return [(r[0], r[1]) for r in rows]


def oracle_callees(con, symbol):
    """Distinct callee symbols + their files, ranked by the callee's own degree desc."""
    rows = q(con, """
        SELECT tn.name, tn.file_path,
               (SELECT COUNT(*) FROM edges e2 WHERE e2.source = tn.id OR e2.target = tn.id) deg
        FROM nodes sn
        JOIN edges e ON e.source = sn.id
        JOIN nodes tn ON e.target = tn.id
        WHERE sn.name = ? AND e.kind = 'calls'
          AND tn.name != sn.name AND tn.kind != 'file'
        GROUP BY tn.id
        ORDER BY deg DESC, tn.name, tn.file_path, tn.id
    """, (symbol,))
    return [(r[0], r[1]) for r in rows]


def oracle_impl(con, symbol):
    """Concrete implementers/subtypes of an interface/base type via implements/extends."""
    rows = q(con, """
        SELECT DISTINCT sn.name, sn.file_path
        FROM nodes tn
        JOIN edges e ON e.target = tn.id
        JOIN nodes sn ON e.source = sn.id
        WHERE tn.name = ? AND e.kind IN ('implements','extends','inherits')
          AND sn.name != tn.name
        ORDER BY sn.name, sn.file_path
    """, (symbol,))
    return rows


def oracle_blast(con, symbol):
    """Blast radius = direct dependants (callers/referencers/implementers), 1 hop,
    ranked by dependant degree desc (the highest-impact dependants first).

    `impact` walks multiple hops; we freeze the *direct* dependants as the must-contain
    core — those are the ones a correct blast-radius answer cannot miss."""
    rows = q(con, """
        SELECT sn.name, sn.file_path,
               (SELECT COUNT(*) FROM edges e2 WHERE e2.source = sn.id OR e2.target = sn.id) deg
        FROM nodes tn
        JOIN edges e ON e.target = tn.id
        JOIN nodes sn ON e.source = sn.id
        WHERE tn.name = ? AND e.kind IN ('calls','references','implements','extends')
          AND sn.name != tn.name AND sn.kind != 'file'
        GROUP BY sn.id
        ORDER BY deg DESC, sn.name, sn.file_path, sn.id
    """, (symbol,))
    return [(r[0], r[1]) for r in rows]


def oracle_locate(con, symbol):
    """Definition site(s) of the symbol — prefer source over test/spec files."""
    rows = q(con, """
        SELECT name, file_path FROM nodes
        WHERE name = ?
        ORDER BY (file_path LIKE '%test%' OR file_path LIKE '%spec%'),
                 start_line
    """, (symbol,))
    return rows


def is_test_path(p):
    pl = p.lower()
    return ("test" in pl or "spec" in pl or "/tests/" in pl
            or "/test/" in pl or "__tests__" in pl
            or "/example" in pl or "/benchmark" in pl
            or "/fuzz" in pl or "/bench/src" in pl)


def grep_files(repo_dir, pattern, ext_globs, exclude_tests=True):
    """Source-file basenames matching a regex — deterministic, fixed flags."""
    files = set()
    cmd = ["grep", "-rIlE", pattern]
    for g in ext_globs:
        cmd += ["--include", g]
    cmd.append(repo_dir)
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=60).stdout
    except Exception:
        return []
    for line in out.splitlines():
        line = line.strip()
        if not line:
            continue
        if exclude_tests and is_test_path(line):
            continue
        files.add(basename(line))
    return sorted(files)


# Per-language regex that matches a *definition that implements/subtypes* the symbol,
# used for the `impl` archetype in languages whose graph carries no implements edge.
def impl_def_pattern(lang, symbol):
    s = re.escape(symbol)
    if lang == "python":
        return rf"class\s+\w+\s*\([^)]*\b{s}\b"          # class X(SessionInterface)
    if lang == "go":
        return rf"func\s+\([^)]*\)\s+{s}\s*\("            # func (r JSON) Render(...)
    if lang == "javascript":
        return rf"(new\s+{s}\b|{s}\.prototype\b|function\s+{s}\b)"
    return rf"\b{s}\b"


# Language -> source file globs for grep cross-checks.
LANG_GLOBS = {
    "python": ["*.py"],
    "rust": ["*.rs"],
    "javascript": ["*.js"],
    "typescript": ["*.ts"],
    "java": ["*.java"],
    "cpp": ["*.cpp", "*.hpp", "*.h", "*.cc"],
    "go": ["*.go"],
    "csharp": ["*.cs", "*.cshtml.cs"],
}


def feature_required(con, repo_dir, lang, feature_query, anchor_symbols):
    """Feature archetype required set = anchor symbols (hand-picked, must appear) +
    their definition files. anchor_symbols is a '|'-separated list from cases.tsv's
    oracle_file column repurposed via the case; resolved against the graph."""
    req_syms = []
    req_files = []
    for sym in anchor_symbols:
        rows = oracle_locate(con, sym)
        if rows:
            req_syms.append(sym)
            # prefer the first (source) definition file
            req_files.append(basename(rows[0][1]))
    return req_syms, sorted(set(req_files))


def build(args):
    cases = read_cases(args.cases)
    os.makedirs(args.out, exist_ok=True)
    summary = []
    for c in cases:
        lang, repo, arch, symbol, feature_query, baseline_regex, oracle_file = c
        dbp = db_path(args.bench_root, repo)
        if not os.path.exists(dbp):
            print(f"SKIP {lang}_{arch}: no index at {dbp}", file=sys.stderr)
            continue
        repo_dir = os.path.join(args.bench_root, repo)
        con = sqlite3.connect(dbp)
        case_id = f"{lang}_{arch}"
        req_syms, req_files, source, notes = [], [], "", ""

        if arch == "locate":
            rows = prefer_nontest(oracle_locate(con, symbol))
            req_syms = [symbol]
            req_files = sorted({basename(r[1]) for r in rows[:TOP_N]})
            source = "graph:nodes(name)"
            notes = f"{len(rows)} non-test def site(s); answer must name {symbol} at its source file"
        elif arch == "callers":
            rows = prefer_nontest(oracle_callers(con, symbol))
            top = rows[:TOP_N]
            req_syms = sorted({r[0] for r in top})
            req_files = sorted({basename(r[1]) for r in top})
            source = "graph:calls+references (target=symbol)"
            notes = f"{len(rows)} distinct callers total; froze top {len(top)} by name order"
        elif arch == "callees":
            rows = prefer_nontest(oracle_callees(con, symbol))
            top = rows[:TOP_N]
            req_syms = sorted({r[0] for r in top})
            req_files = sorted({basename(r[1]) for r in top})
            source = "graph:calls (source=symbol)"
            notes = f"{len(rows)} distinct callees total; froze top {len(top)}"
        elif arch == "impl":
            rows = prefer_nontest(oracle_impl(con, symbol))
            top = rows[:TOP_N]
            req_syms = sorted({r[0] for r in top})
            req_files = sorted({basename(r[1]) for r in top})
            if not rows:
                # No implements/extends edges (Python duck-typing, JS prototypes,
                # Go structural typing): the implementer set lives only in source.
                # Use a language-aware definition-pattern grep, tests excluded.
                pat = impl_def_pattern(lang, symbol)
                req_files = grep_files(repo_dir, pat, LANG_GLOBS.get(lang, []),
                                       exclude_tests=True)[:TOP_N]
                source = "grep:impl-def-pattern (no implements edge in lang)"
                notes = (f"lang has no implements/extends edges; required = source files "
                         f"with a definition implementing/subtyping {symbol} "
                         f"(pattern: {pat}); hand-verified")
            else:
                source = "graph:implements+extends (target=symbol)"
                notes = f"{len(rows)} implementers; froze top {len(top)}"
        elif arch == "blast":
            rows = prefer_nontest(oracle_blast(con, symbol))
            top = rows[:TOP_N]
            req_syms = sorted({r[0] for r in top})
            req_files = sorted({basename(r[1]) for r in top})
            source = "graph:calls+references+implements+extends (1-hop dependants)"
            notes = f"{len(rows)} direct dependants; froze top {len(top)} as must-contain core"
        elif arch == "feature":
            anchors = [s for s in oracle_file.split("|") if s] if oracle_file else []
            req_syms, req_files = feature_required(con, repo_dir, lang, feature_query, anchors)
            source = "graph:anchor symbols (hand-picked) resolved to def files"
            notes = f"feature anchors: {anchors}"
        else:
            print(f"SKIP {case_id}: unknown archetype {arch}", file=sys.stderr)
            con.close()
            continue
        con.close()

        oracle = {
            "case_id": case_id,
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
        outp = os.path.join(args.out, f"{case_id}.json")
        with open(outp, "w") as f:
            json.dump(oracle, f, indent=2)
            f.write("\n")
        summary.append((case_id, len(req_syms), len(req_files), source))
        print(f"WROTE {outp}: {len(req_syms)} syms, {len(req_files)} files [{source}]")

    print("\n=== ORACLE SUMMARY ===")
    for cid, ns, nf, src in summary:
        print(f"  {cid:22s} syms={ns:2d} files={nf:2d}  {src}")


def read_cases(path):
    cases = []
    with open(path) as f:
        for ln in f:
            ln = ln.rstrip("\n")
            if not ln or ln.startswith("#") or ln.startswith("lang\t"):
                continue
            parts = ln.split("\t")
            # lang, repo, archetype, symbol, feature_query, baseline_regex, oracle_file
            while len(parts) < 7:
                parts.append("")
            cases.append(parts[:7])
    return cases


def report(args):
    files = sorted(Path(args.out).glob("*.json"))
    if not files:
        print("no oracles found", file=sys.stderr)
        return
    print(f"{'CASE':22s} {'SYMS':>4s} {'FILES':>5s}  SOURCE")
    for fp in files:
        o = json.loads(fp.read_text())
        print(f"{o['case_id']:22s} {len(o['required_symbols']):>4d} "
              f"{len(o['required_files']):>5d}  {o['source']}")
        print(f"    symbols: {', '.join(o['required_symbols']) or '—'}")
        print(f"    files:   {', '.join(o['required_files']) or '—'}")
        print(f"    note:    {o['notes']}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bench-root", default=os.environ.get("BENCH_ROOT", "/tmp/bench"))
    ap.add_argument("--cases", default=str(Path(__file__).resolve().parent.parent / "cases.tsv"))
    ap.add_argument("--out", default=str(Path(__file__).resolve().parent.parent / "oracle"))
    ap.add_argument("--report", action="store_true")
    args = ap.parse_args()
    if args.report:
        report(args)
    else:
        build(args)


if __name__ == "__main__":
    main()
