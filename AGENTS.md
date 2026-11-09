# Repository Guidelines

## Project Overview
`tabby` is a single-binary Rust TUI (ratatui + tokio) that controls a **mihomo** (Clash.Meta) kernel, with partial support for original Clash, via its external-controller REST + WebSocket API. Eight pages: Overview, Proxies, Providers, Rules, Connections, Logs, Settings, Subscriptions. It can also **run the kernel itself** (a "managed core", enabled by a `[core]` table in `config.toml`): download/update, systemd user unit or plain-process fallback, subscriptions, TUN authorization via `setcap`, login start, desktop system proxy (plan `docs/plans/2026-11-06-managed-core.md`, M7–M9). Kernel lookup: `[core].binary` → `<data>/core/bin/mihomo` (downloaded, or seeded from the kernel shipped at `<prefix>/lib/tabby/mihomo`) → `mihomo` on `$PATH` → ask and download. User-facing UI strings are **Chinese**; code, identifiers and doc comments are **English**. README and the plan are Chinese.

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
- **Managed core** (`src/core/`, state side in `src/app/managed.rs`, `src/app/subscriptions.rs`): `ManagedCore` (an `Arc` in `Profiles.core`) owns everything on disk and every external program; the app never runs a program itself. It is spawned like any other work: `App::spawn_core`/`spawn_subscription` run a future and post `Msg::Core(CoreEvent)` / `Msg::Subscription(SubEvent)`; `CorePhase` (`Idle/Starting/Running/Failed/Stopped`) gates the pages (`controller_up`). Every program (`systemd-run`, `systemctl`, `getcap`, `pkexec`, `sudo`, `gsettings`, `kwriteconfig`, `mihomo -t`, …) goes through the `Runner` trait (`core/runner.rs`) so tests record arguments. `src/event.rs::lend_terminal` hands the terminal to `sudo` (drops the `EventStream`, `ratatui::restore()`, re-inits after).
- Config precedence (`src/config.rs::resolve`): **CLI flag > `TABBY_*` env > `config.toml` > built-in default**; an empty override means unset. Parse errors in `config.toml` are hard errors before the TUI starts.
- Rendering (`src/ui/`): `draw` → `draw_screen` / `draw_too_small`; `regions()` splits header/tabs/banner/body/footer and is shared with paging logic; `draw_page` matches on `Page`; `ready()` handles loading/error panes. Responsive thresholds (cells) in `ui/mod.rs`: `NARROW_WIDTH=100`, `COMPACT_WIDTH=70`, `MIN_WIDTH×MIN_HEIGHT=50×15`, `CARDS_MIN_HEIGHT=28` (overview), `SIDE_MIN_WIDTH=24`.

## Key Directories
| Path | Purpose |
|---|---|
| `src/api/` | `client.rs` (REST), `stream.rs` (WS), `models.rs` (serde types, `Mode`, `CoreKind`), `error.rs` (`ApiError`), `mod.rs` (`backoff`) |
| `src/core/` | The managed core: `mod.rs` (`ManagedCore`, `Paths`, `CoreError`, kernel lookup incl. `$PATH`), `supervisor.rs` (systemd unit / process fallback), `install.rs` (GitHub release download, sha256, `-v`; `seed` copies the shipped kernel in), `profiles.rs` + `subscription.rs` (import/update/switch, `Store`), `encrypted.rs` (age), `runtime.rs` (byte-preserving `config.yaml` generation, `saphyr-parser`), `settings.rs` (`settings.toml`), `privilege.rs` (TUN `setcap`), `autostart.rs` (login-start unit file), `sysproxy.rs` (gsettings/kwriteconfig), `state.rs`, `files.rs` (`write_private`, 0600), `runner.rs`, `testing.rs` (test seams), `tests.rs` |
| `src/app/` | `App` methods split out of `app.rs`: `managed.rs` (`CorePhase`, `CoreEvent`, start/stop/authorize/login start/system proxy), `dns.rs` (`DnsView`, the DNS-overrides popup of the settings page; `ManagedCore::set_dns` checks with `mihomo -t` before keeping a value), `subscriptions.rs` (`Ask`/`Prompt` for the one input box at the foot: import, key, rename, interval; `SubscriptionsView`, `Work`, `SubEvent`, auto-update timer) |
| `src/ui/` | One module per page (`overview`, `proxies`, `providers`, `rules`, `connections`, `logs`, `settings`, `subscriptions`), plus `theme.rs`, `icons.rs`, `shadow.rs`, `text.rs`, `scroll.rs` |
| `src/ui/components/` | Reusable widgets/overlays: `header`, `tabs`, `footer`, `banner`, `states`, `toast`, `popup`, `help`, `profiles`, `themes`, `confirm`, `dns`, `title` |
| `tests/fixtures/` | Real-shaped JSON from mihomo/clash (`mihomo_*.json`, `clash_*.json`); loaded with `include_str!` |
| `docs/plans/` | Implementation plan `2026-11-05-tabby-tui.md` (milestones M0–M6, numbered items, decision records) and SVG assets |
| `scripts/`, `packaging/` | `dist.sh` (`make dist`): release archives with/without mihomo, mihomo source tarball, `SHA256SUMS` into `dist/`; refuses a `v*`-tagged HEAD whose tag ≠ `Cargo.toml` version or whose tree is dirty, warns on an untagged (snapshot) HEAD. `release.sh` (`make cut-release VERSION=x.y.z`): clean tree → `make check` → bump `Cargo.toml`/`Cargo.lock` → git-cliff prepends to `CHANGELOG.md` (`cliff.toml`) → commit `chore(release): vX.Y.Z` → annotated tag → `dist.sh`; never pushes. `mihomo.env` pins the shipped mihomo tag + sha256 per arch; `about.{toml,hbs}` for `cargo-about` (`THIRD-PARTY-LICENSES`) |

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

Per-commit gate: build + clippy `-D warnings` + test must all pass. `make check` runs fmt-check + clippy + test (tests under a throwaway `XDG_*`); `make help` lists targets. `make dist` (needs `cargo-about`: `cargo install --locked --features cli cargo-about`) builds release archives into `dist/` (gitignored; downloads cached in `target/dist-cache/`). `make cut-release VERSION=x.y.z` also needs `git-cliff` (`cargo install --locked git-cliff`; `GIT_CLIFF=<path>` overrides); without `VERSION` it prints git-cliff's suggestion.

Versioning: SemVer, version only in `Cargo.toml`, tags `vX.Y.Z`. While 0.x, breaking changes and features bump minor, fixes bump patch (`cliff.toml [bump]`). Breaking commits use `type!:`. `CHANGELOG.md` is generated at release time (feat → Added, fix → Fixed, perf and other breaking → Changed; other types omitted); never re-generate it wholesale (earlier sections may be hand-edited).

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
- `build.rs` — sets `TABBY_LONG_VERSION` (`0.1.0 (<hash>[-dirty] <date>)`, plain version outside tabby's own git checkout) for `--version` and the startup log line; `-V` stays `CARGO_PKG_VERSION`. Reruns on `src`, `build.rs`, `Cargo.toml`, `Cargo.lock`, `HEAD` and its reflog; `-dirty` = `git status` non-empty for those paths. No new deps.
- `src/app.rs` (~10.8k lines, ~2/3 is tests) — `Page`, `Action`, `Msg`, `Envelope`, `Outbox`, `action_for`, `App`, `handle_msg`, `on_tick`, `set_page`, `switch_profile`. Most state logic lives here.
- `src/event.rs` — main loop (`TICK` 100 ms).
- `src/config.rs` — `Config`, `Overrides`, `resolve`, `Palette`/`[theme_overrides]`, `IconSet`, `parse_color` (`#rrggbb`, 16 ANSI names, `reset`).
- `src/themes.rs` — user theme file loading/scanning.
- `Cargo.toml` — lints, deps, `license = "MIT"` (`LICENSE`). Config at `ProjectDirs("", "", "tabby")` → `config.toml`, `themes/`. **Data dir** (`ProjectDirs::data_local_dir()`, `$XDG_DATA_HOME/tabby`) → `core/{bin/mihomo, bin/mihomo.shipped, home/config.yaml, home/providers/, profiles/<id>.yaml, subscriptions.toml, settings.toml, state.toml, system-proxy.toml, core.log}` (`mihomo.shipped`: size + mtime of the shipped kernel `bin/mihomo` was copied from; absent = downloaded, never overwritten by the shipped one); files with secrets (`config.yaml`, `state.toml`, `subscriptions.toml`) are 0600 via `files::write_private`. tabby-written state lives there, never in `config.toml`.
- New deps for the managed core: `flate2` + `sha2` (kernel download), `saphyr-parser` (locate top-level YAML keys; the subscription is never re-serialized), `base64`, `age` (X25519 + armor; no OpenSSL), `jiff`.
- `docs/plans/2026-11-06-managed-core.md` — managed-core design, risks R1–R10, §15 commit items with per-item implementation records (what was verified live and what was not).
- `docs/plans/2026-11-05-tabby-tui.md` — design rationale, risks R1–R7; its §4 file tree is stale (omits `themes.rs`, `ui/icons.rs`, `shadow.rs`, `text.rs`, `scroll.rs`, `components/{themes,profiles}.rs`).

## Runtime/Tooling Preferences
- Rust stable, edition 2024 (dev toolchain 1.99; no `rust-version`, no `rust-toolchain`). Cargo + a thin `Makefile` wrapping the commands above; `scripts/dist.sh` for release archives, `scripts/release.sh` to cut a release; no justfile/CI.
- Async runtime: tokio multi-thread. TLS: rustls only (reqwest, tokio-tungstenite) — do not add OpenSSL.
- Commits: Conventional Commits, English, lowercase, imperative, no trailing period; scopes like `app`, `api`, `ui`, `config`, `proxies`, `logs`, `connections`, `settings`. Docs follow features with `docs: record M<n> <area> decisions`. Work on `main`; start from a clean tree.
- **Never touch `~/.config/tabby`**. Any run that may read/write config must set a temp `XDG_CONFIG_HOME` (and `XDG_DATA_HOME`/`XDG_STATE_HOME` for the managed core). Manual checks must target a local throwaway mihomo (127.0.0.1, DIRECT-only, a non-default mixed port), never the user's live kernel or its ports (9090/7890).
- Managed-core manual checks: unit names carry a random suffix; afterwards stop them (`systemctl --user list-units 'tabby-mihomo-*' --all` must be empty), remove any unit file you wrote under `~/.config/systemd/user` (symlink the temp `XDG_CONFIG_HOME/systemd` there only for a login-start check) and `daemon-reload`. Never `tmux kill-server` (the user lives in tmux): use named sessions and `kill-session`. Use `GSETTINGS_BACKEND=memory` for gsettings and a temp `XDG_CONFIG_HOME` for `kioslaverc`. **TUN / `setcap` / `pkexec` / `sudo` live tests change routes and need the user's consent first** (FlClash may be running TUN); never ask for or type a password.

## Testing & QA
- Inline `#[cfg(test)] mod tests` per module (no `tests/*.rs` integration crate). Frameworks: built-in `#[test]`/`#[tokio::test]`, `wiremock` (REST), real `TcpListener::bind("127.0.0.1:0")` + `accept_hdr_async` (WS), tokio `test-util` (`#[tokio::test(start_paused = true)]`), ratatui `TestBackend`. No snapshot crates.
- Current baseline (verified): `cargo fmt --check` clean, clippy `-D warnings` clean, **413 passed, 1 ignored**.
- Managed-core tests never run real programs or touch the host: `core::testing` has `Recorder` (a `Runner` that records `Cmd`s and answers from a closure; `.without("prog")` = not found), `scratch`, `free_port`, `serve` (a stand-in mihomo controller thread), `core_at(dir, port, &recorder, systemd)` (seams `mountinfo`, `unit_dir`, `desktop` point at temp values; `search_path` and `shipped` are `None`, set them per test), and `core_on_nosuid_mount`. App-level helpers in `app.rs` tests: `managed_app`, `managed_settings_on`, `pump_until`, `subscriptions_app`, `updating_systemd`, `tun_machine`, `toasted`, `type_text`. `#[test]` fake kernels are `#!/bin/sh` scripts in the temp dir. Subscription servers are `wiremock`.
- App tests: `app_without_controller()` targets closed `http://127.0.0.1:1`; build apps with `launch(...)`/`app_with_proxies()`; inject via `handle_msg`/`key_press`; assert on `screen_text(app, w, h)` (renders to `TestBackend`, substring/exact checks). Test names are long descriptive snake_case sentences (`a_screen_too_small_only_says_so_and_keys_still_work`).
- Config isolation is by construction: pass `Overrides{url: Some(..)}`, `Picker::default()`, or `Config::load(Some(&path))` on a scratch dir (`scratch(name)` → `temp_dir()/tabby-{pid}-{name}`; duplicated in `config.rs`/`themes.rs`/`app.rs`). Still export a temp `XDG_CONFIG_HOME`.
- The one `#[ignore]` test (`api::client::tests::proxy_env_child`) is a subprocess child of `ignores_proxy_environment` (needed because `unsafe_code = "forbid"` blocks in-process `set_var`). Don't run `--ignored` blindly.
- Tests must catch real defects (behavior, boundaries, ordering, errors) — not wiring, defaults, or copies (plan §9). Add tests with every fix/feature; keep them deterministic.
- Known gaps: no direct tests for `event.rs`, `ui/mod.rs`, `api/error.rs`, `ui/proxies.rs`, `ui/scroll.rs`, several components; XDG path resolution untested.
- UI changes: also eyeball at 80×24 and 60×20 (and below 50×15 for the too-small screen).
