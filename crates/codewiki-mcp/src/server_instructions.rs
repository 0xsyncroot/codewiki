//! T-410 — Static server instructions embedded in the MCP `initialize` response.
//!
//! Ported from `research-source/src/mcp/server-instructions.ts`.

/// Server-level instructions emitted in the MCP `initialize` response.
///
/// MCP clients (Claude Code, Cursor, opencode, …) surface this text in the
/// agent's system prompt automatically, giving the agent a high-level
/// playbook for the codewiki toolset before it sees individual tool
/// descriptions.
pub const SERVER_INSTRUCTIONS: &str = r#"# CodeWiki — code intelligence over an indexed knowledge graph

CodeWiki is a SQLite knowledge graph of every symbol, edge, and file
in the workspace. Reads are sub-millisecond; the index lags writes by
about a second through the file watcher. Consult it BEFORE writing or
editing code, not during.

## Answer directly — don't delegate exploration

For "how does X work", architecture, trace, or where-is-X questions,
answer DIRECTLY using 2-3 codewiki calls: `codewiki_context` first,
then ONE `codewiki_explore` for the source of the symbols it surfaces.
CodeWiki IS the pre-built search index — so delegating the lookup to a
separate file-reading sub-task/agent, or running your own grep + read
loop, repeats work codewiki already did and costs more for the same
answer. Reach for raw Read/Grep only to confirm a specific detail
codewiki didn't cover. A direct codewiki answer is typically a handful
of calls; a grep/read exploration is dozens.

## Tool selection by intent

- **"What is the symbol named X?"** → `codewiki_search`
- **"What's the deal with this task / feature / area?"** → `codewiki_context` (PRIMARY — composes search + node + callers + callees in one call)
- **"What calls this?"** → `codewiki_callers`
- **"What does this call?"** → `codewiki_callees`
- **"What would changing this break?"** → `codewiki_impact`
- **"Show me this symbol's source / signature / docstring."** → `codewiki_node`
- **"Show me several related symbols' source / survey an area."** → `codewiki_explore` (ONE capped call; prefer over many codewiki_node/Read)
- **"What's in directory X?"** → `codewiki_files`
- **"Is the index ready / what's its size?"** → `codewiki_status`

## Common chains

- **Onboarding**: `codewiki_context` first. If still unclear, `codewiki_explore` for breadth, then `codewiki_node` on specific symbols.
- **Refactor planning**: `codewiki_search` → `codewiki_callers` → `codewiki_impact`. The blast-radius answer comes from impact, not from walking callers manually.
- **Debugging a regression**: `codewiki_callers` of the suspected symbol; widen with `codewiki_impact` if an unexpected call appears.

## Anti-patterns

- **Don't grep first** when looking up a symbol by name — `codewiki_search` is faster and returns kind + location + signature.
- **Don't chain `codewiki_search` + `codewiki_node`** when you just want context — `codewiki_context` is one round-trip.
- **Don't loop `codewiki_node` over many symbols** — one `codewiki_explore` call returns them all grouped by file, while each separate call re-reads the whole context and costs far more. Use `codewiki_node` for a single symbol.
- **Don't query the index immediately after editing a file** — the watcher needs ~500ms to debounce + sync. Wait for the next turn.

## Limitations

- Index lags file writes by ~1 second.
- Cross-file resolution is best-effort name matching; ambiguous calls may return multiple candidates.
- No live correctness validation — that's still the TypeScript compiler / test suite / linter's job. CodeWiki supplements those with structural context they don't have.
"#;
