# Repository Guidelines

## Project Structure & Module Organization
- Workspace root contains `Cargo.toml`, `README.md`, and contributor docs.
- Core crates live under `src/`: `src/trx-core`, `src/trx-server`, and `src/trx-client`.
- Server backends are under `src/trx-server/trx-backend` (example: `trx-backend-ft817`).
- Client frontends are under `src/trx-client/trx-frontend` (HTTP, JSON, AppKit, rigctl).
- Examples live in `examples/` and static assets in `assets/`.
- Reference configs are `trx-server.toml.example` and `trx-client.toml.example`.

## Build, Test, and Development Commands
- `cargo build --release` builds optimized binaries.
- `cargo test` runs the workspace test suite.
- `cargo clippy` runs lint checks.
- Example server run (release build): `./target/release/trx-server -r ft817 "/dev/ttyUSB0 9600"`.

## Coding Style & Naming Conventions
- Rust standard style: 4-space indentation and rustfmt-compatible formatting.
- Naming: `snake_case` for modules/functions, `CamelCase` for types/traits, `SCREAMING_SNAKE_CASE` for constants.
- Prefer small, crate-focused commits; keep changes localized to the relevant crate.

## Testing Guidelines
- Tests are run via `cargo test` across the workspace.
- Add tests near the code they cover (module-level unit tests are preferred).
- If you change behavior in a crate, add or update tests in that crate.

## Commit & Pull Request Guidelines
- Commit title format: `[<type>](<crate>): <description>` (example: `[fix](trx-frontend-http): handle disconnect`).
- Use `(trx-rs)` for repo-wide changes that are not specific to any crate.
- Allowed types: `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`.
- Use imperative mood, keep lines under 80 chars, and separate body with a blank line.
- Sign commits with `git commit -s` and include `Co-authored-by:` for LLM assistance.
- Write isolated commits for each crate.
- Pull requests should include a clear summary, test status, and note any config or runtime changes.

## Contribution Workflow
- Fork the repository and create a new branch for your changes.
- Follow the project's coding style and conventions.
- Ensure changes are tested and pass existing tests.

## Configuration & Plugins
- Configs use TOML. See the example files for required sections and defaults.
- Plugins can be loaded from `./plugins`, `~/.config/trx-rs/plugins`, or `TRX_PLUGIN_DIRS`.
