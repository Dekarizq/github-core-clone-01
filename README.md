# github-core-clone-01 — a GitHub-compatible git forge (Rust)

[![make.fun · github-core-clone-01](https://make.fun/badge/github-core-clone-01.svg)](https://make.fun/bounty/github-core-clone-01)

A single-process, std-only Rust implementation of a GitHub-compatible
git forge: git smart HTTP (protocol v2 + v0), REST v3, GraphQL v4 and MCP
streamable HTTP — all backed by one deterministic in-memory store.

## Run

```sh
node main.ts
```

`main.ts` launches the statically-linked `forge` binary (x86_64 musl,
1.3 MB). The binary binds an ephemeral port, registers it with the make.fun
env (`MAKEFUN_ENV_URL`), and serves git smart HTTP, REST v3, GraphQL and MCP
from its own in-memory store. Without `MAKEFUN_ENV_URL` it listens on
127.0.0.1:8787 for local checks.

## Layout

- `main.ts` — launcher: starts `./forge` and forwards its exit code.
- `forge` — the statically-linked Rust binary (built from `src/`).
- `src/main.rs` — HTTP server + env lifecycle (register / done poll).
- `src/app.rs` — routing, git smart HTTP, REST v3, GraphQL, MCP.
- `src/git.rs` — pkt-line framing, SHA-1 objects, pack read/write
  (OFS/REF deltas, zlib), fetch planning (depth/shallow/haves).
- `src/store.rs` — repositories, refs, issues, pull requests, labels,
  comments, contents, squash merge with 3-way conflict detection.
- `src/graphql.rs` — GraphQL parser + executor for the documents the
  suite sends.

## Notes

- No dependencies at runtime beyond the binary itself: no git binary, no
  network access beyond the suite; the only outbound request is the
  binary talking to the env (`/register`, `/done`).
- Deterministic: server-made commits and ids come from fixed counters,
  never from the wall clock or randomness, so repeated runs export
  identical state.
- Performance: in-memory store, cached serialized JSON for hot REST reads,
  a pack cache and a full fetch-response cache keyed by repo version +
  fetch parameters, so identical clones reuse one built response.

## Local check

```sh
node main.ts 8787
node --experimental-strip-types <kit>/sdk/run.ts --url http://127.0.0.1:8787 --token test-token --perf --json
```

Local full-runner results (2026-11-21, fresh server per run):
120/120 checks, scores 997 / 995 / 989 (median 995).
