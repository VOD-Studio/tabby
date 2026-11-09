# Changelog

All notable changes to tabby are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-11-09

The `-with-mihomo` archives ship mihomo v1.19.33.

### Added

- ui: show the version in the help popup

## [0.1.0] - 2026-11-09

The `-with-mihomo` archives ship mihomo v1.19.33.

### Added

- tui: init and restore terminal with panic hook
- app: add event loop with tokio select and tick
- app: add Action/Msg dispatch and quit handling
- config: load profiles from config.toml
- cli: add clap args with env/config override precedence
- log: write tracing logs to state dir
- api: add ApiClient with auth and no_proxy
- api: add error type and HTTP status mapping
- api: add version and configs endpoints
- ui: add semantic theme with terminal palette
- ui: add tokyo-night and catppuccin-mocha themes
- ui: add header with core badge, mode pill and status
- ui: add tab bar and page routing
- ui: add contextual key hints footer
- ui: add toast notifications
- ui: add popup
- ui: add help overlay
- ui: add loading and error states
- ui: add disconnected banner
- api: add proxies models and endpoints
- ui: add empty state
- proxies: render group list panel
- proxies: render node card grid with delay colors
- proxies: select node in selector group
- proxies: test delay for node and group with spinner
- proxies: sort and filter nodes
- proxies: 2D navigation in node grid
- app: page by half viewport
- proxies: distinguish failed and timed-out delay tests
- proxies: lead with GLOBAL in global mode
- proxies: refetch proxies after select and group test
- proxies: natural sort by name
- api: add websocket subscriber with backoff reconnect
- overview: add traffic chart and mode switcher
- overview: add stat cards and core info
- connections: render live table with speed diff
- connections: sort, filter and detail popup
- connections: close connections with confirm dialog
- logs: stream logs with level filter and pause
- rules: list and filter rules
- providers: update and health check providers
- settings: patch configs and run maintenance actions
- app: switch profiles at runtime
- ui: responsive layout for narrow terminals
- ui: optional nerd font icons
- config: apply theme overrides
- ui: add stripe and bg colors and four built-in themes
- config: load user theme files by name or path
- ui: pick a theme at runtime and save it to config.toml
- app: mouse capture, hit map, tabs and wheel
- ui: click to select, double-click to activate
- ui: sort by clicked column and switch mode by pill
- ui: show mouse actions in the help overlay
- core: run a managed mihomo in the background
- core: download and update mihomo releases
- subscriptions: import, switch and update subscriptions
- settings: persist managed core settings
- core: authorize tun with setcap
- subscriptions: auto-update subscriptions
- core: start the managed core at login
- core: set system proxy on gnome and kde
- subscriptions: decrypt age-encrypted subscriptions
- subscriptions: rename, set the interval and change the age key
- settings: edit the managed core's dns overrides
- core: run a mihomo found on path
- core: seed the kernel from the one shipped with tabby
- subscriptions: log why an import or update failed
- core: give up a subscription download when it goes quiet, and retry it
- subscriptions: show how much a download got and how fast
- settings: choose how subscriptions are downloaded
- cli: show the commit tabby was built from in --version

### Fixed

- ui: show localized error summary with raw detail
- proxies: sort timeouts before failures by delay
- app: refetch proxies when streams recover
- logs: show reconnecting while the log stream resubscribes
- proxies: include proxy-provider nodes
- ui: keep popup borders intact over wide characters
- ui: describe enter on the proxy group list in help
- ui: paint the cursor over drawn rows
- ui: repaint cells a cut wide character blanks
- subscriptions: say when a download waits or is checked, and name a failed proxy first

