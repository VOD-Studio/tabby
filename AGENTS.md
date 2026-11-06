# Repository Guidelines

## Project Overview
`tabby` is a single-binary Rust TUI (ratatui + tokio) that controls a **mihomo** (Clash.Meta) kernel, with partial support for original Clash, via its external-controller REST + WebSocket API. Seven pages: Overview, Proxies, Providers, Rules, Connections, Logs, Settings. User-facing UI strings are **Chinese**; code, identifiers and doc comments are **English**. README and the plan are Chinese.

## Architecture & Data Flow
Elm-like, single-owner state:

```
crossterm EventStream ─┐
100 ms tick ───────────┼─ select! in event::run ─► action_for(key) ─► App::update ─► repaint
mpsc Envelope{epoch,Msg}┘                                           (Msg only sets `dirty`; next tick repaints)
        ▲
        │ Outbox  (bounded, MSG_BUFFER = 256)
 App::spawn → tokio::spawn(ApiClient call)      api::stream::Streams → one task per Stream
                                                (Traffic, Memory, Logs, Connections; backoff reconnect)
```

- `App` (`src/app.rs`) is the **only mutable state**, touched only by the main loop. Background work never mutates it; it sends a `Msg` through `Outbox`.
- `App::spawn()` clones `ApiClient`, runs the request in `tokio::spawn`, posts the result as `Msg`. `handle_msg` applies it.
- **Stale-reply protection**: every message is wrapped in `Envelope{epoch,msg}`; `receive()` drops mismatched epochs. `switch_profile` bumps the epoch and rebuilds `App` via `App::connect` (keeps theme/sort/log level). Out-of-order replies are dropped via `seq`/`applied_*` counters (e.g. `Msg::Configs`). Messages carry `refetch: bool` so a failed background refresh does not wipe data already on screen.
- Streams (`src/api/stream.rs`): reconnect with `api::backoff()` (1 s doubling, cap 30 s); `retry_now()` cuts waits via a `watch` channel; changing log level aborts/restarts the Logs task. Auth for WS is `?token=` (percent-encoded). **Layering inversion**: `api::stream` imports `crate::app::{Msg, Outbox}`.
- `ApiClient` (`src/api/client.rs`): thin `reqwest` wrapper; private `get_json/put_json/patch_json/post/delete`, one `pub async fn` per endpoint. Path segments are percent-encoded; `.no_proxy()` is set deliberately (env proxies are ignored). `mihomo`-only endpoints (e.g. group delay) are gated via `CoreKind`.
- Config precedence (`src/config.rs::resolve`): **CLI flag > `TABBY_*` env > `config.toml` > built-in default**; an empty override means unset. Parse errors in `config.toml` are hard errors before the TUI starts.
- Rendering (`src/ui/`): `draw` → `draw_screen` / `draw_too_small`; `regions()` splits header/tabs/banner/body/footer and is shared with paging logic; `draw_page` matches on `Page`; `ready()` handles loading/error panes. Responsive thresholds (cells) in `ui/mod.rs`: `NARROW_WIDTH=100`, `COMPACT_WIDTH=70`, `MIN_WIDTH×MIN_HEIGHT=50×15`, `CARDS_MIN_HEIGHT=28` (overview), `SIDE_MIN_WIDTH=24`.

## Key Directories
| Path | Purpose |
|---|---|
| `src/api/` | `client.rs` (REST), `stream.rs` (WS), `models.rs` (serde types, `Mode`, `CoreKind`), `error.rs` (`ApiError`), `mod.rs` (`backoff`) |
| `src/ui/` | One module per page (`overview`, `proxies`, `providers`, `rules`, `connections`, `logs`, `settings`), plus `theme.rs`, `icons.rs`, `shadow.rs`, `text.rs`, `scroll.rs` |
| `src/ui/components/` | Reusable widgets/overlays: `header`, `tabs`, `footer`, `banner`, `states`, `toast`, `popup`, `help`, `profiles`, `themes`, `confirm`, `title` |
| `tests/fixtures/` | Real-shaped JSON from mihomo/clash (`mihomo_*.json`, `clash_*.json`); loaded with `include_str!` |
| `docs/plans/` | Implementation plan `2026-11-05-tabby-tui.md` (milestones M0–M6, numbered items, decision records) and SVG assets |

## Development Commands
```bash
cargo build                                   # debug
cargo build --release                         # target/release/tabby
cargo run -- --url http://127.0.0.1:9090 --secret xxx
cargo fmt --check                             # default rustfmt, no config file
cargo clippy --all-targets -- -D warnings     # REQUIRED gate (pedantic is on)
export XDG_CONFIG_HOME="$(mktemp -d)"; cargo test
cargo install --path .
RUST_LOG=tabby=debug tabby                    # log file: $XDG_STATE_HOME/tabby/tabby.log (fallback: data_local_dir)
```
CLI flags (each also an env var): `--profile/TABBY_PROFILE`, `--url/TABBY_URL` (default `http://127.0.0.1:9090`), `--secret/TABBY_SECRET`, `--theme/TABBY_THEME`. `--theme` pins the theme and is not persisted.

Per-commit gate: build + clippy `-D warnings` + test must all pass. `make check` runs fmt-check + clippy + test (tests under a throwaway `XDG_*`); `make help` lists targets.

**Commit after every completed feature point.** One feature/fix = one commit, made as soon as it is done and the gate above passes — never batch several feature points into one commit, and never leave finished work uncommitted. Use a Conventional Commit message (e.g. `feat(connections): sort, filter and detail popup`). Stage only the files for that feature point.

## Code Conventions & Common Patterns
- Edition 2024 (let-chains used). `unsafe_code = "forbid"`. `clippy::pedantic = warn`, `module_name_repetitions` allowed. Justify any `#[allow(...)]` per-site with a comment (e.g. `cast_precision_loss`, `result_large_err`).
- `too_many_lines` (100) bites `handle_msg`/`update`: split into helpers instead of allowing.
- Bin crate: an unused fn fails `-D warnings` (dead_code). Land a helper in the same commit as its first caller.
- Errors: `color_eyre` at the `main` boundary; `thiserror` `ApiError` (`Unauthorized`, `NotFound`, `Http{status,message}`, `Timeout`, `Connect`, `Decode`) below. `ui/text.rs::describe(&ApiError)` converts to Chinese user text shown as toasts/error panes. No `unwrap` on I/O paths.
- Never log to stdout/stderr — the TUI owns it. Use `tracing` (file appender). `ratatui::restore()` always runs; panic hook logs, then delegates.
- All symbols/glyphs come from `ui/icons.rs::Icons` (unicode/nerd sets); never inline glyphs in pages.
- Colors come from `ui::theme::Theme` semantic fields (`fg muted surface stripe bg accent success warning error c1..c4`); never hardcode `Color::*` in pages.
- Width math: use `unicode-width` and `format::truncate` (wide-char aware, `…`). `ui/shadow.rs` repaints cells blanked by a cut wide char — popups must keep borders intact over CJK.
- Scrolling: `ui::scroll::Scroll` stores offset in a `Cell` so render can update it (`window(selected, len, capacity)`).
- Page renderers: `pub fn render(frame, area, app, …)` in `ui/<page>.rs`; formatting helpers in `src/format.rs` (`rate`/`bytes` return `(number, unit)` tuples).
- Doc comments often cite plan sections (`plan §7.x`); keep them accurate when behavior changes.
- Text-input filters use `tui-input`; in typing mode every key is text, `Ctrl+C` quits, `Esc` clears, `Enter` accepts.
- Config writes use `toml_edit` to change only the targeted key and preserve comments (`config::save_theme`).

### Adding things
- **Page**: `Page` variant + `Page::ALL` + `page_title`, `ui/<page>.rs`, `draw_page` arm, key handling in `action_for` (page-gated `matches!`), `help::page_keys`, spinner in `is_loading`/`on_tick`, lazy load in `set_page` if needed.
- **Built-in theme**: `ThemeName` variant + `id()` + `ALL` + `From<ThemeName> for Theme`. User themes (`<config_dir>/themes/<id>.toml`, `extends = "<builtin>"` + `[colors]`) need no code.
- **Endpoint**: `pub async fn` on `ApiClient`, serde model in `models.rs`, a `Msg` variant, a `spawn` call site, wiremock test.

## Important Files
- `src/main.rs` — CLI parse, logging, config → `resolve` → `themes::load` → `ApiClient::new` → `App::new` → `event::run`.
- `src/app.rs` (~7.6k lines, ~half is tests) — `Page`, `Action`, `Msg`, `Envelope`, `Outbox`, `action_for`, `App`, `handle_msg`, `on_tick`, `set_page`, `switch_profile`. Most state logic lives here.
- `src/event.rs` — main loop (`TICK` 100 ms).
- `src/config.rs` — `Config`, `Overrides`, `resolve`, `Palette`/`[theme_overrides]`, `IconSet`, `parse_color` (`#rrggbb`, 16 ANSI names, `reset`).
- `src/themes.rs` — user theme file loading/scanning.
- `Cargo.toml` — lints, deps. Config at `ProjectDirs("", "", "tabby")` → `config.toml`, `themes/`.
- `docs/plans/2026-11-05-tabby-tui.md` — design rationale, risks R1–R7; its §4 file tree is stale (omits `themes.rs`, `ui/icons.rs`, `shadow.rs`, `text.rs`, `scroll.rs`, `components/{themes,profiles}.rs`).

## Runtime/Tooling Preferences
- Rust stable, edition 2024 (dev toolchain 1.99; no `rust-version`, no `rust-toolchain`). Cargo + a thin `Makefile` wrapping the commands above; no justfile/CI/scripts.
- Async runtime: tokio multi-thread. TLS: rustls only (reqwest, tokio-tungstenite) — do not add OpenSSL.
- Commits: Conventional Commits, English, lowercase, imperative, no trailing period; scopes like `app`, `api`, `ui`, `config`, `proxies`, `logs`, `connections`, `settings`. Docs follow features with `docs: record M<n> <area> decisions`. Work on `main`; start from a clean tree.
- **Never touch `~/.config/tabby`**. Any run that may read/write config must set a temp `XDG_CONFIG_HOME`. Manual checks must target a local throwaway mihomo (127.0.0.1, DIRECT-only), never the user's live kernel.

## Testing & QA
- Inline `#[cfg(test)] mod tests` per module (no `tests/*.rs` integration crate). Frameworks: built-in `#[test]`/`#[tokio::test]`, `wiremock` (REST), real `TcpListener::bind("127.0.0.1:0")` + `accept_hdr_async` (WS), tokio `test-util` (`#[tokio::test(start_paused = true)]`), ratatui `TestBackend`. No snapshot crates.
- Current baseline (verified): `cargo fmt --check` clean, clippy `-D warnings` clean, **232 passed, 1 ignored**.
- App tests: `app_without_controller()` targets closed `http://127.0.0.1:1`; build apps with `launch(...)`/`app_with_proxies()`; inject via `handle_msg`/`key_press`; assert on `screen_text(app, w, h)` (renders to `TestBackend`, substring/exact checks). Test names are long descriptive snake_case sentences (`a_screen_too_small_only_says_so_and_keys_still_work`).
- Config isolation is by construction: pass `Overrides{url: Some(..)}`, `Picker::default()`, or `Config::load(Some(&path))` on a scratch dir (`scratch(name)` → `temp_dir()/tabby-{pid}-{name}`; duplicated in `config.rs`/`themes.rs`/`app.rs`). Still export a temp `XDG_CONFIG_HOME`.
- The one `#[ignore]` test (`api::client::tests::proxy_env_child`) is a subprocess child of `ignores_proxy_environment` (needed because `unsafe_code = "forbid"` blocks in-process `set_var`). Don't run `--ignored` blindly.
- Tests must catch real defects (behavior, boundaries, ordering, errors) — not wiring, defaults, or copies (plan §9). Add tests with every fix/feature; keep them deterministic.
- Known gaps: no direct tests for `event.rs`, `ui/mod.rs`, `api/error.rs`, `ui/proxies.rs`, `ui/scroll.rs`, several components; XDG path resolution untested.
- UI changes: also eyeball at 80×24 and 60×20 (and below 50×15 for the too-small screen).
