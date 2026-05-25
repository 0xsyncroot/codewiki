# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

CodeWiki is pre-1.0; security fixes land on the latest `0.1.x` release.

## Reporting a Vulnerability

Please **do not** open a public issue for security vulnerabilities.

Report privately via either:

- **GitHub Security Advisories** — open a draft advisory under the repository's
  *Security* tab (preferred), or
- **Email** — `work.hiepht@gmail.com`

Please include a description of the issue, steps to reproduce, the affected
version (`codewiki --version`), and the potential impact. We aim to acknowledge
reports within **72 hours** and will keep you informed as we investigate and
prepare a fix. We ask that you give us a reasonable window to release a fix
before any public disclosure.

## Security Posture

CodeWiki is designed to run entirely **locally**:

- It parses source code on your machine and stores the resulting index in a
  local SQLite database under `.codewiki/`.
- It makes **no network calls** during indexing, querying, or serving — there is
  no telemetry, no analytics, and no code or metadata leaves your machine.
- The optional graph web UI binds to `localhost` only.
- The MCP server communicates over local stdio with your AI agent.

Because the index can contain symbol names and code excerpts from your
repository, treat the `.codewiki/` directory with the same sensitivity as the
source it was built from.
