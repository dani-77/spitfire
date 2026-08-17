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
  recompile. `spitfire.bind`/`gesture`/`spawn`/`layout`/`mfact`/`nmaster`/`workspace`/`window`/
  `rule`/`autostart`/`gaps`/`border`/`bar`/`keyboard`/`output`/`anim`/`focus_follows_mouse`. Mod4/Super and Mod1/Alt are both
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
  - `spitfire.keyboard.repeat_delay`/`.repeat_rate` (ms / repeats-per-second, default
    `600`/`25`, hot-reloadable via `KeyboardHandle::change_repeat_info`) — sent to clients
    via `wl_keyboard.repeat_info`; the compositor never synthesizes repeat key events
    itself, each client runs its own repeat timer off these two numbers. **Fixed a
    long-standing "keys repeat randomly" complaint** that earlier sessions audited
    repeatedly and cleared the compositor of (every raw press/release pair in the debug
    log was clean, focus routing was correct, nothing was stuck) — the bug wasn't a
    missing/duplicated event, it was `repeat_delay` being hardcoded to `200`ms
    (`seat.add_keyboard(..., 200, 25)`), never exposed to config. Measured directly
    against a live session log (33.7k press/release pairs): median key-hold is ~120ms,
    but ~8% of ordinary keystrokes hold for >= 200ms (long tail, p95 ~355ms). At a
    200ms delay, every one of those legitimately started the client's own repeat timer —
    a doubled letter with a perfectly clean compositor-side event log, every time. Raised
    the default to 600ms (typical desktop norm; GNOME/KDE/X11 sit in the 450-660ms
    range — p99 of the same sample is ~960ms, comfortably under it) and made both values
    `spitfire.keyboard` fields instead of hardcoded.
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
  - Real bug found and fixed via actual multi-workspace use: the list stayed in order
    for a couple of workspaces, then reshuffled (a newly created one landing leftmost
    instead of at the end) once there were more. Cause was on the compositor side —
    `coordinates` (optional in the protocol, meant for exactly this: giving clients a
    stable key to order workspaces by) was never sent, so Utumno's generic
    `ext-workspace-v1` widget — which sorts purely by `coordinates` — fell back to
    whatever order its own internal bookkeeping iterated in, stable only by luck for a
    handful of workspaces. Fixed by sending 1D `coordinates` (just the workspace's
    index) on every sync.
  - Real bug found and fixed: `spitfire.gaps` was only ever applied to whichever
    workspace already existed at startup (`WorkspaceSet::default()`'s single workspace)
    — any workspace created afterward (dynamic growth via
    `spitfire.workspace.focus(n)`, `create()`, or an `ext-workspace-v1`
    `create_workspace` request) silently fell back to the layout engine's own hardcoded
    gap default instead of whatever was configured. `spitfire.reload()` happened to
    paper over it for workspaces that already existed at reload time (it loops over all
    of them), but not for ones created afterward. Fixed by having `WorkspaceSet`
    remember the last-applied gaps (`apply_gaps`) and seed every newly created
    workspace with it too, instead of only ever re-applying to workspaces that already
    exist.
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
  - **Z-order, fixed 2026-08-11**: every window's border strip/corner mask now renders in
    the same per-window pass as its own content (`render::output_elements`), immediately
    in front of it, instead of one global batch always inserted at the very front of the
    whole frame. The old batch approach was correct only as long as a border genuinely
    never overlapped anything but its own window's square corners — which stopped being
    true the moment some *other* window (in practice, a floating popup) sat on top of a
    tiled window's border strip; confirmed via a zoomed `grim` capture at the time: a
    floating popup overlapping the seam between two tiled windows showed a tiled window's
    border cutting straight across it despite the popup being the actually-focused,
    actually-topmost window. An earlier attempt at this same per-window interleave was
    reverted for pushing window content *behind* the layer-shell background instead of in
    front of it — and the first version of *this* fix repeated that exact mistake, caught
    only hours later on real hardware: it kept every `SpaceRenderElements::Surface` from
    smithay's blanket `space_render_elements` call regardless of which `wlr-layer-shell`
    band it came from, and pushed all of them — upper (`Top`/`Overlay`) *and* lower
    (`Bottom`/`Background`) alike — before any window content. That puts a
    background/wallpaper layer-shell client (confirmed with a real `awww-daemon` solid-fill
    background) permanently in front of every window: nothing you open ever visibly
    appears, not because it isn't rendering, but because it's rendering *behind the
    wallpaper*. Invisible in nested-`--winit` testing the whole time because that
    environment never had a background layer-shell client running — the fix that
    "worked" there had never actually been exercised against the one case that mattered.
    Real fix: `render::layer_shell_elements`, a direct `layer_map_for_output` +
    `Layer`-partition call (line-for-line the same iteration smithay's own
    `space_render_elements` does internally, just callable twice instead of once) —
    upper band pushed before the per-window loop, lower band pushed after it, so a
    wallpaper stays behind every window the way `Layer::Background` promises, while a
    top-layer panel/OSK still stays in front the way `Layer::Overlay` promises. Only
    takes the per-window path at all when there's something to interleave
    (`border_width > 0` and/or an animation in flight) — idle with borders disabled
    still renders through the identical blanket `space_render_elements` call as before,
    unaffected. Re-verified with a nested `spitfire --winit` session running a real
    `awww-daemon` solid-color background: a window opened on top of it is now visible
    immediately, and a floating popup centered over the seam between two tiled windows
    still shows its own border cleanly on top with no bleed-through from the tiled
    windows' border underneath.
- **`spitfire.anim`** (`crate::anim`, `crate::workspace::WorkspaceSlide`): basic window
  animations, mangowm-style — a scale-in ("pop") when a window's content first appears, a
  smooth tween whenever `TilingLayout::arrange` moves/resizes a window (a new window
  joining, a layout/`nmaster`/`mfact` change, another window closing and the rest
  re-flowing, ...), and a slide when switching workspaces.
  `spitfire.anim = { enabled = true, duration = 150 }` (milliseconds; `enabled = false`
  or `duration <= 0` disables all three, one knob for all of it). Interactive drag/resize
  (`shell/grabs.rs`) is never animated — it's already 1:1 with the pointer.
  > Re-enabled at render time 2026-08-11 (`render::constrain_space_element_no_crop`) — see
  > the "third real bug" entry a few bullets down for the fix; this note is kept for
  > context on why it was ever off.
  - Purely a render-time visual transform: `Space`-mapped geometry, `xdg_toplevel`
    configure size, keyboard focus, and input hit-testing are all applied immediately,
    exactly as before this existed — only what gets *drawn* is interpolated (ease-out
    cubic) for `duration` after the triggering event. Built on smithay's own
    `constrain_space_element` (already used for the alt-tab-style preview grid) with
    `ConstrainScaleBehavior::Stretch`, which already does exactly what a move/resize
    tween needs (stretch a window's elements to fill an arbitrary target rect) — no
    hand-rolled rescale/relocate math needed.
  - No new "wake the compositor" timer infrastructure needed in either backend: verified
    by reading the scheduling code directly rather than assuming. `winit.rs` already
    redraws unconditionally every loop iteration; `udev.rs`'s `render_surface`/
    `frame_finish` reschedule chain already keeps invoking the render pipeline roughly
    once per output-refresh interval indefinitely once started, regardless of damage.
    Animation state is just read fresh (`Instant::now()`) every time that already-
    running pipeline renders a frame.
  - Idle (nothing animating) renders through the exact same `space_render_elements` call
    as before this feature existed, byte-for-byte — the per-window animated path in
    `render::output_elements` only runs on the handful of frames something is actually
    animating, so it doesn't regress the zero-damage-when-idle behavior the rest of the
    renderer (`RectCache`, `CornerMaskCache`) is built around.
  - Real bug found and fixed via actual use: a brand-new window joins the tiling order
    (and so gets tiled into its real slot) well before it has any content — there's no
    "this window just appeared" distinction at the layout-engine level, so that very
    first, pre-content placement animated exactly like any other move. That made
    `spitfire.border`'s empty outline visibly grow/slide into place before the window
    had anything to show, out of sync with the open animation that starts moments later
    once real content commits — a border animating on its own reads as a glitch, not an
    animation. Fixed by skipping the move animation (not the placement itself, which
    still happens instantly as before) for any window still in `pending_initial_focus`.
  - A second real bug, also found via actual use: the open animation originally faded
    alpha in (0 → 1) alongside the scale, exactly like most compositors' open animations
    do. `spitfire.border` was never part of that fade (`border_elements` has no alpha
    input), so for the animation's ~150ms a newly-opened window's border was instantly
    opaque around content that was still translucent — see-through straight to whatever
    was already open underneath. Barely noticeable with only one window on screen;
    glaring for a floating window (a polkit/sudo prompt, say) opened on top of another.
    Fixed by making Open scale-only — alpha is always `1.0` now, for every animation kind
    (`anim::open_scale_and_alpha`) — trading the fade for guaranteeing a just-opened
    window is never composited as anything less than fully opaque.
  - A third real bug, more serious, found live rather than in a screenshot: some windows
    (`feh`, GIMP's main window while its own welcome dialog was up) mapped —
    `spitfirectl list-windows`/`list-workspaces` both saw them, tiling reserved their
    slot — but rendered *no content at all*, permanently, until something unrelated
    forced a full redraw (closing another window; killing and relaunching the stuck one).
    Bisected precisely (three real builds tested against the live session, not guessed):
    clean at the `v0.3.0` tag and at the commit immediately before this one, broken at
    this commit itself. Root cause was in `constrain_space_element` (smithay), the
    per-window transform helper the second/third animation kinds above are built on: it
    wraps each element in a `CropRenderElement`, whose `from_element` returns `None` —
    silently producing *no* render element, not an error — whenever its crop rect fails
    to cleanly overlap the element's own geometry that frame (smithay's own source even
    flags this: `// FIXME: intersection sometimes return a 0 size element`). A brand-new
    window's very first paint landing on exactly one of those frames drew nothing that
    frame — and `OutputDamageTracker` still recorded the (empty) draw as "this window's
    commit is handled", so no later frame, including once the animation ended and
    rendering fell back to the plain path, ever had a reason to redraw it. A first fix —
    fall back to a plain render whenever `constrain_space_element` produced zero elements
    that frame, instead of skipping the window — mostly worked but didn't fully close it:
    per the FIXME above, smithay's own intersection check can apparently also return
    `Some` with a degenerate near-zero rect, which no "was it empty" check downstream can
    catch. Correctness (a window is never silently un-renderable) matters more than this
    feature's visual polish, so `render::output_elements` was changed to never consult
    `anims` for what actually gets drawn at all — see this bullet's parent note.
    - **Fixed 2026-08-11** (`render::constrain_space_element_no_crop`): a line-for-line copy
      of smithay's own `constrain_space_element` → `constrain_as_render_elements` →
      `constrain_render_elements` chain, same scale/offset math throughout, except the
      final step never wraps the result in `CropRenderElement` — it stops at
      `RelocateRenderElement<RescaleRenderElement<E>>`, both infallible constructors
      (`Element`, unlike `CropRenderElement`, has no `Option` in its constructor). Only
      used for `spitfire.anim`'s own `Stretch`+`Geometry`+`CENTER` case, where the
      rescaled reference lands on the constrain rect exactly anyway (mod float rounding)
      — the crop step there was never adding real clipping, only the risk of the bug
      above. `space_preview_elements` (the alt-tab-style preview grid) is untouched and
      still goes through smithay's real `constrain_space_element`/`CropRenderElement` —
      its `Fit` behavior keeps content within bounds by construction, so it doesn't hit
      the failure mode this exists to avoid, and doesn't need this guarantee as much as
      it would cost giving up real cropping. New `OutputRenderElements::Anim` variant
      (`RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>`, no
      `CropRenderElement` wrapper) alongside the existing `Preview` one, since
      `render_elements!` needs a distinct variant per wrapped type. Verified against a
      nested `spitfire --winit` session (isolated `XDG_RUNTIME_DIR`/`XDG_CONFIG_HOME` so
      it doesn't collide with a live session's control socket or autostart) with `grim`:
      realistic open/move/workspace-slide sequences, plus a deliberately extreme stress
      pass (10+ windows opened back-to-back, three layout cycles in under two seconds) —
      no window ever lost content permanently in either case. That stress pass did
      surface alacritty terminals losing their visible text after being squeezed to ~1-2
      rows tall and back — reproduced identically with `spitfire.anim.enabled = false`,
      so it's the terminal's own grid-reflow behavior on extreme resize, not a spitfire
      regression (confirmed further by the window's border/background still rendering
      correctly throughout, unlike this bug's original "nothing at all, forever"
      signature).
    - **Follow-up regression caught the same day**: re-enabling per-window content
      animation exposed that `Workspace::border_rects()` had been left reading *plain*
      `element_geometry` (a leftover from when content animation was disabled and
      matching it made both instant, in sync) — so `spitfire.border` snapped to its
      final rect on frame one while content eased in behind it, and during a
      workspace-switch slide the border didn't move with its window at all. Confirmed
      precisely (not just by eye) by adding temporary `eprintln!` tracing to
      `push_move`/`resolve_all`/`border_rects` and diffing the two width sequences over
      a real monocle-layout transition: content ramped smoothly (367 → 370 → 389 → 407
      → … → 821 across ~450 frames), the border jumped straight from 367 to 821 after
      three frames. Fixed by having `border_rects()` run each window's geometry through
      `WindowAnimations::on_screen_rect` (open/move tween) and then
      `workspace_slide_offset_for` (slide offset) before building its `BorderRect` —
      exactly what `anim::animated_windows` already does for content, and what
      `on_screen_rect`'s and `slide_windows`'s own doc comments already described as the
      intent. Re-verified with the same nested-`--winit` + `grim` setup: a mid-transition
      capture now shows the border only around content's current (smaller) rect instead
      of the full target, settling cleanly once the tween finishes.
  - Move animations apply to XWayland windows for free (same backend-agnostic tiling
    order); open animations are Wayland-only, since X11 clients typically already have
    a pixmap by map time.
  - Deliberately out of scope for now: fade-out on close (no dedicated Wayland "window
    destroyed" hook exists today — `XdgShellHandler::toplevel_destroyed` is
    unimplemented — and a client's buffer can become unrenderable mid-fade with no clean
    recovery).
  - **Workspace-switch slide** (`WorkspaceSlide` in `workspace.rs`): the one animation
    that needs two workspaces on screen at once, which nothing else here required.
    `SpitfireState::switch_workspace` used to unmap the outgoing workspace's windows
    from `Space` immediately (`hide_inactive_workspaces`) — an instant cut, nothing left
    to slide out. With `spitfire.anim` enabled it now leaves them mapped instead,
    registers a `WorkspaceSlide { from_idx, to_idx, start, duration }`, and lets
    `arrange_tiling` map the incoming workspace's windows as normal — both workspaces'
    windows are simultaneously live in `Space` for the animation's duration, offset
    left/right of their real position (switching to a higher index enters from the
    right, exits left; a lower index is the mirror). Unmapped for real once the slide
    finishes (`finalize_workspace_slide`, called every frame right alongside
    `window_anims.prune()`).
    - **No changes to `render.rs` at all** — its per-window animated path already takes
      a generic `&[AnimatedWindow]` and doesn't care *why* a window has an entry.
      `animated_windows()` just folds a slide's offset into each affected window's
      existing entry (or adds a plain offset-only one, alpha `1.0`, for a window with no
      independent open/move animation of its own) — reusing the exact
      `constrain_space_element`/`Stretch` machinery built for points 1/2 unchanged.
      `border_rects` gets the same treatment so `spitfire.border` never detaches from
      its window mid-slide.
    - A second switch while one is still in flight doesn't compound offsets: the *old*
      slide's outgoing workspace is unmapped for real immediately (no longer relevant to
      anything), and the workspace it was sliding *into* — which is exactly
      `self.workspaces.active_index()` at that point — becomes the *new* slide's
      outgoing one, so it slides back out instead of just vanishing.
    - Verified by reading rather than assuming: a freshly re-mapped incoming workspace's
      windows don't spuriously also get a per-window Move animation stacked on top of the
      slide offset, because `TilingLayout::arrange`'s existing `space.element_geometry`
      guard (added for point 2) already returns `None` for a window that was unmapped
      while hidden — the exact case a Move animation needs a real "before" rect to
      exist to fire at all.
- **`spitfire.focus_follows_mouse`** (`SpitfireState::update_keyboard_focus_hover` in
  `input_handler.rs`): sloppy focus, off by default —
  `spitfire.focus_follows_mouse = true` to turn it on. Two behavioral choices, both made
  explicitly rather than defaulted into:
  - **No raise on hover.** Only `update_keyboard_focus` (click/touch-down/tablet-tip)
    raises/restacks; the hover path calls `keyboard.set_focus` alone, so a window never
    jumps to the front just because the pointer swept over it. Reordering stays a
    deliberate, click-only action, matching dwm/i3/sway's own sloppy-focus behavior.
  - **Hovering empty space keeps the last focus.** Gaps, wallpaper, and layer-surfaces
    (the built-in bar, or any client one) are all deliberately excluded from hover-focus
    resolution — the pointer leaving every window never focuses "nothing", and moving it
    across the bar to reach another window doesn't steal focus into the bar along the way.
  - Reuses `update_keyboard_focus`'s own target resolution (`FullscreenSurface` first,
    then `Space::element_under`) rather than a new hit-testing path, and the same
    pointer/keyboard/touch grab guard (inverted, since this early-returns instead of
    gating one big block) — a drag-resize/move grab in progress is never focus-stolen
    mid-grab. Short-circuits against `keyboard.current_focus()` before calling
    `set_focus`, so holding the pointer still over one window doesn't re-send focus
    enter/leave every motion event.
  - Called from all three pointer-motion entry points — `on_pointer_move_absolute_windowed`
    (winit/x11), `on_pointer_move` (udev relative), `on_pointer_move_absolute` (udev
    absolute) — right before each one's existing `pointer.motion(...)` call, reusing the
    `Serial` each already computes. Touch-down and tablet-tip are untouched: they already
    go through the raising `update_keyboard_focus` on direct contact, which is orthogonal
    to pointer hover.
- **`spitfire.gesture(fingers, direction, function)`** (2026-08-15, `udev`-only —
  touchpad gesture events are a real-libinput-hardware thing, there's nothing to forward
  in a nested `--winit` session; `spitfire_config::Gesture`/`GestureDirection` +
  `SpitfireState::on_gesture_swipe_*` in `input_handler.rs`). Inspired by the user's other
  compositor's own `wasp.gestures` (`~/Projectos/wasp`) — the underlying libinput swipe
  events (`GestureSwipeBegin`/`Update`/`End`) were already fully wired before this, but
  only ever forwarded to the focused client via `zwp_pointer_gestures`; this is the first
  time the compositor itself ever consumes one. `fingers = 0` matches any finger count,
  same "0/omitted = any" convention `wasp.gestures` documents. Interception has to be
  decided at `GestureSwipeBegin`, before a direction is knowable — only the finger count
  is available yet — so `Config::has_gesture_for_fingers` alone decides whether the whole
  sequence is swallowed (never forwarded, `dx`/`dy` accumulated silently in the new
  `SpitfireState::pending_gesture`) or passed through exactly as before
  `spitfire.gesture` existed; a half-forwarded sequence (client sees `begin`+`update` but
  never an `end`) was the failure mode to avoid. `GestureDirection::classify` picks
  left/right/up/down from the swipe's dominant axis once total `dx`/`dy` is known at
  `GestureSwipeEnd` — same approach wasp's own classifier and niri/GNOME use, so a
  gesture that wobbles diagonally mid-swipe doesn't flip its eventual direction back and
  forth. A cancelled swipe, or one whose finger count matched some `spitfire.gesture` but
  whose eventual direction matched none, fires nothing — a shrug, not an error. Reuses
  `spitfire.bind`'s exact mechanics end to end: a gesture's callback is an
  `mlua::RegistryKey` invoked through `Config::invoke_gesture` (mirrors `invoke_bind`
  precisely), and whatever `Command`s it pushes go through the same
  `apply_config_command` every keybind already does — so `spitfire.workspace.focus(n)`,
  `spitfire.spawn(...)`, or anything else a bind can do works identically from a gesture.
- **`wlr-screencopy-unstable-v1`** (new: `crate::screencopy`, hand-implemented — not
  provided by Smithay, same situation as `ext-workspace-v1`, see `../NOTICE.md`) — what
  `grim` (and, transitively, `xdg-desktop-portal-wlr`'s Screenshot/ScreenCast, though
  the portal side hasn't been tested) speaks. Didn't exist before; added specifically to
  be able to screenshot the compositor at all while chasing the bugs below it and the
  border z-order fix above (`spitfire.border`'s own bullet). wl_shm only (no
  `linux-dmabuf`), manager version 2
  (guarantees a `buffer` event with no `buffer_done` bookkeeping needed) — enough for
  `grim`'s single-shot use, not a real-time screencaster on its own (see the
  damage-tracking sub-bullet just below for the piece of that since fixed). No new Cargo
  dependency — `wayland-protocols-wlr` was
  already pulled in transitively by smithay's own `wayland_frontend` feature. Captures via
  a **fresh offscreen render** of the output (reuses `render::output_elements`/
  `OutputDamageTracker` as-is against a throwaway `GlesTexture`) rather than a readback of
  the just-presented framebuffer, so it doesn't depend on either backend's
  swapchain/scanout buffer still being readable when the request is serviced — the
  tradeoff is a capture never includes the built-in bar (added as `custom_elements` by
  `winit.rs`/`udev.rs` before calling `output_elements`, which the capture path leaves
  out), and `capture_output_region`'s coordinate math assumes `Transform::Normal` (no
  output-rotation handling). Verified working end-to-end against the real udev/DRM
  session with real `grim` captures, and is in fact how the two bugs directly below were
  actually caught and confirmed fixed.
  > **Cursor/dnd-icon fixed 2026-08-14** — the capture path called `output_elements` with
  > an empty `custom_elements` list, so a capture always showed a bare desktop with no
  > pointer, regardless of what was actually on screen: fine for an occasional debug
  > screenshot, a real problem for anything meant to double as a screen-share/streaming
  > source (see the "is wlr-screencopy good enough to stream?" assessment that prompted
  > this). Fixed by extracting the cursor+dnd-icon construction winit.rs/udev.rs already
  > did for their own real frame into a shared `render::cursor_and_dnd_elements`, and
  > having `render_and_copy` call it too — once per pending capture serviced, not built
  > once and shared, since `CustomRenderElements` isn't `Clone`. Verified live on `--udev`:
  > a `grim` capture now shows the actual system cursor at its on-screen position (nested
  > `--winit` testing can't show this one — see that fn's own doc comment: a nested window
  > relies on the host compositor's cursor overlay for the default arrow, which never
  > reaches an inner capture regardless of this fix). First of three planned improvements
  > towards being a real streaming source — real damage-tracking for `copy_with_damage`
  > and a `dmabuf` zero-copy path are still open, see the wl_shm/damage-tracking notes
  > earlier in this same bullet.
  > **Damage-tracking fixed 2026-08-14** (second of the three) — `copy_with_damage`
  > previously behaved exactly like `copy`: every pending capture rendered and copied
  > unconditionally on the very next frame, no `frame.damage()` events ever sent. Correct,
  > but exactly the cost a streaming client (`xdg-desktop-portal-wlr` → PipeWire) wants
  > `copy_with_damage` to avoid paying on a static screen. Fixed by threading the real
  > render loop's own per-frame damage into `service_pending_captures` (`None` when
  > nothing changed that tick) — a `copy_with_damage` capture now only renders/copies on a
  > frame that actually has damage, staying queued at zero cost otherwise, and reports the
  > real rects back via `frame.damage()` before `ready`. winit.rs's plain
  > `OutputDamageTracker` gives exact rects for free; udev.rs's `DrmCompositor` doesn't
  > expose them at that level (only whether *anything* changed), so it conservatively
  > reports the whole output changed instead of a tighter region — always correct, just
  > not maximally precise. Deliberately reused this module's own per-capture
  > `OutputDamageTracker` for *nothing* here — see this bullet's own opening paragraph for
  > why it's recreated fresh every call and so useless as an incremental signal.
  > **Verified live later the same day**, once `wf-recorder` got installed specifically to
  > check this (it requests `copy_with_damage` by default — `grim` never does): recording a
  > nested session with a mostly-idle terminal (only its own text updating roughly once a
  > second) produced just 11 encoded frames over 6 seconds of wall-clock time, instead of
  > one every real frame — direct confirmation the deferred-until-actual-damage gating
  > works, not just that the types compile.
- **A `dmabuf` zero-copy capture path was attempted and reverted the same day.**
  Bumping the manager to protocol version 3 (`linux_dmabuf`/`buffer_done` events,
  `capture_output`-only) let a client request a dmabuf-backed capture instead of `wl_shm`,
  binding the renderer directly to the client's own buffer (genuinely zero-copy — no
  offscreen texture, no `copy_framebuffer`/`map_texture`/`memcpy`). Compiled clean,
  `Bind<Dmabuf>` held for both `GlesRenderer` and `UdevRenderer`'s `MultiRenderer` — but
  tested live with `wf-recorder -c h264_vaapi` (a real GPU-encoder client that negotiates
  dmabuf), it corrupted the shared EGL context: the capture's own dmabuf bind apparently
  used a modifier the driver didn't like, and instead of failing cleanly it broke the
  *real* render loop too (`EGL BAD_ALLOC` / `"context has been lost"`, every frame,
  requiring a hard kill). Root cause is structural, not a bug in this file:
  `linux_dmabuf`'s v3 event only carries a bare fourcc, no modifier list at all — the
  client picks a modifier with no way to know which ones the compositor can actually
  import. Reverted uncommitted rather than shipped. The fix is a different protocol
  entirely, `ext-image-copy-capture-v1` (wire types already available — same
  already-vendored `wayland-protocols` crate, `staging`+`server` features already on via
  smithay), whose `dmabuf_format` event carries a real `modifiers` array — closing exactly
  the hole that crashed the context. The user's own installed `xdg-desktop-portal-wlr`
  (0.8.2) already prefers `ext-image-copy-capture-v1` over `wlr-screencopy` automatically
  when both are advertised, so this isn't speculative — worth doing, just not on this
  protocol.
- **`spitfire.rule({ hide_from_capture = true })`** (2026-08-15,
  `spitfire_config::WindowRule` + `render::output_elements`'s new `hidden_windows` param):
  a privacy flag, inspired by the same feature in the user's other compositor
  ([wasp](https://github.com/dani-77/wasp)'s `shield_when_capture`) — a matching window is
  skipped entirely from a `wlr-screencopy` capture (no content, no border, just whatever's
  behind it left showing through, same as if the window weren't there) while staying fully
  visible on the real screen. Reused for both offscreen-capture paths `output_elements` can
  take (the per-window border/anim loop, and the fullscreen-surface branch); the idle
  "blanket `space_render_elements` call" fast path now also requires an empty
  `hidden_windows` list to take, since that call has no way to skip one window out of the
  batch. Looked up fresh per capture (`render_and_copy` re-derives the matching window list
  from `state.config.rules()` every time), not cached on the window at map time — a
  `spitfire.reload()` that adds/removes the rule takes effect on the very next capture, no
  restart. `grim`/`wf-recorder` unaffected by the flag itself; only whichever window(s)
  actually match it are ever hidden.
- **`ext-image-copy-capture-v1` + `ext-image-capture-source-v1`** (2026-08-16,
  `crate::ext_screencopy`) — the protocol pair that supersedes `wlr-screencopy-unstable-v1`
  above; `xdg-desktop-portal-wlr` (0.8.2+) prefers it automatically the moment both globals
  are advertised. Captures both whole outputs (`ext_output_image_capture_source_manager_v1`)
  and single windows (`ext_foreign_toplevel_image_capture_source_manager_v1`, sourced from an
  `ext-foreign-toplevel-list-v1` handle — see that entry below) — **deliberately SHM-only**:
  no `dmabuf_device`/`dmabuf_format` advertised at all, closing off
  the exact modifier-negotiation hole that crashed the earlier reverted `wlr-screencopy`
  dmabuf attempt (see the entry above) by making that failure mode structurally unreachable,
  rather than merely avoided. Shares `render_and_copy` with `wlr-screencopy` unchanged (only
  gained a `paint_cursor: bool` param, honoring this protocol's `paint_cursors` session
  option) — only the session/frame protocol bookkeeping in `ext_screencopy.rs` is new. The
  `ext_image_copy_capture_cursor_session_v1` object exists (protocol-mandated) but is an inert
  stub — never emits events; a client asking for a separate cursor stream just never sees one
  become active (cursor visibility is still available via a normal session's `paint_cursors`
  option, composited into the frame the same way `wlr-screencopy` always does). Verified live
  with a throwaway `wayland-client` test program (no installed client speaks this protocol
  yet — `wf-recorder` 0.6.0 only ever tries `wlr-screencopy` v3, which spitfire deliberately
  caps at v2, so it never reaches these globals): full session negotiation
  (`buffer_size`/`shm_format`/`done`), a real `capture()` round trip
  (`transform`/`damage`/`presentation_time`/`ready`), and genuine captured pixel data landing
  in the client's buffer — 5/5 clean across repeated fresh nested sessions for the
  single-shot-capture case (matches `grim`'s usage pattern and the
  `xdg-desktop-portal-wlr` Screenshot interface).

  While stress-testing the *second*-capture-on-one-session case (the `copy_with_damage`-style
  gated pattern continuous screen-share/recording needs), found and root-caused a real EGL
  context-loss warning on the *actual on-screen render path*, reproducing identically through
  the already-shipped, unrelated `wlr-screencopy` `copy`→(idle gap)→`copy_with_damage` sequence
  with zero `ext_screencopy.rs` code involved. Confirmed **pre-existing, not introduced by this
  protocol** — tracked as its own open issue (not documented further here, see the project's
  own working notes if picking it back up), not blocking this one.

  **Per-window capture** (`CaptureSource::Toplevel` in `ext_screencopy.rs`) came right after,
  same day: a session sourced from a toplevel handle gets `buffer_size` from the *window's*
  own geometry rather than the output mode, and its render path
  (`render_window_and_copy`) is genuinely different from the whole-output one — not a call
  into `render_and_copy` at all, since that fn is `Space`/output-shaped start to finish.
  Instead it renders just that window's own elements (`WindowElement::render_elements`, the
  same per-window element set the real on-screen per-window loop in `render.rs` already uses)
  into a transparent-backed offscreen texture sized to it — deliberately no compositor
  border/gaps chrome and no cursor compositing, just the window's actual content, matching
  what a "share this window" picker wants. Resolving a client's `ext_foreign_toplevel_handle_v1`
  down to the actual `WindowElement` goes through the protocol's own stable `identifier`
  string, matched against `crate::foreign_toplevel`'s tracking table (no other equality is
  exposed on Smithay's `ForeignToplevelHandle`). A toplevel session's damage gating
  approximates "this window changed" as "the output had damage this tick" — a correct but
  occasionally-over-eager superset, since there's no cheap way to attribute output-level
  damage rects back to one specific window's own (0,0)-based capture buffer. Verified live
  end-to-end with another throwaway test client: found a window by title via
  `ext-foreign-toplevel-list-v1`, created a toplevel-sourced session, and captured it —
  `buffer_size` correctly came back as the *window's* size (not the output's), and the
  captured pixels were fully opaque real content, not the output-capture path's whole-desktop
  composite. The existing whole-output path re-verified alongside it in the same session,
  confirming no regression.
- **`ext-foreign-toplevel-list-v1`** (2026-08-16, `crate::foreign_toplevel`) — advertises
  every open window (title, app_id, a stable per-toplevel identifier) to any client that
  wants a live window list: a pager, a taskbar, or — the reason this landed now — the
  prerequisite for a future per-window `ext-image-copy-capture-v1` source
  (`ext_foreign_toplevel_image_capture_source_manager_v1.create_source` takes exactly this
  protocol's `ext_foreign_toplevel_handle_v1` as its capture target). Unlike every other
  protocol in this file, no hand-rolled `GlobalDispatch`/`Dispatch` code at all — Smithay
  ships a complete, ready-to-use implementation
  (`smithay::wayland::foreign_toplevel_list::ForeignToplevelListState`); `foreign_toplevel.rs`
  is only the glue (`ForeignToplevelListHandler` impl, `delegate_foreign_toplevel_list!`) plus
  `SpitfireState::sync_foreign_toplevels`, a per-frame full resync (same "small enough to be
  free" reasoning `ext_workspace.rs` already uses for its own full resync) that tells it about
  spitfire's own windows.

  The one real design decision: **the source of truth is deliberately not `Space::elements()`**
  — switching workspaces unmaps every window that isn't on the newly active one, so
  `Space::elements()` only ever holds the *active* workspace's windows. Sourcing from it would
  have `send_closed()`ed every other workspace's windows on each switch, then handed out a
  *new* identifier for the same window switching back — a real protocol violation (identifiers
  must stay unique and never be reused for as long as a toplevel is mapped). The real source
  is every workspace's own tiling list (tiled and floating windows both live there) plus the
  two scratchpad slots, which pull a window out of every workspace's tiling list while stashed
  but keep the client itself alive.

  Verified live in nested `--winit` with a throwaway `wayland-client` test program: a window
  already open when the client bound was replayed correctly (identifier/title/app_id, then
  `done`); a *new* window opened after the client had already bound sent a fresh `toplevel`
  event with its own identifier; closing that window sent `closed`. (First attempt at the test
  client itself hit two client-side-only bugs, not spitfire bugs — a missing
  `wayland_client::event_created_child!` specialization, and a polling loop built on
  `dispatch_pending` alone, which only replays already-buffered events and never actually reads
  new ones off the socket; both fixed in the test harness, not here.)
- **`spitfire.rule({ workspace = n })`** (2026-08-16, `WindowRule::workspace` +
  `SpitfireState::move_window_to_workspace`): sends a freshly-mapped window straight to
  workspace `n` (1-based, same convention as `spitfire.workspace.focus`/`move_window`) the
  moment it first maps, without switching the view there — dwm-style "it leaves, you stay
  put", the exact same code path `spitfire.workspace.move_window(n)` already used for the
  focused window; that function is now a thin wrapper around the new, window-generic
  `move_window_to_workspace`. Applied right where `center_if_ruled` already runs (first
  real buffer commit, after the named-scratchpad claim check — a claimed scratchpad window
  skips this, it manages its own workspace membership). If the rule sends the window
  somewhere other than the currently active workspace, the open "pop" animation and the
  first-map keyboard-focus grab are both skipped too (`moved_away` flag) — nothing to
  animate or focus into on a workspace you're not looking at; confirmed via nested
  `--winit` + `spitfirectl`: a ruled window immediately vanished from `list-windows` on the
  active workspace, `list-workspaces` showed it landed on the target workspace instead
  while the active one stayed unchanged, and switching there surfaced it.
- **An opaque backdrop behind every window's content** (`shell/ssd.rs`'s
  `WindowState::backdrop`, drawn in `shell/element.rs`'s
  `WindowElement::render_elements`) — fixes a real bug, reported live and only
  confirmable once `wlr-screencopy` above existed to screenshot it: a floating
  `spitfire.rule({ floating = true })` popup (`d77run`, a launcher) visibly blended with
  whatever window was behind it. Root cause, confirmed via `WAYLAND_DEBUG=1`: the popup's
  search-box surface attaches a real `Argb8888` buffer and only calls
  `wl_surface.set_opaque_region` on a sub-rect smaller than the buffer — genuinely,
  deliberately asking for translucency, presumably expecting compositor-side blur to
  soften it. spitfire implements no blur protocol at all, so the uncovered portion of the
  buffer just alpha-blended with real desktop content sitting behind it — confirmed
  visually via a `grim` capture zoomed on the popup's interior (another window's text
  faintly visible through it). Not the same bug as the open-animation alpha fade fixed
  earlier in this section — that fix is real and unrelated; this is a different, still-live
  client behavior spitfire never had a fallback for. Fixed by giving every window
  (tiled/floating, SSD or not) an opaque `SolidColorBuffer` — reusing the SSD header's own
  color, Tokyo Night `#414868`, for visual consistency — drawn immediately behind its own
  content, sized to exactly its content geometry. Doesn't touch the client's buffer or
  alpha at all, just guarantees something deliberate sits underneath instead of whatever
  else happens to be in the stack. Confirmed fixed via a zoomed `grim` capture: flat,
  uniform background, no bleed-through.
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
  `ScreenCast`/`Screenshot` now route to `xdg-desktop-portal-wlr` too (2026-08-14) —
  spitfire implements `wlr-screencopy` (see the bullet above) and that was the only
  reason they weren't covered before; the config just hadn't caught up.
  `ext-image-copy-capture-v1` (the protocol `xdg-desktop-portal-gnome`/`-kde` would want
  instead) still isn't implemented at all, so those backends remain out of reach on
  purpose. Not yet re-verified end-to-end against a live `Screenshot`/`ScreenCast` call
  (previously confirmed only that `xdg-desktop-portal-wlr` stayed uninvoked while
  unrouted) — worth a real capture through the portal, not just `grim` directly, before
  relying on it. Verified live: a fresh
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
- **`spitfire.rule({ blur = true })` + `spitfire.blur = { radius = n }`** (2026-08-17,
  `crate::blur`) — a frosted-glass backdrop rendered right behind a window's own content,
  for a terminal/launcher with real per-pixel alpha in its own buffer (alacritty's
  `window.opacity`, kitty's `background_opacity`, foot's `alpha`). spitfire has no
  wlroots/SceneFX scene graph to inherit this from (unlike wasp, the feature's origin
  point — see this project's own roadmap notes) — hand-rolled instead: a two-pass
  separable GLES Gaussian blur (`BLUR_FRAG_SHADER`, compiled once via `GlesRenderer::
  compile_custom_texture_shader` and reused every frame after) sampling an offscreen
  capture of the whole output with the `blur = true` window itself excluded, cropped to
  that window's own on-screen rect, composited back in as a plain
  `MemoryRenderBufferRenderElement` immediately behind the window's own content in
  `output_elements`'s existing per-window loop. Real screen only for now —
  `wlr-screencopy`/`ext-image-copy-capture-v1` captures don't reproduce it (a `blur =
  true` window shows as if the rule weren't set in a screenshot/recording).

  **Two real bugs found and fixed live, both the hard way, both required for the feature
  to actually be visible**:

  1. The blur math itself worked on the very first attempt (confirmed by dumping the raw
     blurred buffer to a PNG and inspecting it directly), but nothing showed up on
     screen — a translucent window's own real alpha blending was compositing correctly,
     just against *stale* framebuffer content from before blur ever started, not the
     freshly blurred backdrop. Root cause: a blurred backdrop is a brand-new
     `MemoryRenderBuffer` (a brand-new `Id`) every single frame at a screen position that
     hasn't necessarily moved, which isn't enough on its own for `buffer_age()`-based
     partial redraw to know a *content* change happened there that should count as
     damage. Fixed on `--winit` by forcing a full redraw (`age = 0` instead of the real
     buffer age) on any frame with an active blur backdrop. **Not yet fixed on
     `--udev`**: the equivalent fix (`DrmCompositor::reset_buffer_ages`, cheap — only
     clears each swapchain slot's own age counter) isn't reachable through `DrmOutput`'s
     public API, which only re-exposes the much heavier `reset_buffers` (discards and
     reallocates every slot's actual buffer) — substituting that every frame blur is
     active wasn't a substitution this session could verify was safe on real hardware,
     so it was left as a documented, open gap (`udev.rs`, right where the call would go)
     rather than guessed at.
  2. Even after fix 1, blur *still* didn't show — the window's own translucency was
     reaching a real, freshly-drawn backdrop, just the wrong one. `WindowElement::
     render_elements` (shell/element.rs) already draws an unconditional, fully opaque
     `WindowState::backdrop` immediately behind *every* window's own content (the
     `BG_COLOR` fix noted above, 2026-08-16, for a client relying on compositor blur that
     didn't exist yet) — sitting directly between the window's real surface and
     `crate::blur`'s own backdrop, which is pushed further back still. Any window's own
     alpha was blending with that flat, deliberate fill color, never reaching `crate::
     blur`'s content at all — a `blur = true` window rendered exactly as opaque as any
     other. Fixed with a new `WindowState::blur` flag (shell/ssd.rs), synced fresh every
     frame by `crate::blur::sync_blur_flags` for *every* window on the output (not just
     the currently-blurred ones — a window that loses the rule via `spitfire.reload()`
     needs the flag cleared too, or it keeps skipping its backdrop forever after) and
     read back by `render_elements` to skip pushing its own backdrop entirely for a
     `blur = true` window — an explicit opt-in superseding the generic default, on
     purpose, only for windows that asked for it.

  Every reproduction and the live visual confirmation (a striped/textured background
  clearly visible as a soft Gaussian wash through the translucent window, sharp right up
  to its edge, unchanged outside it) happened in nested `spitfire --winit`.
  **`--udev` update, same day**: confirmed live on the user's own real hardware session
  too — genuine blur, not just opaque — so whatever `DrmCompositor`'s own buffer-age
  bookkeeping does differently from `OutputDamageTracker`'s explicit `age` parameter,
  the stale-framebuffer bug fix 1 above describes doesn't reproduce there in practice, at
  least not in that session. The fix itself (`reset_buffer_ages`) still isn't wired up on
  `--udev` — this is an empirical "hasn't been observed to matter" from one real session,
  not a structural guarantee the way the `--winit` fix is, so the gap stays documented
  rather than declared closed.

  **A third thing worth knowing, not a bug**: `spitfire.rule` matches `app_id` exactly,
  so `spitfire.scratchpad.toggle("term", "alacritty --class scratchterm", ...)`-style
  named scratchpads spawn with a *different* `app_id` (`scratchterm`, not `Alacritty`)
  than a plain launch of the same program — a `blur = true` rule aimed at one doesn't
  cover the other; needs its own separate rule line (see `examples/config.lua`'s
  updated comment).
- **`spitfire.workspace.next()` / `.prev()`** (2026-08-17, `Command::WorkspaceCycle`) —
  relative workspace navigation, switching to whichever workspace is currently active
  `+1`/`-1`, unlike `.focus(n)`'s fixed target. Needed because Lua config code has no
  access to compositor state at all (see this crate's own top-of-file doc comment) — it
  can't read "which workspace is active right now" to compute `focus(current + 1)`
  itself, so that computation has to happen compositor-side, in a dedicated `Command`
  handled where `WorkspaceFocus` already is (`input_handler.rs`). Clamps at workspace 1
  going backward (`(current + delta).max(0)`); capped going forward by
  `spitfire.workspace.max` (`WorkspaceConfig`, default `9`, `0` means unbounded) — a
  plain field set directly on the same `spitfire.workspace` table `.focus`/
  `.move_window`/`.next`/`.prev` already live on (`spitfire.workspace.max = 9`, *not*
  `spitfire.workspace = { max = 9 }`, which would replace the whole table and lose those
  four functions — a real footgun distinct enough from every other `spitfire.<name> =
  {...}` config table in this file that it gets its own test,
  `workspace_functions_still_work_after_setting_max`). `.focus(n)`/`spitfire.rule({
  workspace = n })` stay uncapped either way — explicit, one-shot targets, not something
  a repeated swipe can run away with the way `.next()` can.

  Found via a real config bug: `spitfire.gesture(3, "left", function()
  spitfire.workspace.focus(2) end)` paired with `.focus(1)` on the opposite swipe reads,
  at a glance, like "next/previous workspace" — but it's two fixed targets, so it only
  ever bounces between workspace 1 and 2. From workspace 3 onward, "left" kept landing
  back on 2, never advancing further; "right" always went straight to 1. Not a
  compositor bug — the config just wasn't expressing what it looked like it expressed.
  `examples/config.lua` and the user's own config.lua both had this exact pattern for
  their 3-finger gestures; both now use `.next()`/`.prev()` instead. The `max` cap itself
  was a second, related fix, added right after: testing `.next()` with no ceiling grew
  the user's real workspace list well past 9, which is also what the "Utumno's bar
  isn't showing all 9 workspaces at startup" report the same session turned out to be —
  not a regression (`ext_workspace.rs`, the actual `ext-workspace-v1` exposure, hasn't
  changed at all recently — confirmed via `git log`/`git diff`), just a fresh session
  correctly starting back at exactly 1 workspace after a long prior session had
  organically grown well past 9 through testing.

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
- **Some older GTK3 apps (confirmed: AbiWord, Gnumeric) show a `spitfire.border`-colored
  gap between their rounded corner and where their own content actually starts** — not
  a z-order issue (single window, nothing overlapping it — see the border bullet under
  [What's implemented](#whats-implemented) for the z-order fix itself), and not a wrong
  border position either: `render::border_elements_for` draws flush against whatever geometry
  the client itself reports via `xdg_surface`, and for these two apps specifically that
  reported geometry includes extra invisible margin (presumably an unexcluded CSD
  shadow/resize-border) above and left of where they actually paint — so the border,
  correctly drawn at the edge of that reported geometry, ends up looking detached from
  the visible window. Confirmed via a zoomed, pixel-sampled `grim` comparison: Alacritty
  (well-behaved) shows a border flush against its content with zero gap; AbiWord/Gnumeric
  show a consistent ~15-20 physical-pixel gap of pure `border.inactive` color on both the
  top and left edges alike. `pcmanfm` (also GTK3) doesn't show it — narrows this down to
  something specific to how AbiWord/Gnumeric's older codebases report their window
  geometry rather than a GTK3-wide issue. Nothing to fix compositor-side: spitfire has no
  way to know a client's reported geometry doesn't match where it actually draws.
- **`spitfire.rule({ blur = true })`'s `--winit` stale-framebuffer fix has no `--udev`
  equivalent wired up** — confirmed working live on real `--udev` hardware regardless
  (single-GPU; see [What's implemented](#whats-implemented)), so the bug the `--winit`
  fix addresses (forcing a full redraw on any frame with an active blur backdrop) hasn't
  been observed to reproduce there in practice. Still no equivalent fix actually wired
  up, though: the cheap API for it (`DrmCompositor::reset_buffer_ages`) isn't reachable
  through `DrmOutput`'s public wrapper, and substituting the much heavier
  `reset_buffers` (reallocates every swapchain slot) every frame blur is active wasn't
  something to guess is safe without more hardware to test it on — one clean real-world
  session not hitting it isn't the same as it being structurally impossible. Also
  unverified, for the multi-GPU case specifically: whether `MultiRenderer`'s
  `Offscreen<GlesTexture>` and its `AsMut<GlesRenderer>` resolve to the same GPU context
  (see `crate::blur`'s own doc comment) — true by construction on single-GPU, unverified
  on real multi-GPU.

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
