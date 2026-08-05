<p align="center"><img src="assets/logo/spitfire-wc-icon-192.png" width="96" alt="spitfire logo"></p>

# spitfire

A Wayland compositor in Rust on top of [Smithay](https://github.com/Smithay/smithay),
reconfigurable at runtime like [Niri](https://github.com/YaLTeR/niri)/
[Hyprland](https://hyprland.org/), but with the single-file config philosophy of
[dwm](https://dwm.suckless.org/) — here in **Lua** instead of `config.h`, reloadable
without a recompile. Four layout modes: `tile` (master-stack), `floating`, `fibonacci`
(spiral), and `monocle`, each tracked independently per workspace.

spitfire has no opinion on frontend: point `spitfire.autostart` (in `config.lua`) at
whatever draws your bar/launcher/wallpaper/lockscreen — a wlr-layer-shell-v1 client is
all that's required. [Utumno](https://github.com/dani-77/utumno) is one example (a
QML/Quickshell shell with a bar, launcher, wallpaper picker, lockscreen, session menu,
OSD, and Ollama chat popup), not a dependency of this repo.

v1 is complete: config, IPC, workspaces, layer-shell/session-lock, and window borders
all implemented and verified against real clients (swaylock, Quickshell, the real Utumno
shell) — not just unit tests. See [What's implemented](#whats-implemented) below for
what that covers and [Known limitations](#known-limitations--pending-work) for what's
still open, and the full phase-by-phase implementation history at
`/home/dani77/.claude/plans/sparkling-shimmying-jellyfish.md`.

## What's implemented

- **Layouts** (`spitfire-layout`, pure Rust, no Wayland deps, 29 tests): `tile`
  (dwm master-stack), `floating`, `fibonacci` (spiral), `monocle`. Each workspace tracks
  its own mode/`nmaster`/`mfact`/gaps independently.
- **Config** (`spitfire-config`, via `mlua`): a single `config.lua`
  (`$XDG_CONFIG_HOME/spitfire/config.lua`, falling back to
  `~/.config/spitfire/config.lua`), reloadable at runtime with `spitfire.reload()` — no
  recompile. `spitfire.bind`/`spawn`/`layout`/`mfact`/`nmaster`/`workspace`/`rule`/
  `autostart`/`gaps`/`border`/`bar`. Mod4/Super and Mod1/Alt are both first-class modifiers,
  freely mixable per bind. See `examples/config.lua` for the default bindings.
- **Control socket** (`spitfire-ipc` + the `spitfirectl` CLI, JSON lines at
  `$XDG_RUNTIME_DIR/spitfire.sock`, one request/response per connection — niri
  msg/hyprctl-style): `reload`, `quit`, `layout <mode>`, `workspace focus <n>`,
  `list-windows`, `list-outputs`, `list-workspaces`.
- **Dynamic per-output workspaces** (`crate::workspace`, niri-style growth — focusing
  workspace 5 when only 2 exist just creates 3, 4, and 5), advertised live over
  `ext-workspace-v1` (`crate::ext_workspace`, hand-implemented — not provided by
  Smithay, see `NOTICE.md`). `spitfire.workspace.focus(n)` /
  `spitfire.workspace.move_window(n)` (1-based).
  - Verified end-to-end against a real Quickshell client (`Quickshell.WindowManager`,
    the same component Utumno's `Workspaces.qml` uses) in both directions:
    compositor-driven switches show up live in the client, and the client calling
    `activate()` switches the compositor.
- **wlr-layer-shell-v1** (came wired from Smithay's `anvil` example) — bars, launchers,
  wallpaper backgrounds. Exclusive zones are subtracted from the tiling area. Confirmed
  with a real Quickshell layer-shell client.
- **ext-session-lock-v1**, implemented from scratch (`SessionLockHandler` in
  `shell/mod.rs`): while locked, rendering shows only the lock surface over an opaque
  black backdrop, and both keyboard focus (`update_keyboard_focus`) and pointer focus
  (`surface_under`, the single choke point every pointer handler routes through) are
  pinned to it — nothing hidden underneath can receive input. Verified against a real
  `swaylock`.
- **The real Utumno shell**, autostarted and run inside spitfire end-to-end: loaded
  cleanly, no crashes, correctly fell back to the generic `ext-workspace-v1` workspaces
  widget (no niri/Hyprland/Sway misdetection). Confirmed via screenshot.
- **`spitfire.border`**, rendered as four thin solid-color strips around each tiled
  window (`render::border_elements`) — active/inactive color picked from whichever
  window has keyboard focus. Confirmed via screenshot with a bright test color.
- **A `.desktop` session entry + app icon** (`packaging/spitfire.desktop`,
  `assets/logo/`), installable via `make install` — see [Packaging](#packaging).
  `XDG_CURRENT_DESKTOP=spitfire` is set for autostarted clients, same convention as
  niri/Hyprland/sway.
- **Optional built-in bar** (`crate::bar`, swaybar/i3bar-style, off by default —
  `spitfire.bar = { enable, height, bg, fg, fg_active }`). Not a client or a protocol:
  drawn by the compositor itself as solid-color rectangles, the same
  `SolidColorRenderElement` primitive as `spitfire.border`, reserving `height` at the top
  of the tiling area the same way a layer-shell exclusive zone would. No font — digits
  are 7-segment glyphs and the layout-mode indicator is a small geometric icon, both built
  from rectangles. Workspace list + active layout mode on the left, clock/date on the
  right; hidden while the session is locked. Confirmed against a running compositor
  (caught and fixed a real bug this way: the background strip was painted after the
  digits/icon in element order, which — same render-order lesson as `spitfire.border` —
  made it draw on top of them instead of behind).
- **XWayland** (opt-in `xwayland` cargo feature, off in the default build): X11
  application support via Smithay's built-in XWayland integration —
  `state.start_xwayland()`, the `XwmHandler` impl in `shell/x11.rs` (window
  map/configure/maximize/fullscreen/move/resize requests, clipboard forwarding to and
  from the Wayland selection). Works under any backend, including the nested `--winit`
  one. Every X11 top-level is wrapped in the same `WindowElement` xdg-shell windows use,
  so it's tiled/bordered/focused exactly the same way — nothing downstream needs to know
  a window came from XWayland. Doesn't fail hard if `Xwayland` isn't installed: it just
  warns and X11-only apps won't work, everything else is unaffected. Verified against
  real X11 clients (`xeyes`, `xterm`) run inside a `--winit` session with
  `--features xwayland`.
- **DRM/KMS backend** (opt-in `udev` cargo feature, `spitfire --udev`): runs as the real
  login session instead of nested inside one — session handling via libseat, GPU/output
  enumeration via udev, real input devices via libinput, adapted from `anvil`'s own
  `udev.rs` (`crates/spitfire/src/udev.rs`). Same tiling/border/bar integration points as
  `--winit`: `arrange_tiling()` runs at the top of every `render_surface` call (there's no
  busy loop to piggyback on the way `--winit` has — rendering is fully event/timer-driven
  under DRM/KMS), `spitfire.border`/`spitfire.bar` drawn the same way. Compiles clean
  (`cargo build -p spitfire --features udev`) but **not yet verified against real
  hardware** — see [Known limitations](#known-limitations--pending-work).

## Known limitations / pending work

- **DRM/KMS backend hasn't run on real hardware yet** — only compile-verified so far (see
  above). Testing it means switching to a bare TTY, which isn't something that can happen
  safely from an automated nested session; needs a hands-on pass before it's trustworthy
  enough for the `.desktop` entry (see [Packaging](#packaging)) to actually be usable.
- Not a spitfire bug, but worth knowing: Utumno's `modules/Bar.qml` overlaps its
  center/right rows on narrow outputs (its own `Math.max`/`Math.min` collision math
  doesn't account for not enough width for both) — only shows up on a narrow nested test
  window, not a real display; left for Utumno to fix.

## Layout

```
crates/
├── spitfire/          # binary + compositor state (winit backend, xdg-shell, wlr-layer-shell)
├── spitfire-layout/    # layout engine, pure, no Wayland deps
├── spitfire-config/    # Lua config loader (mlua)
└── spitfire-ipc/       # control socket (JSON lines) + spitfirectl binary, no Wayland deps either
assets/logo/            # app icon (SVG + PNG sizes)
packaging/spitfire.desktop  # wayland-sessions entry, see Packaging below
examples/config.lua     # default config.lua
```

## Build & run

```sh
cargo build --workspace
cargo test --workspace             # 64 tests across the workspace, no Wayland needed
cargo run -p spitfire -- --winit   # or no arguments at all, --winit is the default

# X11 application support (opt-in, off by default — needs the Xwayland binary installed):
cargo run -p spitfire --features xwayland -- --winit

# DRM/KMS backend (opt-in, real login session instead of nested — see the note above,
# not yet run on real hardware):
cargo build -p spitfire --features udev
```

Needs `wayland-client`/`wayland-server`, `xkbcommon`, EGL, and GBM installed (dev
packages) at minimum; `--features udev` additionally needs `libseat`, `libinput`, and
`libdisplay-info` development packages (`libseat-devel`/`libinput-devel`/
`libdisplay-info-devel` on Void, `-dev` on Debian/Ubuntu, no suffix on Arch).

Config file: `$XDG_CONFIG_HOME/spitfire/config.lua` (falls back to
`~/.config/spitfire/config.lua`). See `examples/config.lua` for the default.

## Packaging

`sudo make install` builds in release mode and installs `spitfire`/`spitfirectl` to
`$PREFIX/bin`, the icon (`assets/logo/`) into the `hicolor` theme, and a
[`packaging/spitfire.desktop`](packaging/spitfire.desktop) session entry into
`$PREFIX/share/wayland-sessions/` (`make uninstall` reverses it; `PREFIX` defaults to
`/usr`, override with `make PREFIX=... install`). The `.desktop` entry is still installed
ahead of being trustworthy: `spitfire`'s DRM/KMS backend (what a display manager
launching it from a bare TTY would need) exists and compiles (`--features udev`, see
above) but hasn't been run on real hardware yet, so a display manager launching it
straight from a bare TTY is unverified. Run it from inside your current session for now
(`cargo run -p spitfire -- --winit`); the entry is there so packaging is ready once
`--udev` gets that hands-on pass.
