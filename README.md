# spitfire

A Wayland compositor in Rust on top of [Smithay](https://github.com/Smithay/smithay),
reconfigurable at runtime like [Niri](https://github.com/YaLTeR/niri)/
[Hyprland](https://hyprland.org/), but with the single-file config philosophy of
[dwm](https://dwm.suckless.org/) — here in **Lua** instead of `config.h`. Four layout
modes: `tile` (master-stack), `floating`, `fibonacci` (spiral), and `monocle`.

spitfire has no opinion on frontend: point `spitfire.autostart` (in `config.lua`) at
whatever draws your bar/launcher/wallpaper/lockscreen — a wlr-layer-shell-v1 client is
all that's required. [Utumno](https://github.com/dani-77/utumno) is one example (a
QML/Quickshell shell with a bar, launcher, wallpaper picker, lockscreen, session menu,
OSD, and Ollama chat popup), not a dependency of this repo.

Full implementation plan at
`/home/dani77/.claude/plans/sparkling-shimmying-jellyfish.md`.

## Current status

- **Phase 0 (done)**: Cargo workspace skeleton + winit backend, adapted from Smithay's
  own [`anvil`](https://github.com/Smithay/smithay/tree/v0.7.0/anvil) example
  (MIT/Apache-2.0). `cargo run -p spitfire -- --winit` opens a nested window with basic
  xdg-shell and wlr-layer-shell working.
- **Phase 1 (done)**: `spitfire-layout` layout engine (tile/floating/fibonacci/
  monocle), pure and tested (`cargo test -p spitfire-layout`, 29 tests), wired into the
  compositor via `crates/spitfire/src/layout.rs` — real windows (`xdg_toplevel`) are
  positioned by the workspace's active layout.
- **Phase 2 (done)**: Lua config (`spitfire-config`, via `mlua`) — a single
  `config.lua`, dwm-`config.h`-style, reloadable at runtime (`spitfire.reload()`).
  `spitfire.bind`/`spawn`/`layout`/`mfact`/`nmaster`/`rule`/`autostart`/`gaps`/`border`
  are all live — see `examples/config.lua` for the default bindings (`Mod4+t/f/m` for
  tile/fibonacci/monocle, `Mod4+Shift+space` for floating, `Mod4+h/l`/`Mod4+i/d` for
  `mfact`/`nmaster`, `Mod4+Shift+q` to quit, `Mod4+Shift+r` to reload). Mod4/Super and
  Mod1/Alt are both first-class modifiers, freely mixable per bind. Window rules
  (`spitfire.rule({ app_id = ..., floating = true })`) exclude matching windows from
  tiling entirely. `spitfire.autostart` is how a frontend/shell gets launched.
  `spitfire.border` is parsed and stored but not yet rendered — that lands once
  `shell/ssd.rs`'s decorations get extended to draw it.
- **Phase 3 (done)**: control socket (`spitfire-ipc`, JSON lines at
  `$XDG_RUNTIME_DIR/spitfire.sock`, one request/response per connection) + the
  `spitfirectl` CLI (`reload`, `quit`, `layout <mode>`, `list-windows`,
  `list-outputs`). `spitfirectl reload` re-runs `config.lua` without
  restarting the compositor.
- **Phase 4 (done)**: wlr-layer-shell-v1 (already came wired from `anvil` — bars,
  launchers, wallpaper backgrounds; exclusive zones are subtracted from the tiling area,
  confirmed with a real Quickshell layer-shell client) + ext-session-lock-v1, implemented
  from scratch (`SessionLockHandler` in `shell/mod.rs`): while locked, rendering shows
  only the lock surface over an opaque black backdrop (`render.rs`) and keyboard focus is
  pinned to it (`update_keyboard_focus` in `input_handler.rs` refuses to steal it back).
  Verified against a real `swaylock` — lock request received, surface mapped, rendering
  switched. Known gap: pointer clicks aren't yet confined to the lock surface (no
  click-through *visually* possible since nothing else renders, but the events could
  still reach a hidden window) — left for a follow-up, since the protocol's
  security-critical path (keyboard/password) is solid. `spitfire.border` rendering is
  still open too.
- **Phase 5 (done)**: dynamic per-output workspaces (`crate::workspace`, niri-style —
  asking to focus workspace 5 when only 2 exist just creates 3, 4, and 5) advertised
  live over `ext-workspace-v1`, hand-implemented (`crate::ext_workspace`; not provided
  by Smithay, unlike Phase 4's protocols — see `NOTICE.md`). Each workspace owns its own
  tile/floating/fibonacci/monocle layout state. `spitfire.workspace.focus(n)` /
  `spitfire.workspace.move_window(n)` (1-based) + `spitfirectl workspace focus <n>` /
  `list-workspaces`. Verified end-to-end against a real Quickshell client using
  `Quickshell.WindowManager` (the same component Utumno's `Workspaces.qml` uses) in both
  directions: compositor-driven switches show up live in the client, and the client
  calling `activate()` on a workspace switches the compositor.
- **Phase 6 (done, one open finding)**: ran the real Utumno shell (`~/Projectos/utumno`,
  autostarted via `spitfire.autostart`) inside spitfire — no niri/Hyprland/Sway env vars
  present, so it correctly fell back to the generic `ext-workspace-v1` path (Phase 5's
  own protocol) rather than misdetecting a compositor. Loaded cleanly, no crashes, no QML
  errors. Confirmed via screenshot (`grim`, of the nested window inside the real host session):
  the backdrop, launcher/AI buttons, and workspace numbers all render correctly. **Open
  finding**: `modules/Bar.qml`'s center row (Weather+Clock) and right row (Cpu/Ram/Volume/
  Network/Battery/session button) overlap — traced to its own `Math.max`/`Math.min`
  collision-avoidance `x` binding (lines ~107–117) not accounting for the case where
  there isn't enough width for both rows side by side. Ruled out as a spitfire bug:
  reverted the config; forcing full-redraw every frame didn't change it (not a
  damage-tracking issue); a minimal ticking-clock layer-surface test with no such
  multi-row math renders perfectly, dynamic content included. This is a real, narrow-output
  Utumno UI bug, not a compositor one — the nested `--winit` test window (754px) is much
  narrower than a real display, which is what exposes it. Left for a Utumno-side fix (or
  a wider test), not tracked further here.
- **Out of scope for now**: the DRM/KMS backend (running as the real login session) and
  XWayland.

## Layout

```
crates/
├── spitfire/          # binary + compositor state (winit backend, xdg-shell, wlr-layer-shell)
├── spitfire-layout/    # layout engine, pure, no Wayland deps
├── spitfire-config/    # Lua config loader (mlua)
└── spitfire-ipc/       # control socket (JSON lines) + spitfirectl binary, no Wayland deps either
```

## Build & run

```sh
cargo build --workspace
cargo test --workspace             # 62 tests across the workspace, no Wayland needed
cargo run -p spitfire -- --winit   # or no arguments at all, --winit is the default
```

Needs `wayland-client`/`wayland-server`, `xkbcommon`, EGL, and GBM installed (dev
packages). Without a DRM/KMS backend, it only runs nested inside an existing graphical
session for now.

Config file: `$XDG_CONFIG_HOME/spitfire/config.lua` (falls back to
`~/.config/spitfire/config.lua`). See `examples/config.lua` for the default.
