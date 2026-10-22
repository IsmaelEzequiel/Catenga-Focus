# Catenga Focus

Desktop time blocking app: notes, timer and metrics on top of Google Calendar. See `CLAUDE.md` for the full spec and milestones.

## Requirements

- Rust (stable), Node 22, pnpm
- Platform dependencies for Tauri 2: https://tauri.app/start/prerequisites/

## Develop

```sh
pnpm install
pnpm dev                          # desktop app with hot reload
cargo run -p catenga-server       # server on :8080 (GET /health)
cargo test --workspace
```

## Build

```sh
pnpm build                                   # installers for the current OS
docker build -f server/Dockerfile -t catenga-server .
```

GitHub Actions builds installers for macOS (arm64 and x64), Windows and Linux on every push. Pushing a `v*` tag creates a draft release with them.
