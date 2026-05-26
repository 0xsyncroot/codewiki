<!-- CODEWIKI_START -->
## CodeWiki

This project has a CodeWiki MCP server (`codewiki_*` tools) configured. CodeWiki is a tree-sitter-parsed knowledge graph of every symbol, edge, and file. Reads are sub-millisecond and return structural information grep cannot.

### When to prefer codewiki over native search

Use codewiki for **structural** questions — what calls what, what would break, where is X defined, what is X's signature. Use native grep/read only for **literal text** queries (string contents, comments, log messages) or after you already have a specific file open.

| Question | Tool |
|---|---|
| "Where is X defined?" / "Find symbol named X" | `codewiki_search` |
| "What calls function Y?" | `codewiki_callers` |
| "What does Y call?" | `codewiki_callees` |
| "What would break if I changed Z?" | `codewiki_impact` |
| "Show me Y's signature / source / docstring" | `codewiki_node` |
| "Give me focused context for a task/area" | `codewiki_context` |
| "See several related symbols' source at once" | `codewiki_explore` |
| "What files exist under path/" | `codewiki_files` |
| "Is the index healthy?" | `codewiki_status` |

### Rules of thumb

- **Answer directly — don't delegate exploration.** For "how does X work" / architecture / trace questions, answer with 2-3 codewiki calls: `codewiki_context` first, then ONE `codewiki_explore` for the source of the symbols it surfaces. CodeWiki IS the pre-built index, so spawning a separate file-reading sub-task/agent — or running a grep + read loop — repeats work codewiki already did and costs more for the same answer.
- **Trust codewiki results.** They come from a full AST parse. Do NOT re-verify them with grep — that's slower, less accurate, and wastes context.
- **Don't grep first** when looking up a symbol by name. `codewiki_search` is faster and returns kind + location + signature in one call.
- **Don't chain `codewiki_search` + `codewiki_node`** when you just want context — `codewiki_context` is one call.
- **Don't loop `codewiki_node` over many symbols** — one `codewiki_explore` call returns several symbols' source grouped in a single capped call, while each separate node/Read call re-reads the whole context and costs far more.
- **Index lag**: the file watcher debounces ~500ms behind writes; don't re-query immediately after editing a file in the same turn.

### If `.codewiki/` doesn't exist

The MCP server returns "not initialized." Ask the user: *"I notice this project doesn't have CodeWiki initialized. Want me to run `codewiki init` to build the index?"*
<!-- CODEWIKI_END -->
