# github-core-clone-01 — a GitHub-compatible git forge

A single-process, stdlib-only (Node 22) implementation of a GitHub-compatible
git forge: git smart HTTP (protocol v2 + v0), REST v3, GraphQL v4 and MCP
streamable HTTP — all backed by one deterministic in-memory store.

## Layout

- `main.ts` — the entry <-> env lifecycle (starter, unchanged).
- `app.ts` — the HTTP server: routing, git smart HTTP, REST v3, GraphQL, MCP.
- `git.ts` — pkt-line framing, SHA-1 objects (commit/tree/blob/tag), pack
  read/write (no deltas, zlib level 1), fetch planning (depth/shallow/haves).
- `store.ts` — repositories, refs, issues, pull requests, labels, comments,
  contents, squash merge with 3-way conflict detection.
- `graphql.ts` — a small GraphQL parser + executor for the documents the
  suite sends (viewer, repository, node, __type, issues/pullRequests
  connections, create/close/merge/addComment mutations, batched operations).

## Notes

- No dependencies, no git binary, no network access at runtime; the only
  outbound request is `main.ts` talking to the env (`/register`, `/done`).
- Deterministic: server-made commits and ids come from fixed counters, never
  from the wall clock or randomness, so repeated runs export identical state.
- Performance: in-memory store, cached serialized JSON for hot REST reads,
  and a pack cache keyed by repo version + fetch parameters so identical
  clones reuse one built pack.

## Local check

```sh
node --experimental-strip-types main.ts   # exits 1 without MAKEFUN_ENV_URL
```

The local runner (in the kit) scores the entry against the sealed suite:

```sh
node --experimental-strip-types sdk/run.ts --url http://127.0.0.1:PORT --token test-token --perf --json
```
