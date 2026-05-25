# CodeWiki — QC Test Plan (End-to-End, Binary-Level)

## 1. Purpose

This plan proves, **end-to-end against the real `codewiki` binary**, that:

- The CLI and all subcommands launch and expose help (`--help`, `--version`).
- `init` actually indexes real source code and produces **correct nodes and edges** (not just "unit tests pass").
- Indexing is **correct per language** — the right `NodeKind` and `EdgeKind` values appear for known, minimal inputs.
- Edge kinds (`calls`, `imports`, `contains`, `extends`, `implements`) resolve correctly.
- Framework resolvers (routes, components, DI) produce the expected nodes/edges.
- The **9 MCP tools** return correct, non-empty, correctly-shaped data.
- **Incremental sync** is correct on add / modify / rename / delete, including the mtime-preserved-but-content-changed case.
- The system is robust on edge cases (empty repo, binaries, unicode, real cloned repos).
- The graph UI + MCP server serve and behave gracefully.

> The unit-test green bar is **not** evidence of correct indexing. Every case below inspects the **actual SQLite graph** (`.codewiki/codewiki.db`) and/or **actual command output**. Verdicts are derived from real data, never from "tests passed".

## 2. Binary / Version Under Test

| Item | Value |
|---|---|
| Binary path | `/root/develop/code-wiki/target/release/codewiki` |
| Reported version | `codewiki 0.1.0` (verify in case **A1**) |
| DB path created by `init` | `<project>/.codewiki/codewiki.db` |
| DB schema (key columns) | `nodes(id, kind, name, qualified_name, file_path, language, start_line, end_line, start_column, end_column, ...)`; `edges(id, source, target, kind, metadata, line, col, provenance)` |
| Edge join columns | `edges.source` / `edges.target` → `nodes.id` (NOT `source_id`/`target_id`) |
| Reference type defs | `crates/codewiki-core/src/types.rs` |

**`NodeKind` (actual variants, snake_case in DB):** `function, method, class, interface, enum, enum_member, struct, trait, type, variable, constant, module, namespace, property, field, constructor, getter, setter, file, component, route, unknown`

**`EdgeKind` (actual variants, snake_case in DB):** `calls, imports, contains, extends, implements, uses, references, instantiates, exports, renders, resolves, unknown`

## 3. How To Run (copy-paste prerequisites)

Each executor sets these once per shell. **Every case is self-contained** and creates its own temp workspace.

```bash
# Pin the binary and a private workspace root for this QC agent.
export CW=/root/develop/code-wiki/target/release/codewiki
export QC_ROOT="/tmp/qc-$(id -u)-$$"      # unique per agent/shell; avoids cross-agent collisions
mkdir -p "$QC_ROOT"
# sqlite3 is required for graph inspection. Verify:
command -v sqlite3 && command -v curl
"$CW" --version
```

**DB inspection helpers** (used throughout; `$DB` is set per case):

```bash
# All nodes by kind+name:
sqlite3 "$DB" "SELECT kind,name FROM nodes ORDER BY kind,name;"
# All edges with endpoint names + provenance:
sqlite3 "$DB" "SELECT e.kind, s.name, t.name, COALESCE(e.provenance,'') \
  FROM edges e JOIN nodes s ON e.source=s.id JOIN nodes t ON e.target=t.id \
  ORDER BY e.kind, s.name, t.name;"
# Count of a specific edge kind:
sqlite3 "$DB" "SELECT count(*) FROM edges WHERE kind='implements';"
```

> **IMPORTANT — exit codes through pipes.** When a command is piped (e.g. `| tail`), `$?` reflects the *last* pipeline stage. To assert a command's own exit status, run it **without a pipe** and check `echo $?` on its own line, or use `set -o pipefail`.

## 4. Legend

| Mark | Meaning |
|---|---|
| **PASS** | Observed output/data matches the Expected column exactly (or within the stated threshold). |
| **FAIL** | Observed result contradicts Expected (missing node/edge, wrong kind, panic, non-zero exit where success expected, etc.). |
| **BLOCKED** | Could not run (missing dependency, prior setup failed, environment issue). Record why. |

> Executors fill **Result** with one of the three marks **plus pasted evidence** (the actual command output / SQL rows) in **Notes**. Leave Result/Notes blank in this template.

---

## A. Install / CLI Smoke

Setup for this whole section: none (binary only).

| ID | Area | Setup | Steps (exact commands) | Expected result | Result | Notes |
|---|---|---|---|---|---|---|
| A1 | version | — | `"$CW" --version` | Prints `codewiki 0.1.0`; exit 0 | | |
| A2 | top help | — | `"$CW" --help` | Lists subcommands incl. `setup, status, doctor, query, context, init, index, sync, serve, files, callers, callees, impact, affected, install, uninstall, uninit, graph, snapshot, restore`; exit 0 | | |
| A3 | per-cmd help | — | `for c in setup status doctor query context init index sync serve files callers callees impact affected install uninstall uninit graph snapshot restore; do echo "== $c =="; "$CW" $c --help >/dev/null 2>&1 && echo OK $c || echo FAIL $c; done` | Every line prints `OK <cmd>`; no `FAIL` | | |
| A4 | init help flags | — | `"$CW" init --help` | Shows `--no-index`, `--path <PATH>`, `-i/--interactive` | | |
| A5 | unknown cmd | — | `"$CW" definitely-not-a-cmd; echo "exit=$?"` | Non-zero exit; error message naming the bad subcommand; no panic/backtrace | | |
| A6 | status (no index) | `mkdir -p "$QC_ROOT/a6" && cd "$QC_ROOT/a6"` | `"$CW" status; echo "exit=$?"` | Graceful error: `no index found ... run codewiki init first`; non-zero exit; no panic | | |
| A7 | doctor (no index) | `mkdir -p "$QC_ROOT/a7" && cd "$QC_ROOT/a7"` | `"$CW" doctor` | Prints diagnostics table; `binary on PATH` ✓; `index initialized` ✗ with hint `Run: codewiki setup`; no panic | | |
| A8 | doctor --strict | `cd "$QC_ROOT/a7"` (unindexed) | `"$CW" doctor --strict; echo "exit=$?"` | Non-zero exit (a check failed under `--strict`) | | |
| A9 | query (no index) | `cd "$QC_ROOT/a6"` | `"$CW" query foo; echo "exit=$?"` | Graceful "no index" error; non-zero exit; no panic | | |

---

## B. `init` End-to-End

Each case uses a fresh temp dir.

| ID | Area | Setup | Steps (exact commands) | Expected result | Result | Notes |
|---|---|---|---|---|---|---|
| B1 | init creates DB | `D="$QC_ROOT/b1"; mkdir -p "$D" && cd "$D"; printf 'def alpha():\n    return 1\n' > a.py` | `"$CW" init; ls -l .codewiki/codewiki.db; echo "exit=$?"` | Prints `Indexed N files, M nodes, K edges` with **N≥1, M≥2, K≥1** (non-zero counts); `.codewiki/codewiki.db` exists; exit 0 | | |
| B2 | indexed counts real | `cd "$QC_ROOT/b1"` (after B1) | `DB=.codewiki/codewiki.db; sqlite3 "$DB" "SELECT count(*) FROM nodes; SELECT count(*) FROM files;"` | nodes count ≥ 2 (`file` + `function alpha`); files count = 1 | | |
| B3 | git hooks installed | `D="$QC_ROOT/b3"; mkdir -p "$D" && cd "$D"; git init -q; printf 'def a():\n    return 1\n' > a.py` | `"$CW" init >/dev/null 2>&1; ls -1 .git/hooks/ \| grep -E 'post-commit\|post-merge\|post-checkout'` | At least one CodeWiki git hook present in `.git/hooks/` (e.g. `post-commit`) | | |
| B4 | no .git → no hook (graceful) | `D="$QC_ROOT/b4"; mkdir -p "$D" && cd "$D"; printf 'def a():\n    return 1\n' > a.py` | `"$CW" init >/dev/null 2>&1; test -d .git && echo "has-git" \|\| echo "no-git"; "$CW" doctor \| grep -i 'git hook'` | `no-git`; doctor shows `git hook` ✓ with `not a git repo — hook not required`; no error | | |
| B5 | re-run init is sane | `cd "$QC_ROOT/b1"` (already inited) | `"$CW" init; echo "exit=$?"; sqlite3 .codewiki/codewiki.db "SELECT count(*) FROM nodes WHERE name='alpha';"` | Exit 0; no crash; exactly **1** `alpha` node (no duplication from re-init) | | |
| B6 | `--no-index` scaffolds only | `D="$QC_ROOT/b6"; mkdir -p "$D" && cd "$D"; printf 'def a():\n    return 1\n' > a.py` | `"$CW" init --no-index; echo "exit=$?"; DB=.codewiki/codewiki.db; test -f "$DB" && sqlite3 "$DB" "SELECT count(*) FROM nodes WHERE kind!='file';"` | Exit 0; DB scaffold exists; **0** non-`file` symbol nodes (nothing indexed yet) | | |
| B7 | index after --no-index | `cd "$QC_ROOT/b6"` | `"$CW" index; echo "exit=$?"; sqlite3 .codewiki/codewiki.db "SELECT count(*) FROM nodes WHERE name='a';"` | Exit 0; now **1** node named `a` | | |
| B8 | `index` rebuild integrity | `cd "$QC_ROOT/b1"` | `"$CW" index >/dev/null 2>&1; sqlite3 .codewiki/codewiki.db "SELECT count(*) FROM nodes;"` | Node count unchanged vs B2 and > 0. **Note:** `index` may print `0 nodes, 0 edges` when nothing changed — that summary line is acceptable **only if** the DB still contains the correct nodes. If the DB is emptied/zeroed → **FAIL**. | | |
| B9 | uninit removes index | `D="$QC_ROOT/b9"; mkdir -p "$D" && cd "$D"; printf 'def a():\n    return 1\n' > a.py; "$CW" init >/dev/null 2>&1` | `"$CW" uninit --force; echo "exit=$?"; test -d .codewiki && echo "still-there" \|\| echo "removed"` | Exit 0; prints `removed`; `.codewiki/` gone | | |

---

## C. Per-Language Indexing Correctness

**Method for every case in C/D/E:** create the exact file shown, run `init -q`, then dump nodes and edges via the helpers in §3. Compare against the Expected columns. The expected values below were **grounded against this binary** on the same minimal inputs.

> Every indexed file also yields exactly one `file` node (name = basename, e.g. `sample.py`) plus `contains` edges from the file node and from container symbols. The Expected tables list the **symbol** nodes and the **semantic** edges; the `file`-node + structural `contains` edges are asserted separately in **C0**.

### C0 — Structural baseline (applies to every language case)

| ID | Area | Setup | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|---|
| C0 | file node + contains | Any indexed single-file project, e.g. `cd "$QC_ROOT/cpy"` after C-PY | `sqlite3 "$DB" "SELECT count(*) FROM nodes WHERE kind='file'; SELECT count(*) FROM edges WHERE kind='contains';"` | Exactly **1** `file` node; `contains` edge count ≥ (number of top-level symbols). `file` node name = file basename | | |

### C-PY — Python

Setup:
```bash
D="$QC_ROOT/cpy"; mkdir -p "$D" && cd "$D"
cat > sample.py <<'EOF'
class Greeter:
    def __init__(self, name):
        self.name = name

    def greet(self):
        return self._format()

    def _format(self):
        return "Hello, " + self.name
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| C-PY-1 | nodes | dump nodes | `class Greeter`; `method __init__`; `method greet`; `method _format` (3 methods, 1 class). No method mis-kinded as `function`. | | |
| C-PY-2 | calls edge | `sqlite3 "$DB" "SELECT s.name,t.name FROM edges e JOIN nodes s ON e.source=s.id JOIN nodes t ON e.target=t.id WHERE e.kind='calls';"` | Exactly one `calls`: `greet` → `_format` | | |
| C-PY-3 | counts | `"$CW" status \| grep -E 'Nodes\|Edges'` | Nodes = 5 (file + class + 3 methods); Edges ≥ 5 (4 contains + 1 calls) | | |

### C-RS — Rust

Setup:
```bash
D="$QC_ROOT/crs"; mkdir -p "$D" && cd "$D"
cat > sample.rs <<'EOF'
pub trait Speak {
    fn speak(&self) -> String;
}

pub struct Dog;

impl Speak for Dog {
    fn speak(&self) -> String {
        self.bark()
    }
}

impl Dog {
    fn bark(&self) -> String {
        String::from("woof")
    }
}

pub fn make_dog() -> Dog {
    Dog
}
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| C-RS-1 | struct + trait nodes | dump nodes | `struct Dog`; a node for trait `Speak`; `make_dog`; methods `speak`/`bark` | | |
| C-RS-2 | implements edge | `sqlite3 "$DB" "SELECT s.name,t.name FROM edges e JOIN nodes s ON e.source=s.id JOIN nodes t ON e.target=t.id WHERE e.kind='implements';"` | `Dog` → `Speak` present (1 `implements`) | | |
| C-RS-3 | calls edge | edge dump | `speak` → `bark` (`calls`) present | | |
| C-RS-4 | trait kind / dedup (KNOWN-RISK) | dump nodes | **Document actual kind of `Speak`** (this build maps Rust traits to `interface`, not `trait`). **Document whether `speak`/`bark` appear duplicated** (both `function` and `method`) and whether the `calls` edge is duplicated. If duplicate symbol nodes exist for one definition → **FAIL** (note exact rows). Mark PASS only if each definition yields one node. | | |

### C-TS — TypeScript

Setup:
```bash
D="$QC_ROOT/cts"; mkdir -p "$D" && cd "$D"
cat > sample.ts <<'EOF'
export interface Animal {
  speak(): string;
}

export class Dog implements Animal {
  speak(): string {
    return this.bark();
  }
  bark(): string {
    return "woof";
  }
}

export function makeDog(): Dog {
  return new Dog();
}
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| C-TS-1 | nodes | dump nodes | `interface Animal`; `class Dog`; `function makeDog`; `method speak`; `method bark` | | |
| C-TS-2 | calls edge | edge dump `kind='calls'` | `speak` → `bark` present | | |
| C-TS-3 | implements edge | `sqlite3 "$DB" "SELECT count(*) FROM edges WHERE kind='implements';"` | **Expected by language semantics: 1** (`Dog` implements `Animal`). **KNOWN-RISK:** this build produced **0** on grounding. Record actual count; `0` → **FAIL** (implements regression in TS). | | |
| C-TS-4 | instantiates edge | `sqlite3 "$DB" "SELECT count(*) FROM edges WHERE kind='instantiates';"` | `makeDog` instantiates `Dog`. Record actual count; document if 0. | | |

### C-JS — JavaScript

Setup:
```bash
D="$QC_ROOT/cjs"; mkdir -p "$D" && cd "$D"
cat > sample.js <<'EOF'
class Calculator {
  add(a, b) {
    return this.sum(a, b);
  }
  sum(a, b) {
    return a + b;
  }
}

function makeCalc() {
  return new Calculator();
}

module.exports = { Calculator, makeCalc };
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| C-JS-1 | nodes | dump nodes | `class Calculator`; `method add`; `method sum`; `function makeCalc` | | |
| C-JS-2 | calls edge | `kind='calls'` dump | `add` → `sum` present | | |

### C-CS — C#

Setup:
```bash
D="$QC_ROOT/ccs"; mkdir -p "$D" && cd "$D"
cat > Sample.cs <<'EOF'
namespace App
{
    public interface IGreeter
    {
        string Greet();
    }

    public class Greeter : IGreeter
    {
        public string Greet()
        {
            return "Hello";
        }
    }

    public class Service
    {
        private readonly IGreeter _greeter;
        public Service(IGreeter greeter)
        {
            _greeter = greeter;
        }
        public string Run()
        {
            return _greeter.Greet();
        }
    }
}
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| C-CS-1 | nodes | dump nodes | `namespace App`; `interface IGreeter`; `class Greeter`; `class Service`; `method Greet` (×2: one in interface, one in class); `method Run`; constructor `Service` | | |
| C-CS-2 | namespace contains | edge dump | `App` `contains` `IGreeter`, `Greeter`, `Service` | | |
| C-CS-3 | implements edge (post resolver fix) | `sqlite3 "$DB" "SELECT s.name,t.name FROM edges e JOIN nodes s ON e.source=s.id JOIN nodes t ON e.target=t.id WHERE e.kind='implements';"` | **Expected: `Greeter` → `IGreeter`** (the recent C# resolver fix must produce this `implements` edge). **KNOWN-RISK:** grounding on this build produced **0** `implements` edges → if still 0 after build, **FAIL** and this is the headline regression. | | |
| C-CS-4 | DI call resolution | `sqlite3 "$DB" "SELECT s.name,t.name,e.provenance FROM edges e JOIN nodes s ON e.source=s.id JOIN nodes t ON e.target=t.id WHERE e.kind='calls';"` | `Run` → `Greet` (`calls`). Note provenance: should be DI/type-resolved (e.g. `framework-csharp`), not ambiguous `NameMatcher`. If resolved only via `NameMatcher` against the wrong/ambiguous `Greet`, flag in Notes. | | |

### C-GO — Go

Setup:
```bash
D="$QC_ROOT/cgo"; mkdir -p "$D" && cd "$D"
cat > sample.go <<'EOF'
package main

type Speaker interface {
	Speak() string
}

type Dog struct{}

func (d Dog) Speak() string {
	return d.bark()
}

func (d Dog) bark() string {
	return "woof"
}

func main() {
	d := Dog{}
	d.Speak()
}
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| C-GO-1 | nodes | dump nodes | `interface Speaker`; `struct Dog`; `method Speak`; `method bark`; `function main` | | |
| C-GO-2 | calls edges | `kind='calls'` dump | `Speak` → `bark` and `main` → `Speak` present | | |
| C-GO-3 | implements (structural) | `sqlite3 "$DB" "SELECT count(*) FROM edges WHERE kind='implements';"` | Go satisfies interfaces structurally. **KNOWN-RISK:** grounding produced **0**. Record actual; document whether `Dog`→`Speaker` `implements` is expected/supported. Not an automatic FAIL — note the design decision, but flag if README/docs claim Go implements edges. | | |

### C-JAVA — Java

Setup:
```bash
D="$QC_ROOT/cjava"; mkdir -p "$D" && cd "$D"
cat > Sample.java <<'EOF'
package app;

interface Animal {
    String speak();
}

class Dog implements Animal {
    public String speak() {
        return bark();
    }
    private String bark() {
        return "woof";
    }
}
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| C-JAVA-1 | nodes | dump nodes | `interface Animal`; `class Dog`; `method speak`; `method bark` (optionally a `namespace`/package node `app`) | | |
| C-JAVA-2 | calls edge | `kind='calls'` dump | `speak` → `bark` present | | |
| C-JAVA-3 | implements edge | `sqlite3 "$DB" "SELECT count(*) FROM edges WHERE kind='implements';"` | Expected: 1 (`Dog` implements `Animal`). Record actual count; 0 → FAIL or document. | | |

### C-CPP — C++

Setup:
```bash
D="$QC_ROOT/ccpp"; mkdir -p "$D" && cd "$D"
cat > sample.cpp <<'EOF'
class Base {
public:
    virtual int value() const { return compute(); }
    int compute() const { return 42; }
};

class Derived : public Base {
public:
    int value() const override { return 7; }
};

int makeIt() {
    Derived d;
    return d.value();
}
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| C-CPP-1 | nodes | dump nodes | `class Base`; `class Derived`; methods `value` (×2), `compute`; `function makeIt` | | |
| C-CPP-2 | extends edge | `sqlite3 "$DB" "SELECT s.name,t.name FROM edges e JOIN nodes s ON e.source=s.id JOIN nodes t ON e.target=t.id WHERE e.kind='extends';"` | Expected: `Derived` → `Base`. Record actual; document if missing. | | |
| C-CPP-3 | calls edge | `kind='calls'` dump | `value` (Base) → `compute` present | | |

### C-VUE — Vue

Setup:
```bash
D="$QC_ROOT/cvue"; mkdir -p "$D" && cd "$D"
cat > Hello.vue <<'EOF'
<template>
  <div>{{ msg }}</div>
</template>

<script>
export default {
  name: 'HelloWorld',
  data() {
    return { msg: 'hi' };
  },
  methods: {
    greet() {
      return this.msg;
    }
  }
}
</script>
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| C-VUE-1 | component node | `sqlite3 "$DB" "SELECT kind,name FROM nodes WHERE kind='component';"` | One `component` node (grounded name = `Hello`, from filename). Record exact name; document if it uses the `name:` option (`HelloWorld`) vs filename | | |
| C-VUE-2 | method nodes | dump nodes | `method greet` and `method data` present | | |

### C-REST — Remaining languages (smoke only)

For each language below: create the one-file sample, `init -q`, assert (a) exit 0, (b) ≥1 non-`file` node, (c) no panic. These are smoke checks; deep edge assertions are out of scope but record the node kinds observed.

| ID | Language | Setup file (name → minimal content) | Steps | Expected | Result | Notes |
|---|---|---|---|---|---|---|
| C-REST-PHP | PHP | `t.php` → `<?php class A { function f(){ return $this->g(); } function g(){ return 1; } }` | init -q; dump nodes | `class A`, `method f`, `method g`; ≥1 `calls` edge `f`→`g` | | |
| C-REST-RB | Ruby | `t.rb` → `class A\n  def f\n    g\n  end\n  def g\n    1\n  end\nend` | init -q; dump nodes | `class A`, methods `f`,`g` | | |
| C-REST-SWIFT | Swift | `t.swift` → `class A {\n  func f() -> Int { return g() }\n  func g() -> Int { return 1 }\n}` | init -q; dump nodes | `class A`, methods `f`,`g`; `calls` `f`→`g` if supported | | |
| C-REST-KT | Kotlin | `t.kt` → `class A {\n  fun f(): Int = g()\n  fun g(): Int = 1\n}` | init -q; dump nodes | `class A`, functions/methods `f`,`g` | | |
| C-REST-SCALA | Scala | `t.scala` → `class A {\n  def f(): Int = g()\n  def g(): Int = 1\n}` | init -q; dump nodes | `class A`, `f`,`g` | | |
| C-REST-C | C | `t.c` → `int g(){return 1;}\nint f(){return g();}` | init -q; dump nodes | functions `g`,`f`; `calls` `f`→`g` | | |
| C-REST-DART | Dart | `t.dart` → `class A {\n  int f() => g();\n  int g() => 1;\n}` | init -q; dump nodes | `class A`, methods `f`,`g` | | |
| C-REST-LUA | Lua | `t.lua` → `local function g() return 1 end\nlocal function f() return g() end` | init -q; dump nodes | functions `g`,`f`; ≥1 node | | |
| C-REST-SVELTE | Svelte | `T.svelte` → `<script>\nfunction greet(){ return 'hi'; }\n</script>\n<div>{greet()}</div>` | init -q; dump nodes | ≥1 node (component or function `greet`) | | |

> Pascal / DFM / Razor / Liquid / Luau are also supported (`Language` enum). They are **out of scope for deep assertions** here; if time permits, run a one-file smoke (≥1 node, exit 0, no panic) and record results in Notes.

---

## D. Edge-Kind Correctness

These cases isolate each semantic edge kind. (C# `implements` + DI is the headline area post the recent resolver fix.)

| ID | Area | Setup | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|---|
| D1 | `implements` (C#) | use `$QC_ROOT/ccs` from C-CS | `sqlite3 "$DB" "SELECT s.name,t.name FROM edges e JOIN nodes s ON e.source=s.id JOIN nodes t ON e.target=t.id WHERE e.kind='implements';"` | `Greeter` → `IGreeter` present (count ≥ 1). Empty → **FAIL** (regression). | | |
| D2 | `implements` (Rust) | use `$QC_ROOT/crs` from C-RS | same query, `kind='implements'` | `Dog` → `Speak` present | | |
| D3 | `extends` (C++) | use `$QC_ROOT/ccpp` from C-CPP | same query, `kind='extends'` | `Derived` → `Base` present (or document support gap) | | |
| D4 | `calls` (Python) | use `$QC_ROOT/cpy` from C-PY | `kind='calls'` | `greet` → `_format`, exactly 1 | | |
| D5 | `imports` (JS/Express) | `D="$QC_ROOT/d5"; mkdir -p "$D" && cd "$D"; printf "const express = require('express');\nconst app = express();\nmodule.exports = app;\n" > app.js; "$CW" init -q; DB=.codewiki/codewiki.db` | `sqlite3 "$DB" "SELECT s.name,t.name,e.provenance FROM edges e JOIN nodes s ON e.source=s.id JOIN nodes t ON e.target=t.id WHERE e.kind='imports';"` | An `imports` edge for `express` exists (grounded: `app.js` → `express`, provenance `NameMatcher`). Record actual. | | |
| D6 | no spurious edges | use `$QC_ROOT/cpy` | `sqlite3 "$DB" "SELECT DISTINCT kind FROM edges;"` | Only `contains` and `calls` for this file — no stray `extends`/`implements`/`renders` that the source does not justify | | |
| D7 | edge endpoints valid | any indexed DB | `sqlite3 "$DB" "SELECT count(*) FROM edges e LEFT JOIN nodes s ON e.source=s.id LEFT JOIN nodes t ON e.target=t.id WHERE s.id IS NULL OR t.id IS NULL;"` | **0** dangling edges (every edge endpoint resolves to a real node) | | |

---

## E. Framework Resolvers

> **KNOWN-RISK (grounded):** on this build, Express and Django produced **no `route` nodes** (handlers appeared only as plain `function`/`variable` nodes). These cases assert the *expected* framework behavior; if `route`/`component` nodes are absent, mark **FAIL** and record it as a framework-resolver regression. Re-verify after any rebuild.

### E-EXPRESS — Express routes

```bash
D="$QC_ROOT/eexp"; mkdir -p "$D" && cd "$D"
cat > app.js <<'EOF'
const express = require('express');
const app = express();

app.get('/users', function getUsers(req, res) {
  res.send('users');
});

app.post('/users', function createUser(req, res) {
  res.send('created');
});

module.exports = app;
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| E-EXP-1 | route nodes | `sqlite3 "$DB" "SELECT kind,name FROM nodes WHERE kind='route' ORDER BY name;"` | 2 `route` nodes (GET `/users`, POST `/users`). Grounded actual = **0** → FAIL if still 0. | | |
| E-EXP-2 | handler functions | dump nodes | `function getUsers`, `function createUser` present (these DID appear in grounding) | | |

### E-DJANGO — Django routes

```bash
D="$QC_ROOT/edj"; mkdir -p "$D" && cd "$D"
cat > urls.py <<'EOF'
from django.urls import path
from . import views

urlpatterns = [
    path('home/', views.home, name='home'),
    path('about/', views.about, name='about'),
]
EOF
cat > views.py <<'EOF'
def home(request):
    return "home"

def about(request):
    return "about"
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| E-DJ-1 | route nodes | `sqlite3 "$DB" "SELECT kind,name FROM nodes WHERE kind='route';"` | 2 `route` nodes (`home/`, `about/`). Grounded actual = **0** → FAIL if still 0. | | |
| E-DJ-2 | view functions | `sqlite3 "$DB" "SELECT name FROM nodes WHERE kind='function' ORDER BY name;"` | `home`, `about` present | | |

### E-ANGULAR — Angular component

```bash
D="$QC_ROOT/eng"; mkdir -p "$D" && cd "$D"
cat > app.component.ts <<'EOF'
import { Component } from '@angular/core';

@Component({
  selector: 'app-root',
  template: '<h1>{{ title }}</h1>'
})
export class AppComponent {
  title = 'demo';
  greet(): string {
    return this.title;
  }
}
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| E-NG-1 | component node | `sqlite3 "$DB" "SELECT kind,name FROM nodes WHERE name='AppComponent';"` | `AppComponent` present; kind should be `component` (framework resolver). If kind is plain `class`, record it (component-resolver gap). | | |
| E-NG-2 | method | dump nodes | `method greet` present | | |

### E-ASPNET — ASP.NET route + DI

```bash
D="$QC_ROOT/easp"; mkdir -p "$D" && cd "$D"
cat > UsersController.cs <<'EOF'
using Microsoft.AspNetCore.Mvc;

namespace Api
{
    public interface IUserService { string GetAll(); }

    public class UserService : IUserService
    {
        public string GetAll() { return "all"; }
    }

    [ApiController]
    [Route("api/users")]
    public class UsersController : ControllerBase
    {
        private readonly IUserService _svc;
        public UsersController(IUserService svc) { _svc = svc; }

        [HttpGet]
        public string List() { return _svc.GetAll(); }
    }
}
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| E-ASP-1 | DI implements | `sqlite3 "$DB" "SELECT s.name,t.name FROM edges e JOIN nodes s ON e.source=s.id JOIN nodes t ON e.target=t.id WHERE e.kind='implements';"` | `UserService` → `IUserService` present (FAIL if 0 — same C# resolver path as D1) | | |
| E-ASP-2 | route node | `sqlite3 "$DB" "SELECT kind,name FROM nodes WHERE kind='route';"` | A `route` node for `api/users` (or `List`/HttpGet). Record actual; document if 0 (ASP.NET route-resolver gap) | | |
| E-ASP-3 | DI call resolution | `sqlite3 "$DB" "SELECT s.name,t.name,e.provenance FROM edges e JOIN nodes s ON e.source=s.id JOIN nodes t ON e.target=t.id WHERE e.kind='calls' AND t.name='GetAll';"` | `List` → `GetAll` present; provenance ideally DI-resolved, not pure name-match | | |

---

## F. MCP Tool Correctness

Two execution paths — run **both** and they must agree:
- **F-CLI**: the CLI subcommands that mirror the tools (`query`↔search, `context`, `callers`, `callees`, `impact`, `files`, `status`; node/explore via `query`+`context`).
- **F-MCP**: raw JSON-RPC against `serve --mcp` (verifies the actual MCP surface the agents use).

**Shared setup** (indexed Python project with a known call graph):
```bash
D="$QC_ROOT/fmcp"; mkdir -p "$D" && cd "$D"
cat > sample.py <<'EOF'
class Greeter:
    def greet(self):
        return self._format()

    def _format(self):
        return "Hi"
EOF
"$CW" init -q; export DB=.codewiki/codewiki.db
```

### F-CLI — CLI mirrors of the tools

| ID | Tool | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| F-CLI-1 | search | `"$CW" query greet` | Table lists `greet` (method) at `sample.py:2`; non-empty; exit 0 | | |
| F-CLI-2 | search qualifiers | `"$CW" query "kind:method"` | Returns only method-kind nodes (`greet`, `_format`); none of kind class/file | | |
| F-CLI-3 | context | `"$CW" context "greeting logic"` | Markdown with a `Nodes` section listing `Greeter`/`greet`/`_format` and an `Edges` section; non-empty | | |
| F-CLI-4 | callers | `"$CW" callers _format` | Shows `greet` as a caller of `_format` (`--[calls]-->`) | | |
| F-CLI-5 | callees | `"$CW" callees greet` | Shows `_format` as a callee of `greet` | | |
| F-CLI-6 | impact | `"$CW" impact _format` | Lists affected nodes incl. `_format`, `greet`, `Greeter`; non-empty count | | |
| F-CLI-7 | files | `"$CW" files` | Lists `sample.py` with language `Python`, node_count > 0 | | |
| F-CLI-8 | status | `"$CW" status` | Nodes/Edges/Files all > 0; journal mode `wal`; "Nodes by kind" includes `method` and `class` | | |
| F-CLI-9 | query empty result | `"$CW" query zzz_nonexistent_symbol; echo "exit=$?"` | Empty result handled gracefully (no rows / "no matches"); exit 0; no panic | | |

### F-MCP — Raw JSON-RPC (the actual 9 tools)

Helper to drive the server (sends `initialize` then a tool call; reads from stdin):
```bash
mcp_call() {  # usage: mcp_call '<json for the tools/call request>'
  printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"qc","version":"1"}}}' \
  "$1" \
  | timeout 8 "$CW" serve --mcp --no-watch 2>/dev/null
}
```
> Run all F-MCP cases from inside `$QC_ROOT/fmcp` (the server uses cwd as project root). Responses are JSON-RPC; the result content is in `result.content`. If line-buffering makes a single piped read flaky, increase the `timeout` value and/or add a trailing `{"jsonrpc":"2.0","id":99,"method":"ping"}` line to flush — record any transport quirk in Notes rather than marking FAIL for a timing artifact.

| ID | Tool | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| F-MCP-0 | tools/list | `printf '%s\n%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"qc","version":"1"}}}' '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \| timeout 8 "$CW" serve --mcp --no-watch 2>/dev/null \| grep -oE 'codewiki_[a-z]+' \| sort -u` | Exactly these 9: `codewiki_callees, codewiki_callers, codewiki_context, codewiki_explore, codewiki_files, codewiki_impact, codewiki_node, codewiki_search, codewiki_status` | | |
| F-MCP-1 | search | `mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codewiki_search","arguments":{"query":"greet"}}}'` | Response contains `greet`; no `"isError":true` | | |
| F-MCP-2 | context | `mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codewiki_context","arguments":{"task":"greeting logic"}}}'` | Non-empty content mentioning `Greeter`/`greet` | | |
| F-MCP-3 | callers | `mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codewiki_callers","arguments":{"name":"_format"}}}'` | Mentions `greet` | | |
| F-MCP-4 | callees | `mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codewiki_callees","arguments":{"name":"greet"}}}'` | Mentions `_format` | | |
| F-MCP-5 | impact | `mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codewiki_impact","arguments":{"name":"_format"}}}'` | Non-empty affected set incl. `greet` | | |
| F-MCP-6 | status | `mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codewiki_status","arguments":{}}}'` | JSON-ish stats with node_count/edge_count > 0 | | |
| F-MCP-7 | files | `mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codewiki_files","arguments":{}}}'` | Lists `sample.py` | | |
| F-MCP-8 | node | `mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codewiki_node","arguments":{"name":"greet"}}}'` | Returns `greet` symbol detail (location `sample.py:2`, kind method). (If schema needs `id` instead of `name`, first get the id via search and retry; record the accepted arg shape.) | | |
| F-MCP-9 | explore | `mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codewiki_explore","arguments":{"query":"Greeter"}}}'` | Returns source/grouped detail for `Greeter` and its methods; non-empty | | |
| F-MCP-10 | bad tool name | `mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codewiki_does_not_exist","arguments":{}}}'` | JSON-RPC error (method/tool not found) — **no crash**, server stays responsive | | |
| F-MCP-11 | parity vs CLI | compare F-MCP-3/4 output to F-CLI-4/5 | MCP and CLI report the **same** caller/callee relationships (no divergence) | | |

> **Tool arg names:** if a `tools/call` returns an argument-validation error, inspect the `inputSchema` from the F-MCP-0 `tools/list` output and retry with the documented arg names. Record the exact accepted schema in Notes; an arg-name mismatch in *this plan* is a doc fix, not a product FAIL — but a tool that rejects all reasonable inputs **is** a FAIL.

---

## G. Incremental Sync Correctness

**Shared setup:**
```bash
D="$QC_ROOT/gsync"; mkdir -p "$D" && cd "$D"
printf 'def alpha():\n    return 1\n' > a.py
"$CW" init -q; export DB=.codewiki/codewiki.db
sqlite3 "$DB" "SELECT count(*) FROM nodes WHERE kind='function';"   # baseline = 1
```

| ID | Area | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|
| G1 | add symbol + new file | `cd "$QC_ROOT/gsync"; printf 'def alpha():\n    return beta()\n\ndef beta():\n    return 2\n' > a.py; printf 'def gamma():\n    return 3\n' > b.py; "$CW" sync; sqlite3 "$DB" "SELECT name FROM nodes WHERE kind='function' ORDER BY name;"` | Sync reports `added`/`modified` > 0; functions now `alpha, beta, gamma`; new `calls` edge `alpha`→`beta` exists (`sqlite3 "$DB" "SELECT count(*) FROM edges WHERE kind='calls';"` ≥ 1) | | |
| G2 | rename symbol | `cd "$QC_ROOT/gsync"; printf 'def alpha():\n    return renamed()\n\ndef renamed():\n    return 2\n' > a.py; "$CW" sync; sqlite3 "$DB" "SELECT name FROM nodes WHERE name IN ('beta','renamed');"` | `beta` gone, `renamed` present; old node removed, not duplicated | | |
| G3 | remove symbol | `cd "$QC_ROOT/gsync"; printf 'def alpha():\n    return 1\n' > a.py; "$CW" sync; sqlite3 "$DB" "SELECT count(*) FROM nodes WHERE name='renamed';"` | `renamed` count = 0 (symbol removed); `alpha` still present | | |
| G4 | delete file | `cd "$QC_ROOT/gsync"; rm b.py; "$CW" sync; sqlite3 "$DB" "SELECT count(*) FROM nodes WHERE name='gamma'; SELECT count(*) FROM files WHERE path LIKE '%b.py';"` | Sync reports `removed` ≥ 1; `gamma` count = 0; no `b.py` file row | | |
| G5 | add file back | `cd "$QC_ROOT/gsync"; printf 'def delta():\n    return 4\n' > c.py; "$CW" sync; sqlite3 "$DB" "SELECT count(*) FROM nodes WHERE name='delta';"` | `delta` count = 1; `c.py` indexed | | |
| G6 | mtime-preserved content change (size/hash tier) | `cd "$QC_ROOT/gsync"; touch -r a.py /tmp/qc_mref; printf 'def alpha():\n    return changed_fn()\n\ndef changed_fn():\n    return 9\n' > a.py; touch -r /tmp/qc_mref a.py; "$CW" sync; sqlite3 "$DB" "SELECT count(*) FROM nodes WHERE name='changed_fn';"` | `changed_fn` count = 1 — change detected **despite identical mtime** (content hash/size tier works). 0 → **FAIL** (stale-index bug) | | |
| G7 | no-op sync idempotent | `cd "$QC_ROOT/gsync"; "$CW" sync; "$CW" sync; sqlite3 "$DB" "SELECT count(*) FROM nodes WHERE name='alpha';"` | Second sync reports `0 added, 0 modified, 0 removed`; `alpha` count still exactly 1 (no duplication) | | |
| G8 | dangling edges after churn | `cd "$QC_ROOT/gsync"; sqlite3 "$DB" "SELECT count(*) FROM edges e LEFT JOIN nodes s ON e.source=s.id LEFT JOIN nodes t ON e.target=t.id WHERE s.id IS NULL OR t.id IS NULL;"` | 0 dangling edges after all the add/rename/remove churn | | |

---

## H. Robustness / Edge Cases

| ID | Area | Setup | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|---|
| H1 | empty repo | `D="$QC_ROOT/h1"; mkdir -p "$D" && cd "$D"` | `"$CW" init; echo "exit=$?"` | Exit 0; reports 0 source files (or only a sentinel); DB created; no panic | | |
| H2 | binary / non-source ignored | `D="$QC_ROOT/h2"; mkdir -p "$D" && cd "$D"; printf 'def a():\n    return 1\n' > a.py; head -c 4096 /dev/urandom > blob.bin; printf 'plain text\n' > notes.txt; printf '{"k":1}\n' > data.json` | `"$CW" init -q; sqlite3 .codewiki/codewiki.db "SELECT DISTINCT language FROM files;"` | `a.py` indexed; `blob.bin` NOT indexed as code; no panic. (`.txt`/`.json` may or may not be indexed — record actual; the hard requirement is **no crash** and **binary not parsed as source**.) | | |
| H3 | large file | `D="$QC_ROOT/h3"; mkdir -p "$D" && cd "$D"; python3 -c "f=open('big.py','w'); [f.write(f'def fn_{i}():\n    return {i}\n\n') for i in range(5000)]; f.close()" 2>/dev/null \|\| awk 'BEGIN{for(i=0;i<5000;i++)printf "def fn_%d():\n    return %d\n\n",i,i}' > big.py` | `time "$CW" init -q; echo "exit=$?"; sqlite3 .codewiki/codewiki.db "SELECT count(*) FROM nodes WHERE kind='function';"` | Completes (no hang/OOM); exit 0; function count ≈ 5000 (record actual; must be in the thousands, not 0) | | |
| H4 | unicode / Vietnamese identifiers | `D="$QC_ROOT/h4"; mkdir -p "$D" && cd "$D"; printf '# Tính toán\nclass MáyTính:\n    def tính_tổng(self, a, b):\n        return self.cộng(a, b)\n    def cộng(self, a, b):\n        return a + b\n' > t.py` | `"$CW" init -q; echo "exit=$?"; sqlite3 .codewiki/codewiki.db "SELECT kind,name FROM nodes ORDER BY kind,name;"` | Exit 0; nodes preserve unicode names (`MáyTính`, `tính_tổng`, `cộng`); `calls` edge `tính_tổng`→`cộng`; no mojibake; no panic | | |
| H5 | mixed-language repo | `D="$QC_ROOT/h5"; mkdir -p "$D" && cd "$D"; printf 'def a():\n    return 1\n' > x.py; printf 'pub fn b() -> i32 { 1 }\n' > y.rs; printf 'function c(){return 1;}\n' > z.js` | `"$CW" init -q; sqlite3 .codewiki/codewiki.db "SELECT language,count(*) FROM files GROUP BY language;"` | 3 languages present (Python, Rust, JavaScript); each with ≥1 node; no cross-language confusion | | |
| H6 | deeply nested dirs / .gitignore respect | `D="$QC_ROOT/h6"; mkdir -p "$D/src/a/b/c" "$D/node_modules/pkg" && cd "$D"; printf 'def deep():\n    return 1\n' > src/a/b/c/d.py; printf 'def vendored():\n    return 1\n' > node_modules/pkg/v.py; printf 'node_modules/\n' > .gitignore` | `"$CW" init -q; sqlite3 .codewiki/codewiki.db "SELECT count(*) FROM nodes WHERE name='deep'; SELECT count(*) FROM nodes WHERE name='vendored';"` | `deep` = 1 (nested file found); `vendored` = 0 if `node_modules`/.gitignore is respected — record actual ignore behavior | | |
| H7 | real repo: Flask (Python) | `D="$QC_ROOT/h7"; git clone --depth 1 https://github.com/pallets/flask "$D" 2>/dev/null && cd "$D"` | `time "$CW" init -q; echo "exit=$?"; "$CW" status \| grep -E 'Nodes\|Files'` | Clones+indexes without panic; exit 0; Nodes in the **thousands**, Files in the **dozens+**; completes in reasonable time. BLOCKED if no network. | | |
| H8 | real repo: ripgrep (Rust) | `D="$QC_ROOT/h8"; git clone --depth 1 https://github.com/BurntSushi/ripgrep "$D" 2>/dev/null && cd "$D"` | `time "$CW" init -q; echo "exit=$?"; "$CW" status \| grep -E 'Nodes\|Files'; sqlite3 .codewiki/codewiki.db "SELECT count(*) FROM edges WHERE kind='implements';"` | Indexes without panic; exit 0; thousands of nodes; `implements` edges > 0 (Rust trait impls); completes. BLOCKED if no network. | | |
| H9 | real repo: Express (JS) | `D="$QC_ROOT/h9"; git clone --depth 1 https://github.com/expressjs/express "$D" 2>/dev/null && cd "$D"` | `time "$CW" init -q; echo "exit=$?"; "$CW" status \| grep -E 'Nodes\|Files'` | Indexes without panic; exit 0; nodes > 0; completes. Record `route` node count (relates to E-EXPRESS finding). BLOCKED if no network. | | |
| H10 | query on real repo sanity | after H7 (`cd "$QC_ROOT/h7"`) | `"$CW" query Flask -n 5; "$CW" callers __init__ \| head` | Returns plausible, non-empty results; no panic | | |

---

## I. Graph UI + `serve`

> Pick an unused port per case (examples use 7090–7099). If a port is busy, choose another and note it. Always kill the background server at the end.

| ID | Area | Setup | Steps | Expected result | Result | Notes |
|---|---|---|---|---|---|---|
| I1 | graph serves root | indexed project, e.g. `cd "$QC_ROOT/fmcp"` | `"$CW" graph --port 7090 --no-open >/tmp/g1.log 2>&1 & GPID=$!; sleep 3; curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:7090/; kill $GPID` | HTTP `200` from `/`; log prints the UI URL | | |
| I2 | /api/health | as I1 (port 7091) | `... curl -s http://127.0.0.1:7091/api/health; kill $GPID` | JSON with `"ok":true` and `db_size_bytes` | | |
| I3 | /api/stats has data | as I1 (port 7092) | `... curl -s http://127.0.0.1:7092/api/stats; kill $GPID` | JSON with `node_count>0`, `edge_count>0`, `nodes_by_kind`, `edges_by_kind`, `journal_mode`:`wal` | | |
| I4 | /api/files | as I1 (port 7093) | `... curl -s http://127.0.0.1:7093/api/files; kill $GPID` | JSON `{"files":[...],"total":N}` with N>0, each file has `path`,`language`,`node_count` | | |
| I5 | /api/top-nodes | as I1 (port 7094) | `... curl -s http://127.0.0.1:7094/api/top-nodes; kill $GPID` | JSON `{"nodes":[{node:{id,name,kind,...},...}]}` non-empty | | |
| I6 | /api/node/{id} | as I1 (port 7095); first get an id: `ID=$(sqlite3 "$QC_ROOT/fmcp/.codewiki/codewiki.db" "SELECT id FROM nodes WHERE name='greet' LIMIT 1;")` | `... curl -s "http://127.0.0.1:7095/api/node/$ID"; kill $GPID` | JSON node detail for `greet`; HTTP 200 | | |
| I7 | /api/neighborhood/{id} | as I6 (port 7096) | `... curl -s "http://127.0.0.1:7096/api/neighborhood/$ID"; kill $GPID` | JSON subgraph (nodes+edges) around `greet`; non-empty | | |
| I8 | unknown route 404 | as I1 (port 7097) | `... curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:7097/api/definitely-not-real; kill $GPID` | Non-200 (404) — server doesn't crash on unknown API path | | |
| I9 | serve --mcp graceful in UNINDEXED dir | `D="$QC_ROOT/i9"; mkdir -p "$D" && cd "$D"` | `echo 'garbage-not-jsonrpc' \| timeout 3 "$CW" serve --mcp --no-watch; echo "exit=$?"` | **No panic / no Rust backtrace.** Graceful protocol error (grounded: `MCP protocol error: ... expect initialize request`). **Verify after build** — this depended on a pending fix; a panic/segfault here = **FAIL**. | | |
| I10 | serve --mcp valid handshake in UNINDEXED dir | `cd "$QC_ROOT/i9"` | `printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"qc","version":"1"}}}' \| timeout 5 "$CW" serve --mcp --no-watch 2>&1 \| head -c 400; echo` | Returns a valid `initialize` result (serverInfo `codewiki`); tool calls against the empty index return graceful empty/error, not a crash | | |

---

## 5. Execution Protocol — 3 Independent QC Agents

**Independence requirement.** Three QC agents (**Agent1**, **Agent2**, **Agent3**) execute this plan **independently and in isolation**. Each:

1. **Own workspace.** Sets its own `QC_ROOT` (the §3 snippet already makes it unique via `$$` and uid). Agents must NOT share temp dirs or the same `.codewiki/` DB. Never run inside `/root/develop/code-wiki` itself.
2. **Same binary.** All agents use `/root/develop/code-wiki/target/release/codewiki`. Record `codewiki --version` and `sha256sum` of the binary at the top of the report so all three are provably testing the same artifact.
3. **No source edits.** This is execution only — do not modify CodeWiki source, do not rebuild, do not `cargo` anything. If the binary appears stale vs a claimed fix, mark the relevant case **BLOCKED** with the note "needs rebuild" rather than editing/building.
4. **Run every case in order**, top to bottom (A→I). A case that depends on prior setup (e.g. F-MCP reuses `$QC_ROOT/fmcp`) must run after that setup case in the same agent's workspace.
5. **Record per case:** the mark (**PASS / FAIL / BLOCKED**) **and pasted evidence** — the actual command output or the actual SQL rows that justify the mark. A bare "PASS" with no evidence is not acceptable.
6. **Determinism:** because inputs are fixed minimal files, node/edge **kinds and relationships** must be identical across agents. Counts must match exactly for the C/D/E/G cases. For real-repo cases (H7–H9) exact counts will vary by clone state — assert only the thresholds (counts > 0 / in the thousands, no panic, completes).
7. **Cleanup:** after finishing, each agent removes its `QC_ROOT` (`rm -rf "$QC_ROOT"`).

**Discrepancy handling.** After all three finish, fill the summary table. Any row where the three marks are not unanimous is a **DISCREPANCY** and must be investigated: re-run that single case in a clean workspace, capture evidence, and record the resolved verdict + root cause (e.g. environment difference, flaky MCP transport timing, nondeterministic ordering). Non-unanimous rows are the highest-priority follow-ups.

**Known-risk areas flagged during plan design (expect possible FAILs; confirm against the freshly built binary):**
- **C# `implements` edge** (C-CS-3, D1, E-ASP-1) — grounded as **0**; the recent C# resolver fix must make this ≥1.
- **TypeScript `implements`** (C-TS-3) — grounded as **0**.
- **Express/Django/ASP.NET `route` nodes** (E-EXP-1, E-DJ-1, E-ASP-2) — grounded as **0** route nodes.
- **Rust trait node kind + duplicate symbol nodes** (C-RS-4) — traits map to `interface`; watch for duplicated `function`/`method` nodes per definition.
- **`serve --mcp` graceful start** (I9) — depends on a pending fix; verify no panic.

---

## 6. Results Summary (fill after execution)

Binary sha256: `__________`  •  Version: `__________`  •  Date: `__________`

| ID | Case | Agent1 | Agent2 | Agent3 | Unanimous? | Notes / discrepancy root cause |
|---|---|---|---|---|---|---|
| A1 | --version | | | | | |
| A2 | top help | | | | | |
| A3 | per-cmd help | | | | | |
| A4 | init help flags | | | | | |
| A5 | unknown cmd | | | | | |
| A6 | status no index | | | | | |
| A7 | doctor no index | | | | | |
| A8 | doctor --strict | | | | | |
| A9 | query no index | | | | | |
| B1 | init creates DB | | | | | |
| B2 | indexed counts real | | | | | |
| B3 | git hooks installed | | | | | |
| B4 | no .git graceful | | | | | |
| B5 | re-run init sane | | | | | |
| B6 | --no-index scaffold | | | | | |
| B7 | index after --no-index | | | | | |
| B8 | index rebuild integrity | | | | | |
| B9 | uninit removes index | | | | | |
| C0 | file node + contains | | | | | |
| C-PY-1..3 | Python | | | | | |
| C-RS-1..4 | Rust | | | | | |
| C-TS-1..4 | TypeScript | | | | | |
| C-JS-1..2 | JavaScript | | | | | |
| C-CS-1..4 | C# | | | | | |
| C-GO-1..3 | Go | | | | | |
| C-JAVA-1..3 | Java | | | | | |
| C-CPP-1..3 | C++ | | | | | |
| C-VUE-1..2 | Vue | | | | | |
| C-REST-* | other langs | | | | | |
| D1 | implements C# | | | | | |
| D2 | implements Rust | | | | | |
| D3 | extends C++ | | | | | |
| D4 | calls Python | | | | | |
| D5 | imports JS | | | | | |
| D6 | no spurious edges | | | | | |
| D7 | edge endpoints valid | | | | | |
| E-EXP-1..2 | Express routes | | | | | |
| E-DJ-1..2 | Django routes | | | | | |
| E-NG-1..2 | Angular component | | | | | |
| E-ASP-1..3 | ASP.NET route+DI | | | | | |
| F-CLI-1..9 | CLI tool mirrors | | | | | |
| F-MCP-0..11 | MCP raw JSON-RPC | | | | | |
| G1 | sync add | | | | | |
| G2 | sync rename | | | | | |
| G3 | sync remove symbol | | | | | |
| G4 | sync delete file | | | | | |
| G5 | sync add file back | | | | | |
| G6 | mtime-preserved change | | | | | |
| G7 | no-op idempotent | | | | | |
| G8 | dangling edges | | | | | |
| H1 | empty repo | | | | | |
| H2 | binary ignored | | | | | |
| H3 | large file | | | | | |
| H4 | unicode/Vietnamese | | | | | |
| H5 | mixed-language | | | | | |
| H6 | nested / gitignore | | | | | |
| H7 | real repo Flask | | | | | |
| H8 | real repo ripgrep | | | | | |
| H9 | real repo Express | | | | | |
| H10 | query real repo | | | | | |
| I1 | graph serves root | | | | | |
| I2 | /api/health | | | | | |
| I3 | /api/stats | | | | | |
| I4 | /api/files | | | | | |
| I5 | /api/top-nodes | | | | | |
| I6 | /api/node/{id} | | | | | |
| I7 | /api/neighborhood | | | | | |
| I8 | unknown route 404 | | | | | |
| I9 | mcp graceful unindexed | | | | | |
| I10 | mcp valid handshake | | | | | |

**Totals:** ~90 case-checks each — Agent1 / Agent2 / Agent3 all PASS the core
engine; 7 distinct defects FAILed unanimously; 0 BLOCKED.

---

## Execution Results — 3 independent agents (2026-05-25)

Binary under test: `codewiki 0.1.0`, sha256 `1d3a581e…` (all three agents verified
the same build, commit `25bcb06`). Workspaces `/tmp/qc{1,2,3}`, ports 7101-7103.

### Unanimously CONFIRMED defects (all 3 agents agree)

| ID | Defect | Severity | Evidence (concordant across agents) |
|----|--------|:--------:|--------------------------------------|
| C-CS-3 / D1 / E-ASP-1 | **C# `implements` = 0** for `class Greeter : IGreeter` and synthetic DI `UserService : IUserService` | High | `edges WHERE kind='implements'` empty in all three runs. (Real eShopOnWeb DI yielded 13, ripgrep Rust 116 — so the edge kind works; the C# base-list / synthetic-DI path doesn't emit the ref.) |
| C-CPP-1/2/3 | **C++ extraction near-broken** | High | Only class nodes + a stray `variable`; methods, free functions, `extends`, and all `calls` edges MISSING |
| H4 | **Unicode/Vietnamese identifiers stripped to ASCII** | High | `MáyTính`→`MayTinh`, `cộng`→`cong` in node names (hex/byte-verified, not display) |
| C-RS-4 | **Rust duplicate nodes** | Medium | each trait/impl fn emitted as BOTH `function` AND `method` (same line range); `calls` edge duplicated |
| C-JAVA-1/3 | **Java `interface` mis-kinded as `class`; `implements` = 0** | Medium | + duplicate `speak` node |
| C-TS-3 | **TypeScript `implements` = 0** | Medium | `class Dog implements Animal` → no implements edge |
| E-EXP-1 / E-DJ-1 / E-ASP-2 | **No `route` nodes on minimal synthetic samples** | Low | real `expressjs/express` clone DID produce 266 route nodes — resolver fires on real router patterns, not the minimal `app.get('/p', fn)` form |

### Confirmed WORKING (all 3 agents PASS)
init/index/uninit, git hooks, `--no-index`; Python/Rust/Go/JS/Vue + 9 "rest"
languages basic extraction + calls; full incremental sync incl. the
mtime-preserved-content-change case; all 9 MCP tools over JSON-RPC + CLI parity;
graph UI + all API routes; graceful `serve --mcp` in an unindexed dir (friendly
non-error message, no crash); robustness (empty repo, binary-ignore, 5k-fn file,
gitignore); real-repo scale (Flask 2110 / ripgrep 4987 / Express 2291 nodes).

### Clarified NON-defects (test-plan artifacts, not product bugs)
- `codewiki_node` takes arg **`symbol`** (not `name`/`id`); Agent1's apparent
  FAIL was the plan example using the wrong arg name → **doc fix**.
- F-MCP raw JSON-RPC needs the `notifications/initialized` message + slow stdin
  feed; the single-pipe helper closes stdin too early → **test-harness timing**.
- Go structural interface satisfaction → no `implements` edge: **design decision**.
- Angular/Vue `component` overlay node co-existing with the `class` node: by design.

**Discrepancies (non-unanimous):** none of substance — the only divergence was
the `codewiki_node` arg-name artifact above, resolved in favor of `symbol`.
