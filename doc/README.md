<p align="center"><img src="../assets/logo/spitfire-wc-icon-192.png" width="96" alt="spitfire logo"></p>

# spitfire (developer notes)

This is the technical/developer-facing README — architecture, crate layout, build
flags, protocol coverage, what's been verified against real clients/hardware and how.
If you just want to *use* spitfire as your Wayland session, see the
[root README](../README.md) instead; this one assumes you're reading or changing the
code.

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
OSD, and Ollama chat popup), not a dependency of this repo. There's also an optional
built-in bar (`spitfire.bar`) for anyone who'd rather not run a separate client at all.

v1 is complete and has since had a full hands-on hardware pass: config, IPC, workspaces,
layer-shell/session-lock, window borders, XWayland, and the DRM/KMS backend are all
implemented *and* verified against real clients (swaylock, Quickshell, the real Utumno
shell) and, as of the latest round, a real login session on real hardware (not nested) —
not just unit tests. See [What's implemented](#whats-implemented) below for what that
covers and [Known limitations](#known-limitations--pending-work) for what's still open.

## What's implemented

- **Layouts** (`spitfire-layout`, pure Rust, no Wayland deps, 29 tests): `tile`
  (dwm master-stack), `floating`, `fibonacci` (spiral), `monocle`. Each workspace tracks
  its own mode/`nmaster`/`mfact`/gaps independently.
- **Config** (`spitfire-config`, via `mlua`): a single `config.lua`
  (`$XDG_CONFIG_HOME/spitfire/config.lua`, falling back to
  `~/.config/spitfire/config.lua`), reloadable at runtime with `spitfire.reload()` — no
  recompile. `spitfire.bind`/`spawn`/`layout`/`mfact`/`nmaster`/`workspace`/`window`/
  `rule`/`autostart`/`gaps`/`border`/`bar`/`keyboard`. Mod4/Super and Mod1/Alt are both
  first-class modifiers, freely mixable per bind. See `../examples/config.lua` for the
  default bindings.
  - `spitfire.window.close()`/`.focus_next()`/`.focus_prev()`: close the focused window,
    cycle keyboard focus within the active workspace (dwm-style, wraps around). Newly
    mapped windows also grab keyboard focus automatically now, the moment their first
    buffer commits — no pointer hover/click required.
  - `spitfire.keyboard = { layout, variant, model, options, rules }`: XKB config from
    Lua instead of always defaulting to `"us"`, applied at startup and hot-reloaded via
    `spitfire.reload()` (`KeyboardHandle::set_xkb_config`, no restart needed).
- **Control socket** (`spitfire-ipc` + the `spitfirectl` CLI, JSON lines at
  `$XDG_RUNTIME_DIR/spitfire.sock`, one request/response per connection — niri
  msg/hyprctl-style): `reload`, `quit`, `layout <mode>`, `workspace focus <n>`,
  `list-windows`, `list-outputs`, `list-workspaces`.
- **Dynamic per-output workspaces** (`crate::workspace`, niri-style growth — focusing
  workspace 5 when only 2 exist just creates 3, 4, and 5), advertised live over
  `ext-workspace-v1` (`crate::ext_workspace`, hand-implemented — not provided by
  Smithay, see `../NOTICE.md`). `spitfire.workspace.focus(n)` /
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
  pinned to it — nothing hidden underneath can receive input, and no
  `spitfire.bind()`/built-in shortcut fires while locked (only VT-switch still does, as
  an escape hatch). Verified against real `swaylock` and, on real DRM/KMS hardware, a
  real PAM-backed Quickshell lock screen — including a real bug caught only there: the
  lock surface never received `wl_surface.frame` done callbacks under the event-driven
  DRM/KMS render loop (`--winit`'s continuous redraw loop masked it entirely), so a
  frame-throttled client painted once at map time and never again. Fixed in
  `pre_repaint`/`post_repaint`/`update_primary_scanout_output`/
  `take_presentation_feedback` (`state.rs`), which now treat the lock surface as a
  first-class surface alongside windows and layer-shell surfaces.
- **The real Utumno shell**, autostarted and run inside spitfire end-to-end: loaded
  cleanly, no crashes, correctly fell back to the generic `ext-workspace-v1` workspaces
  widget (no niri/Hyprland/Sway misdetection). Confirmed via screenshot.
- **`spitfire.border`**, rendered as four thin solid-color strips around each tiled
  window (`render::border_elements`) — active/inactive color picked from whichever
  window has keyboard focus. Confirmed via screenshot with a bright test color.
- **Server-side decoration header bar** (`shell/ssd.rs`, for clients that don't draw
  their own — e.g. alacritty): a thin 11px strip with close/maximize buttons, colored
  from the Tokyo Night palette (background `#414868`, close `#f7768e`, maximize
  `#9ece6a`) — same palette `spitfire.border`'s own defaults already use.
- **A `.desktop` session entry + app icon** (`packaging/spitfire.desktop`,
  `assets/logo/`), installable via `make install` — see [Packaging](#packaging).
  `XDG_CURRENT_DESKTOP=spitfire` is set for autostarted clients, same convention as
  niri/Hyprland/sway.
- **Optional built-in bar** (`crate::bar`, on by default in `examples/config.lua` —
  `spitfire.bar = { enable, height, bg, fg, fg_active }`). Not a client or a protocol:
  drawn by the compositor itself as solid-color rectangles, the same
  `SolidColorRenderElement` primitive as `spitfire.border`. Floats — inset from the
  top/left/right edges of the output by `spitfire.gaps.outer`, the same gap windows
  get — rather than sitting flush against the screen edge; `TilingLayout::arrange`
  reserves that gap (above *and* below the bar) plus `height` at the top of the tiling
  area. No TTF: every glyph is its own hand-built 5×7 bitmap (`glyph_for` in `bar.rs`) —
  digits, A-Z, and the symbols the bar needs (`% - : . | +`). Workspace list + active
  layout mode on the left; on the right, in order: CPU% (delta over two `/proc/stat`
  samples), RAM% (`/proc/meminfo`), battery% with a `+` while charging
  (`/sys/class/power_supply/BAT*`, `--` on a desktop), network SSID + signal% (via `iw`,
  `--` with no wireless/no `iw`), and the clock/date. Hidden while the session is
  locked. Confirmed against a running compositor (caught and fixed two real bugs this
  way: the background strip painted after its own content in element order — same
  render-order lesson as `spitfire.border` — and later, this same underlying "focus
  the newly-committed buffer" check reused for window auto-focus initially read a
  buffer field the renderer had already consumed, so it silently never fired).
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
  enumeration via udev, real input devices via libinput (including tap-to-click, enabled
  by default on touchpads that support it), adapted from `anvil`'s own `udev.rs`
  (`crates/spitfire/src/udev.rs`). Same tiling/border/bar integration points as
  `--winit`: `arrange_tiling()` runs at the top of every `render_surface` call (there's
  no busy loop to piggyback on the way `--winit` has — rendering is fully event/timer-
  driven under DRM/KMS), `spitfire.border`/`spitfire.bar` drawn the same way. **Verified
  on real hardware** as an actual login session via greetd (`packaging/spitfire.desktop`
  → `packaging/spitfire-session`, which logs to
  `$XDG_STATE_HOME/spitfire/session.log`) — that hands-on pass is what found and fixed a
  batch of DRM/KMS-only bugs: a crash on the very first `spitfire.bind()` press (a
  missing match arm), every `Mod4+Shift+<letter>` bind silently never matching (XKB
  reports the *shifted*/uppercase keysym once Shift is held; bind matching now
  case-folds letters before comparing), server-side-decorated windows overflowing past
  their tile slot (the header bar's height wasn't subtracted from the configured content
  size), and the session-lock frame-callback bug described above.

## Known limitations / pending work

- Not a spitfire bug, but worth knowing: Utumno's `modules/Bar.qml` overlaps its
  center/right rows on narrow outputs (its own `Math.max`/`Math.min` collision math
  doesn't account for not enough width for both) — only shows up on a narrow nested test
  window, not a real display; left for Utumno to fix.
- The bar's bitmap font only has uppercase A-Z — text is uppercased before drawing, so
  an SSID (or anything else routed through it) always displays in caps, not necessarily
  matching its real casing.

## Layout

```
crates/
├── spitfire/          # binary + compositor state (winit + udev/DRM-KMS backends, xdg-shell, wlr-layer-shell)
├── spitfire-layout/    # layout engine, pure, no Wayland deps
├── spitfire-config/    # Lua config loader (mlua)
└── spitfire-ipc/       # control socket (JSON lines) + spitfirectl binary, no Wayland deps either
assets/logo/            # app icon (SVG + PNG sizes)
packaging/               # spitfire.desktop (wayland-sessions entry) + spitfire-session (logging wrapper)
examples/config.lua     # default config.lua
doc/README.md            # this file
```

## Build & run

```sh
cargo build --workspace
cargo test --workspace             # tests across the workspace, no Wayland needed
cargo run -p spitfire -- --winit   # or no arguments at all, --winit is the default

# X11 application support (opt-in, off by default — needs the Xwayland binary installed):
cargo run -p spitfire --features xwayland -- --winit

# DRM/KMS backend (opt-in, real login session instead of nested):
cargo run -p spitfire --features udev -- --udev
```

Needs `wayland-client`/`wayland-server`, `xkbcommon`, EGL, and GBM installed (dev
packages) at minimum; `--features udev` additionally needs `libseat`, `libinput`, and
`libdisplay-info` development packages (`libseat-devel`/`libinput-devel`/
`libdisplay-info-devel` on Void, `-dev` on Debian/Ubuntu, no suffix on Arch).

Config file: `$XDG_CONFIG_HOME/spitfire/config.lua` (falls back to
`~/.config/spitfire/config.lua`). See `../examples/config.lua` for the default.

## Packaging

`sudo make install` builds in release mode (with `udev,xwayland` — see `Makefile`) and
installs `spitfire`/`spitfirectl`/`spitfire-session` to `$PREFIX/bin`, the icon
(`assets/logo/`) into the `hicolor` theme, and a
[`packaging/spitfire.desktop`](../packaging/spitfire.desktop) session entry into
`$PREFIX/share/wayland-sessions/` (`make uninstall` reverses it; `PREFIX` defaults to
`/usr`, override with `make PREFIX=... install`). The `.desktop` entry launches
`spitfire-session`, a thin wrapper around `spitfire --udev` that redirects its
stdout/stderr to `$XDG_STATE_HOME/spitfire/session.log` (a bare `Exec=` line otherwise
has nowhere obvious to send that output) — this log is what made diagnosing the
DRM/KMS-only bugs above possible. Both the `.desktop` entry and the DRM/KMS backend it
launches have now had a real hands-on pass on real hardware (see above), including
through `greetd`.
