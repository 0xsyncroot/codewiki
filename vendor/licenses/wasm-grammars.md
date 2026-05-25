# WASM Grammar Files — Source URLs and License Information

The following WebAssembly grammar files are bundled in `crates/codewiki-extraction/grammars/`
and embedded via `include_bytes!` when the `wasmtime-grammars` feature is enabled.

All four grammars are MIT-licensed. Embedding their compiled WASM binaries requires
reproducing the copyright notice and license text (satisfied by the `vendor/licenses/`
files referenced below).

---

## tree-sitter-lua.wasm

- **Source repository:** https://github.com/tree-sitter-grammars/tree-sitter-lua
  _(canonical successor to MunifTanjim/tree-sitter-lua — same grammar, active maintenance)_
- **Version / tag:** v0.5.0
- **Commit SHA:** `10fe0054734eec83049514ea2e718b2a56acd0c9`
- **Release date:** 2026-02-26
- **Retrieved:** 2026-05-24
- **Retrieval URL:** https://github.com/tree-sitter-grammars/tree-sitter-lua/releases/download/v0.5.0/tree-sitter-lua.wasm
- **SHA-256:** `df08a1704e504c70b8dba4a3e6f8e0c99a4fb94e1b1693d2969f53141d09f0d4`
- **File size:** 54603 bytes
- **ABI version:** 15 (compatible with tree-sitter 0.22–0.26+)
- **Symbol count:** 143
- **SPDX identifier:** MIT
- **License file:** `vendor/licenses/tree-sitter-lua-LICENSE`
- **Used by:** Lua language extraction (wasmtime backend)

### Key node types (verified present)
`function_declaration`, `function_call`, `variable_declaration`, `string`,
`string_content`, `identifier`, `dot_index_expression`, `method_index_expression`

---

## tree-sitter-luau.wasm

- **Source repository:** https://github.com/polychromatist/tree-sitter-luau
- **Version / tag:** no formal releases or tags; pinned to commit `71b03e6`
- **Commit SHA:** `71b03e66` (Dec 8 2025 — "add new tokens `<<` and `>>` to highlight queries")
- **Retrieved:** 2026-05-24 (built locally from source using `tree-sitter build --wasm`)
- **SHA-256:** `b2c35dffa5c5b013167748dda0b73075964b03204327f9192f037d231526cad2`
- **File size:** 469434 bytes
- **ABI version:** 14 (compatible with tree-sitter 0.22–0.26+)
- **Symbol count:** (complete grammar rewrite; substantially more coverage than previous 197-symbol build)
- **SPDX identifier:** MIT
- **License file:** `vendor/licenses/tree-sitter-luau-LICENSE`
- **Used by:** Luau language extraction (wasmtime backend)

### Build command
```
git clone https://github.com/polychromatist/tree-sitter-luau
cd tree-sitter-luau
git checkout 71b03e66
tree-sitter build --wasm -o tree-sitter-luau-71b03e6.wasm .
```
(tree-sitter CLI 0.26.9; no wasi-sdk required — tree-sitter-cli ships its own WASM toolchain)

### Why updated (2026-05-24)
The previous bundled WASM (94204 bytes, commit unknown) pre-dated a breaking node-type rename
in the polychromatist grammar.  The new grammar uses `local_fn_stmt`, `fn_stmt`, `call_stmt`,
`local_var_stmt`, and `type_stmt` instead of the old Lua-derived names.  The extractor in
`src/languages/luau.rs` has been fully rewritten as a standalone extractor aligned to these
new node names.

The JohnnyMorganz/tree-sitter-luau alternative continues to be rejected: it uses a GOT-based
WASM linkage model incompatible with wasmtime's `WasmStore::load_language`.

### Key node types (verified present in 71b03e6)
`local_fn_stmt`, `fn_stmt`, `call_stmt`, `local_var_stmt`, `type_stmt`,
`binding`, `bindinglist`, `explist`, `anon_fn`, `arglist`, `name`,
`string`, `key`, `field`

---

## tree-sitter-pascal.wasm

- **Source repository:** https://github.com/Isopod/tree-sitter-pascal
- **Version / tag:** v0.10.2
- **Commit SHA:** `042119eca2e18a60e56317fb06ee3ba5c32cb447`
- **Release date:** 2025-12-23
- **Retrieved:** 2026-05-24 (built locally using `tree-sitter build --wasm` with tree-sitter CLI 0.26.9 and wasi-sdk-29)
- **SHA-256:** `ef67d845bd3517eec56f4b9820db3b4cda15c4af3efb9b5fb6e3377152202ebc`
- **File size:** 716894 bytes
- **ABI version:** 14 (compatible with tree-sitter 0.22–0.26+)
- **Symbol count:** 371
- **SPDX identifier:** MIT
- **License file:** `vendor/licenses/tree-sitter-pascal-LICENSE`
- **Used by:** Pascal/DFM language extraction (wasmtime backend)

### Build command
```
tree-sitter build --wasm -o tree-sitter-pascal-v0.10.2.wasm /path/to/Isopod/tree-sitter-pascal@v0.10.2
```
(No pre-built WASM is published in GitHub releases for this grammar; built from source.)

### Key node types (verified present)
`declProc`, `declClass`, `declIntf`, `declEnum`, `declType`, `declUses`,
`exprCall`, `declField`, `declConst`

---

## tree-sitter-scala.wasm

- **Source repository:** https://github.com/tree-sitter/tree-sitter-scala
- **Version / tag:** v0.26.0
- **Commit SHA:** `2653eb5dcabaf655781bcd33890f076c724c7ed2`
- **Release date:** 2026-04-18
- **Retrieved:** 2026-05-24
- **Retrieval URL:** https://github.com/tree-sitter/tree-sitter-scala/releases/download/v0.26.0/tree-sitter-scala.wasm
- **SHA-256:** `026c2f9a8374109861f6621f4759ef690faebcaa67c2d56b06af3786c206b030`
- **File size:** 4951912 bytes
- **ABI version:** 15 (compatible with tree-sitter 0.22–0.26+)
- **Symbol count:** 357
- **SPDX identifier:** MIT
- **License file:** `vendor/licenses/tree-sitter-scala-LICENSE`
- **Used by:** Scala language extraction (wasmtime backend)

### Key node types (verified present)
`class_definition`, `object_definition`, `trait_definition`, `function_definition`,
`function_declaration`, `enum_definition`, `type_definition`, `import_declaration`,
`call_expression`, `val_definition`, `var_definition`, `enum_case_definitions`,
`simple_enum_case`, `full_enum_case`, `extension_definition`

---

## Notes on ABI Compatibility

tree-sitter 0.25/0.26 (wasmtime backend) supports grammar ABI versions **14 and 15**.
Grammars compiled with tree-sitter-cli >= 0.22 emit ABI 14 or 15. Older ABI 13 grammars
(e.g., the `tree-sitter-wasms` npm package's lua build from `@tree-sitter-lua` v2.1.3)
will fail to load.

All four bundled grammars have been verified ABI-compatible using Node.js WebAssembly
introspection (reading the `version` field from the language struct at the exported
`tree_sitter_<lang>()` pointer).

## Notes on tree-sitter-wasms npm package

The `tree-sitter-wasms@0.1.13` npm package only bundles Lua (from `@tree-sitter-lua`
v2.1.3) and Scala (from `tree-sitter-scala` v0.19.0) for our four languages. It does NOT
include Luau or Pascal. The Lua and Scala builds in that package are too old for ABI 14/15
requirements, so we vendor our own.
