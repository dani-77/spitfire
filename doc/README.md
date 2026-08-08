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
  `rule`/`autostart`/`gaps`/`border`/`bar`/`keyboard`/`output`. Mod4/Super and Mod1/Alt are both
  first-class modifiers, freely mixable per bind. See `../examples/config.lua` for the
  default bindings.
  - `spitfire.window.close()`/`.focus_next()`/`.focus_prev()`/`.swap_next()`/`.swap_prev()`: close
    the focused window, cycle keyboard focus or swap the window's position within the active
    workspace tiling order (dwm-style, wraps around). Newly mapped windows also grab keyboard
    focus automatically now, the moment their first buffer commits — no pointer hover/click
    required.
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
  - **`spitfire.border.radius`**: optional rounded corners, `0` (square, the default)
    otherwise. The obvious approach — a GLES pixel shader, what niri/Strata both use —
    doesn't fit here: `border_elements` is generic over the renderer (shared by winit's
    `GlesRenderer` and udev's `UdevRenderer`/`MultiRenderer`), and smithay's
    `PixelShaderElement` only implements `RenderElement` for a concrete `GlesRenderer`,
    so no single trait bound satisfies both backends' concrete renderer type at once.
    Uses a CPU-rasterized `MemoryRenderBuffer` corner mask instead (`CornerMaskCache`) —
    already generic over the renderer (the cursor uses the same mechanism in both
    backends) — drawn on top of the window so it also masks the small triangular
    slivers of the window's own square corners that poke out past the rounded inner
    edge, without ever clipping the client's real content. Confirmed working on real
    DRM/KMS hardware, no flicker.
- **Scratchpad windows** (`crate::workspace`):
  - `spitfire.window.toggle_scratchpad()`: a single anonymous slot. Stashes whichever
    window has keyboard focus (unmapped, taken out of its workspace's tiling order —
    same treatment `hide_inactive_workspaces` already gives an inactive workspace), or
    brings back whatever's already stashed there, centered (`shell::center_on_output`,
    factored out of `center_if_ruled`) and focused.
  - `spitfire.scratchpad.toggle(name, spawn_cmd, app_id, width_frac?, height_frac?)`: a
    named, app-specific scratchpad (XMonad/LeftWM-style "drop-down terminal"). Spawns
    `spawn_cmd` the first time (or again if the window it held died), claims the next
    window that maps with the given `app_id`
    (`SpitfireState::claim_pending_named_scratchpad`, called from `shell::commit`'s
    first-buffer path), then shows/hides that exact instance on every later toggle —
    never closed and reopened, scrollback and all preserved. `width_frac`/`height_frac`
    (each `0.0..=1.0`, either omittable) size it as a fraction of the output's usable
    area once, at claim time.
    - Both share a `layout::ForceFloating` `UserDataMap` marker (deliberately never
      removed once set, matching how sway/i3 scratchpad windows stay floating for
      good) to keep `TilingLayout::arrange` from folding a scratchpad window back into
      the tiling grid the instant it's shown.
    - Real bug found and fixed via actual use: a freshly spawned named-scratchpad
      window's `app_id` becomes known well before its first buffer commits, and
      `arrange` runs every frame — so for however many frames passed in between, an
      unclaimed scratchpad window sat in the *tiled* set and got a real tile-slot size
      `send_pending_configure`d to it (a terminal generally honors whatever size it's
      told before it first draws), opening at the full height of the usable area
      instead of its own preferred size. Fixed by having `arrange`/
      `matches_floating_rule` also exclude any `app_id` a pending
      `spitfire.scratchpad.toggle` call is currently waiting to claim, closing the gap
      instead of only ever preventing *future* passes (`ForceFloating` isn't set until
      the claim itself runs, which is too late for the *first* configure).
- **Server-side decoration header bar** (`shell/ssd.rs`, for clients that don't draw
  their own — e.g. alacritty): a thin 11px strip with a single close button (the
  maximize button was dropped — unused clutter next to it on a client with no
  titlebar of its own), 5px in from the right edge, colored from the Tokyo Night
  palette (background `#414868`, close `#f7768e`) — same palette `spitfire.border`'s
  own defaults already use.
- **`spitfire.output.scale`** (niri-style): a fractional multiplier (`>= 1.0`, default
  `1.0`) applied to every output at startup — `OutputConfig` in `spitfire-config`,
  applied via `Output::change_current_state` in both `winit.rs` and `udev.rs`, and
  re-applied live on `spitfire.reload()`. Just a starting value: `Mod4+Shift+P`/`M`
  already rescaled outputs live at runtime before this config field existed
  (`KeyAction::ScaleUp`/`ScaleDown` in `input_handler.rs`); this only seeds that same
  mechanism instead of always starting at `1.0`.
- **A `.desktop` session entry + app icon** (`packaging/spitfire.desktop`,
  `assets/logo/`), installable via `make install` — see [Packaging](#packaging).
  `XDG_CURRENT_DESKTOP=spitfire` is set for autostarted clients, same convention as
  niri/Hyprland/sway.
- **A D-Bus session bus + `xdg-desktop-portal` backend selection**, both handled by
  `packaging/spitfire-session`/`spawn_autostart` rather than left to the user: nothing
  upstream of spitfire (greetd et al.) provisions `DBUS_SESSION_BUS_ADDRESS` on this kind
  of session, so `spitfire-session` wraps the compositor in `dbus-run-session`, which
  opens a private bus and exports that variable before `exec`ing `spitfire --udev`.
  That bus exists *before* spitfire runs, though, so D-Bus-activated services (notably
  `xdg-desktop-portal`'s backends) don't inherit `WAYLAND_DISPLAY`/`DISPLAY` just because
  spitfire itself has them — `spawn_autostart` (`input_handler.rs`) now runs
  `dbus-update-activation-environment` synchronously first, pushing
  `WAYLAND_DISPLAY`/`DISPLAY`/`XDG_CURRENT_DESKTOP` into the bus's activation environment
  before any autostart entry gets a chance to trigger a portal call. Without this,
  `xdg-desktop-portal-gtk` exited immediately with `Gtk-WARNING: cannot open display` in
  a crash/re-activate loop the moment anything asked for a `FileChooser`/`Settings`/etc.
  portal. `packaging/spitfire-portals.conf` (installed to
  `/etc/xdg-desktop-portal/spitfire-portals.conf`) then picks `xdg-desktop-portal-gtk` as
  the preferred backend for those interfaces instead of leaving it to the generic
  `portals.conf`'s ambiguous `default=*`, which could otherwise land on
  `xdg-desktop-portal-gnome` with no `gnome-shell`/Mutter actually backing it.
  `ScreenCast`/`Screenshot` aren't covered by this config — spitfire doesn't implement
  `wlr-screencopy`/`ext-image-copy-capture-v1` yet, so there's nothing for
  `xdg-desktop-portal-wlr` to call into even if it were installed. Verified live: a fresh
  session restart on real hardware, `xdg-desktop-portal-gtk` activates cleanly and stays
  up, `FileChooser`/`Settings` (via the `gnome` backend for the latter, which doesn't
  need Mutter for plain `GSettings` reads) both answer over D-Bus.
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
  `--features xwayland`. Three real bugs found and fixed against a much heavier real
  client (Steam) than `xeyes`/`xterm` ever exercised:
  - `map_window_request` never joined an X11 window to the active workspace's tiling
    order the way a Wayland toplevel does in `shell/xdg.rs`'s `new_toplevel` — so
    `hide_inactive_workspaces` had no idea it existed and never unmapped it on a
    workspace switch. An XWayland window stayed mapped and visible on *every*
    workspace, forever, instead of just the one it opened on. Fixed by pushing it into
    the tiling order on map and removing it again on unmap, mirroring the Wayland path.
    Confirmed fixed on real hardware.
  - `configure_request` called `X11Surface::configure()` unconditionally for every
    window and discarded the `Result`. That call immediately errors out — no XCB
    request goes out at all — when given a position for an override-redirect window
    (menus, dropdowns, tooltips: windows that opt out of window-manager placement by
    definition), so every override-redirect `ConfigureRequest` was a silent no-op: a
    menu asking to be placed at the click location, or a submenu asking to be placed
    next to its parent, just stayed wherever it was originally created. Steam's own
    menus opened in the screen's top-left corner, and submenus did nothing at all when
    hovered into. Fixed by skipping `configure()` entirely for `is_override_redirect()`
    windows — `configure_notify` (unchanged) is what actually keeps `self.space` in
    sync with wherever the X server applied their own `ConfigureWindow` request.
  - Plenty of real popups/menus (Steam's CEF-based UI included) map as *ordinary*,
    non-override-redirect windows rather than true override-redirect ones, so the fix
    above didn't cover them — they still went through `place_new_window`'s random
    cascade and then `TilingLayout::arrange`'s tiling, landing at a tile-slot position
    instead of wherever they'd actually asked for. `is_positioned_by_client()` extends
    the same "leave it alone" treatment to a window that's transient-for another
    window (dialogs) or EWMH-typed as a menu/dropdown/popup-menu/tooltip/notification.
    Confirmed on real hardware: Steam's menus went from unusable (opening off in the
    corner) to clickable, though not pixel-perfectly positioned — see
    [Known limitations](#known-limitations--pending-work).
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
  size), and the session-lock frame-callback bug described above. Also: a key held
  across a VT switch (Ctrl+Alt+F2 and back, or the chord that triggered the switch
  itself) never got its release, since `libinput` is fully suspended for as long as the
  session is inactive — `SessionEvent::PauseSession` now calls the already-existing
  (but previously never invoked) `release_all_keys()` right as the pause begins, so a
  VT switch resets held-key state instead of carrying it across a gap the compositor
  can't observe. A separate, still-open "key occasionally does the wrong thing" report
  from the same testing session turned out, on inspection, to trace back to the
  XWayland-workspace-visibility bug above (Steam silently still receiving input) rather
  than a lost keyboard event — a full keycode-level audit of the press/release log
  never turned up an actual dropped or duplicated event.

## Known limitations / pending work

- Not a spitfire bug, but worth knowing: Utumno's `modules/Bar.qml` overlaps its
  center/right rows on narrow outputs (its own `Math.max`/`Math.min` collision math
  doesn't account for not enough width for both) — only shows up on a narrow nested test
  window, not a real display; left for Utumno to fix.
- Steam's own menus/submenus (see the XWayland bullet above) are clickable but land
  roughly 15-20% off from where they actually asked to be — `configure_notify` keeps
  `self.space` in sync with whatever the X server reports, so this looks like it's
  Steam/CEF itself computing that position from a wrong idea of the X11 screen's size
  rather than something `configure_request`/`configure_notify` are getting wrong
  server-side. Not chased further — the menus are usable now, which is the part that
  mattered.
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
packaging/               # spitfire.desktop (wayland-sessions entry), spitfire-session (D-Bus/logging
                        # wrapper), spitfire-portals.conf (xdg-desktop-portal backend preference)
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

The crate's own `winit`/`udev`/`xwayland` cargo features are independent of each
other (`--no-default-features --features udev,xwayland` builds and runs fine with no
nested `--winit` mode compiled in at all — `main.rs`'s no-argument default falls back
to `--udev` in that case) — verified across all four combinations (`winit,egl`
default; `winit,xwayland,egl`; `udev,xwayland,egl`; `udev,xwayland,winit,egl`, the
real packaging build).

Config file: `$XDG_CONFIG_HOME/spitfire/config.lua` (falls back to
`~/.config/spitfire/config.lua`). See `../examples/config.lua` for the default.

## Packaging

`sudo make install` builds in release mode (with `udev,xwayland` — see `Makefile`) and
installs `spitfire`/`spitfirectl`/`spitfire-session` to `$PREFIX/bin`, the icon
(`assets/logo/`) into the `hicolor` theme, a
[`packaging/spitfire.desktop`](../packaging/spitfire.desktop) session entry into
`$PREFIX/share/wayland-sessions/`, and
[`packaging/spitfire-portals.conf`](../packaging/spitfire-portals.conf) into
`/etc/xdg-desktop-portal/spitfire-portals.conf` — note that last one is *not* under
`$PREFIX`, only `$DESTDIR` (`make uninstall` reverses all of it; `PREFIX` defaults to
`/usr`, override with `make PREFIX=... install`). The `.desktop` entry launches
`spitfire-session`, a thin wrapper around `spitfire --udev` (via `dbus-run-session`, see
[What's implemented](#whats-implemented) above for the D-Bus/portal details) that
redirects its stdout/stderr to `$XDG_STATE_HOME/spitfire/session.log` (a bare `Exec=`
line otherwise has nowhere obvious to send that output) — this log is what made
diagnosing the DRM/KMS-only bugs above possible, and the D-Bus activation ones. Both the
`.desktop` entry and the DRM/KMS backend it launches have now had a real hands-on pass on
real hardware (see above), including through `greetd`.
