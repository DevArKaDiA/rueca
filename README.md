# Telar

Code intelligence engine for coding agents. One core, three deployment modes:

- **Local** — single binary, indexes a working tree, MCP over stdio.
- **Service** — one container, snapshots in object storage, MCP over HTTP.
- **Org brain** — indexer workers + stateless query nodes over every repo in
  GitHub / GitLab / Bitbucket, with per-repo permissions mirrored from the host.

Local mode can overlay uncommitted changes on top of the org snapshot.

## Scope

In: source code and what lives in repositories (README, ADRs, OpenAPI, infra
manifests, diagrams). Pull requests and issues are a v2 candidate.

Out: Notion, Drive, and other document stores. Agents federate those through
their own MCP servers; Telar exposes stable symbol IDs
(`repo@sha:path#symbol`) so results can be cross-referenced.

## Parity contract

Telar is built from scratch. codebase-memory-mcp v0.12.0 is the reference
baseline: for every supported language, Telar must match or beat it on the
benchmark (localization, cross-service edges, structural questions) before it
is recommended as a replacement.

See `docs/decisions.md` and `docs/rfcs/`.
