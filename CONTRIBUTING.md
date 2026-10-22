# Contributing to Catenga Focus

Thanks for your interest! This guide explains what kind of changes fit the project, how to set it up, and what a pull request needs to be merged.

## Before you start

Catenga Focus is deliberately small. It does a few things — time blocks, permanent notes, a timer, metrics on top of Google Calendar — and tries to do them well.

- **Bug fixes, tests, docs, accessibility and performance:** just open a pull request.
- **New features or behaviour changes:** open an issue first and describe the problem you want to solve. Features are planned in milestones (see [`CLAUDE.md`](CLAUDE.md)); good ideas that don't fit yet go to [`IDEAS.md`](IDEAS.md).
- **Not planned for v1** (please don't send PRs for these): task lists / to-dos, integrations other than Google Calendar, auto-scheduling or AI features, daily planning wizards, a mobile app, sharing or teams.

## Setup

1. Install [Rust](https://rustup.rs) (stable), the [Tauri 2 prerequisites](https://tauri.app/start/prerequisites/), Node.js 22 and pnpm 8.
2. Fork and clone the repository, then:

   ```sh
   pnpm install
   CATENGA_NOTES_DIR=$(mktemp -d) pnpm dev
   ```

   Using a temporary notes folder keeps your real `~/Catenga` notes safe while you develop.
3. Optional pieces:
   - **Google Calendar:** create your own Google OAuth client of type "Desktop app" and export `CATENGA_GOOGLE_CLIENT_ID` and `CATENGA_GOOGLE_CLIENT_SECRET`.
   - **Account server:** `pnpm server:dev` runs it locally with SQLite and prints magic links to the console; start the app with `CATENGA_SERVER_URL=http://localhost:8080`.

## Making a change

1. Create a branch from `main`.
2. **Write the test first**, see it fail, then make it pass. Pure logic lives in small modules that are easy to test (`apps/desktop/src/lib/*` with Vitest, Tauri-free Rust modules with `#[cfg(test)]`).
3. Keep the change focused: one fix or feature per pull request.
4. Run every check — these are exactly what CI runs:

   ```sh
   pnpm typecheck && pnpm test
   mkdir -p apps/desktop/dist
   cargo fmt --all --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   ```

5. Open the pull request with what changed, why, and how you tested it (screenshots for UI changes).

## Rules that protect users' data

These are not style preferences; PRs that break them can't be merged.

- **Never lose user text.** Writes to notes are atomic (temp file + rename). Malformed headings are kept as plain text, never deleted.
- **The markdown files are the source of truth for notes.** The search index (`index.sqlite`) must stay rebuildable from them. The app database (`catenga.sqlite`) is a source of truth: schema changes are forward migrations that back up the file first — never reset it.
- **The day-file format** (`## HH:MM–HH:MM · Title #tag`) needs parser/serializer tests, including round-trip (parse → serialize → identical file) and malformed input.
- **The app must keep working offline and without an account.**
- **No secrets in the repo.** Never commit `.env` files, tokens or keys. Google tokens stay in the OS keychain on the user's computer.

## Code conventions

- **TypeScript:** strict mode; no `any` without a comment explaining why.
- **Where code lives:** file I/O, day-file parsing, the timer, Google Calendar and SQLite are Rust; the frontend calls them through Tauri commands.
- **Dependencies:** don't add one without a clear reason in the PR description. Animations and charts are CSS/SVG.
- **UI:**
  - Colors come from the CSS variables in `apps/desktop/src/theme.css` — never raw hex in components.
  - Icons are [Heroicons](https://heroicons.com) (`@heroicons/react`, 20/solid).
  - CSS is global, so every class uses a block prefix unique to its component (`focus__…`, `dash__…`).
  - Respect `prefers-reduced-motion`.
- **Comments:** explain *why*, not *what*. Match the density of the surrounding code.
- **Commits:** short and imperative, mentioning the milestone when there is one, e.g. `M6: discard sessions under a minute`.

## Reporting bugs

Open an issue with your OS and app version, what you did, what you expected and what happened. If the bug touches your notes, please include a minimal day file that reproduces it (with private text removed).

**Security issues:** please don't open a public issue — use GitHub's *Report a vulnerability* (Security tab) so it can be fixed before it's disclosed.

## License

By contributing, you agree that your contributions are licensed under the [Apache License 2.0](LICENSE), the same license as the project.
