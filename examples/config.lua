-- spitfire default config — the dwm config.h equivalent, but Lua and
-- reloadable at runtime (spitfire.reload(), or `spitfirectl reload`)
-- instead of a recompile.
--
-- Install at $XDG_CONFIG_HOME/spitfire/config.lua (usually
-- ~/.config/spitfire/config.lua).

-- spitfire.bind(mods, key, fn) takes any modifier per bind, independently —
-- Mod4 (Super/Windows) and Mod1 (Alt) are both first-class; nothing below
-- is fixed, edit any line to move it to whichever modifier (or key) you
-- want, or mix both across different binds. Accepted spellings:
-- Mod4/Super/Logo/Cmd and Mod1/Alt (see spitfire_config::bind::Modifiers::parse)
-- — Shift/Ctrl combine the same way, e.g. "Mod4+Shift".

-- Layouts: tile (dwm master-stack), floating, fibonacci (spiral), monocle.
spitfire.bind("Mod4", "t", function() spitfire.layout.set("tile") end)
spitfire.bind("Mod4", "f", function() spitfire.layout.set("fibonacci") end)
spitfire.bind("Mod4", "m", function() spitfire.layout.set("monocle") end)
spitfire.bind("Mod4+Shift", "space", function() spitfire.layout.set("floating") end)
spitfire.bind("Mod4", "space", function() spitfire.layout.cycle() end)

-- Master column size / count — dwm: MODKEY+h/l and MODKEY+i/d.
spitfire.bind("Mod4", "l", function() spitfire.mfact.inc(0.05) end)
spitfire.bind("Mod4", "h", function() spitfire.mfact.inc(-0.05) end)
spitfire.bind("Mod4", "i", function() spitfire.nmaster.inc(1) end)
spitfire.bind("Mod4", "d", function() spitfire.nmaster.inc(-1) end)

-- Workspaces — dynamic, niri-style: asking to focus workspace 5 when only
-- 2 exist just creates 3, 4, and 5. 1-based. Advertised over
-- ext-workspace-v1, so any bar that knows that protocol sees them too —
-- spitfire.bar below shows them directly.
for i = 1, 9 do
  spitfire.bind("Mod4", tostring(i), function() spitfire.workspace.focus(i) end)
  spitfire.bind("Mod1", tostring(i), function() spitfire.workspace.move_window(i) end)
end

-- Touchpad gestures — udev/real-hardware only, a no-op in a nested
-- --winit session (there's no libinput device to swipe on). `fingers = 0`
-- would match any finger count; 3/4 here keep both sets off of whatever a
-- 2-finger swipe already means to you (scroll). A gesture's callback runs
-- exactly like a bind's — any spitfire.* call works inside it.
spitfire.gesture(3, "left", function() spitfire.workspace.focus(2) end)
spitfire.gesture(3, "right", function() spitfire.workspace.focus(1) end)
spitfire.gesture(3, "up", function() spitfire.window.toggle_scratchpad() end)
spitfire.gesture(3, "down", function() spitfire.layout.cycle() end)
-- 4-finger left/right cycles window focus instead — same as Mod4+j/k,
-- kept on a different finger count so it can't collide with the 3-finger
-- workspace switch above.
spitfire.gesture(4, "left", function() spitfire.window.focus_prev() end)
spitfire.gesture(4, "right", function() spitfire.window.focus_next() end)

-- Launching things — this is you configuring which terminal to use,
-- spitfire itself has no built-in/hardcoded default (see spitfire.spawn
-- below); swap "Mod4" for "Mod1" here, or anywhere else, freely.
spitfire.bind("Mod4", "Return", function() spitfire.spawn("alacritty") end)
spitfire.bind("Mod4+Shift", "q", function() spitfire.quit() end)
spitfire.bind("Mod4+Shift", "r", function() spitfire.reload() end)

-- Media keys — dedicated hardware keys, so no modifier needed (they can't
-- collide with anything text-related). Raise/lower/mute via ALSA
-- (amixer); swap for "wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+-/mute-toggle"
-- (PipeWire) or "pactl set-sink-volume/set-sink-mute @DEFAULT_SINK@ ..."
-- (PulseAudio via pactl) if amixer/ALSA isn't what your system mixes
-- through.
spitfire.bind("", "XF86AudioRaiseVolume", function() spitfire.spawn("amixer -q set Master 5%+") end)
spitfire.bind("", "XF86AudioLowerVolume", function() spitfire.spawn("amixer -q set Master 5%-") end)
spitfire.bind("", "XF86AudioMute", function() spitfire.spawn("amixer -q set Master toggle") end)

-- Window management — close the focused window, cycle which one has
-- keyboard focus (dwm-style j/k; wraps around), swap window order (Shift+j/k).
spitfire.bind("Mod4", "q", function() spitfire.window.close() end)
spitfire.bind("Mod4", "j", function() spitfire.window.focus_next() end)
spitfire.bind("Mod4", "k", function() spitfire.window.focus_prev() end)
spitfire.bind("Mod4+Shift", "j", function() spitfire.window.swap_next() end)
spitfire.bind("Mod4+Shift", "k", function() spitfire.window.swap_prev() end)

-- Scratchpad — a single hidden slot. Press once on the focused window to
-- stash it (unmapped, out of the way); press again with nothing relevant
-- focused to bring it back, centered on screen. Same bind toggles both
-- directions — it always acts on whatever's already stashed once the slot
-- is full, regardless of what's currently focused.
spitfire.bind("Mod4+Shift", "grave", function() spitfire.window.toggle_scratchpad() end)

-- Named scratchpad — a drop-down terminal, XMonad/LeftWM-style: the same
-- bind spawns it the first time, then shows/hides that exact instance
-- (same process, scrollback and all) every time after. `--class` gives it
-- an app_id distinct from any other alacritty window, so spitfire knows
-- which newly-mapped window to claim as "the" scratchpad terminal instead
-- of grabbing the next alacritty you happen to open normally. The last two
-- args (both optional) size it as a fraction of the screen's usable area
-- the moment it's claimed — here, full width, half height, so it doesn't
-- default to filling the whole usable area top to bottom. Drop both to
-- just keep whatever size alacritty opens itself at.
spitfire.bind("Mod4", "grave", function()
  spitfire.scratchpad.toggle("term", "alacritty --class scratchterm", "scratchterm", 1.0, 0.5)
end)

-- Window rules — floating windows are left out of the tiling arrangement
-- entirely (their geometry is never touched). Add `centered = true` to have
-- it open in the middle of the screen instead of wherever it happened to
-- cascade to.
spitfire.rule({ app_id = "pavucontrol", floating = true, centered = true })

-- `hide_from_capture = true` skips this window entirely from any
-- wlr-screencopy capture (grim, screen-share/recording) — a privacy flag,
-- not a real-screen effect: the window stays fully visible to you, just
-- never shows up in a screenshot or stream. Uncomment and adjust the
-- app_id for whatever you'd rather keep off-screen in a recording.
-- spitfire.rule({ app_id = "signal", hide_from_capture = true })

-- `workspace = n` (1-based) sends this app straight to workspace n the
-- moment it opens, wherever you're currently looking — same convention as
-- Mod4+n above. Handy for apps you always want parked in the same spot
-- (a word processor on workspace 2, a chat client on 9, ...).
-- spitfire.rule({ app_id = "abiword", workspace = 2 })

-- `blur = true` renders a frosted-glass backdrop right behind this
-- window's own content — meant for a terminal (or launcher) with real
-- per-pixel transparency of its own (alacritty's `window.opacity`, kitty's
-- `background_opacity`, foot's `alpha`), so whatever's behind it reads as
-- blurred through the translucent parts instead of sharp. `spitfire.blur`
-- below is the one global strength knob every `blur = true` window shares.
-- `app_id` matching is exact and case-sensitive (same caveat the
-- `workspace` rule above already calls out) — a plain `alacritty` launch
-- reports "Alacritty", capitalized; confirm with `spitfirectl list-windows`
-- if this doesn't fire. Two rules, not one, because the named-scratchpad
-- terminal above spawns with `--class scratchterm` — a *different* app_id
-- from a plain launch, so it needs its own line to get blur too.
-- spitfire.rule({ app_id = "Alacritty", blur = true })
-- spitfire.rule({ app_id = "scratchterm", blur = true })

-- `radius` (logical pixels, default 0) rounds the border's corners — and,
-- since it's drawn on top of the window, masks the window's own square
-- corners along with it. 0 keeps the classic square-cornered look. Keep
-- `width` comfortably thick relative to `radius` (5:12 below) — too thin a
-- border can't fully cover the window's own square corner tips at the
-- radius's curve, leaving a sliver of square corner poking out.
spitfire.border = { width = 5, active = "#7aa2f7", inactive = "#414868", radius = 12 }
spitfire.gaps = { inner = 20, outer = 10 }

-- Strength of every `spitfire.rule({ blur = true })` window's backdrop
-- (roughly a pixel radius — see the rule example above). 0 disables blur
-- entirely without touching individual rules. Only costs anything on a
-- frame where a `blur = true` window is actually on screen.
-- spitfire.blur = { radius = 20 }

-- Window animations (scale-in "pop" on open, tween on tiling re-flow, slide
-- on workspace switch) — purely visual; layout, focus and hit-testing are
-- unaffected. duration is milliseconds; enabled = false (or duration <= 0)
-- disables all three.
spitfire.anim = { enabled = true, duration = 150 }

-- Focus-follows-mouse (sloppy focus) — off by default, so the current
-- click-to-focus behavior is untouched unless you turn this on. Hovering a
-- window gives it keyboard focus without raising/reordering it (raising
-- stays click-only, so windows don't jump to the front just because the
-- pointer swept over them). Hovering empty space — gaps, wallpaper, or a
-- layer-surface like a bar — leaves focus exactly where it was, it never
-- focuses nothing.
-- spitfire.focus_follows_mouse = true

-- Output scale (niri-style): a fractional multiplier applied to every
-- output at startup, `1.0` (the default) being the classic 1:1 behavior.
-- Just a starting value — Mod+Shift+P/M already rescale up/down live at
-- runtime; this only seeds that same mechanism, and re-applies live on
-- spitfire.reload() too. Must be >= 1.0.
-- spitfire.output = { scale = 1.0 }

-- Keyboard layout — empty string means "let xkbcommon pick its own
-- default" (in practice, "us"). Same fields/meaning as setxkbmap's flags
-- of the same names. Applied at startup and again on every
-- spitfire.reload() (no restart needed to try a different layout).
--
-- repeat_delay (ms, default 600) / repeat_rate (repeats per second,
-- default 25) tune how long a key must be held before it starts
-- auto-repeating, and how fast it repeats after that — sent to clients,
-- which run their own repeat timer off these two numbers (the compositor
-- itself never synthesizes repeat key events). If typing normally ever
-- feels like it's dropping in doubled letters, raise repeat_delay rather
-- than assuming a bug: a normal keystroke can easily hold for 150-200ms,
-- and too tight a delay makes clients mistake that for an intentional
-- hold.
-- spitfire.keyboard = { layout = "pt", variant = "", model = "", options = "", repeat_delay = 600, repeat_rate = 25 }

-- Optional built-in bar — on by default so there's something visibly
-- alive on screen out of the box. Not a client, not a protocol: drawn by
-- spitfire itself (a bitmap font, no TTF), floating with a gap of
-- spitfire.gaps.outer on the top/left/right edges. Workspace list +
-- active layout mode on the left; CPU/RAM/battery/network/clock/date on
-- the right. Coexists fine with a client bar (e.g. Utumno's own) — set
-- `enable = false` if you're using one of those instead.
spitfire.bar = {
  enable = true,
  height = 28,
  bg = "#1e1e2e",
  fg = "#6c7086",
  fg_active = "#cdd6f4",
}

-- Autostart — spitfire has no opinion on which frontend/shell you use.
-- Point this at whatever draws your bar/launcher/wallpaper/lockscreen: a
-- wlr-layer-shell-v1 client is all that's required (waybar, eww, a plain
-- swaybg + swaylock + wlogout combo, a Quickshell-based shell, ...).
spitfire.autostart({
  -- "your-bar-here",
  -- "swaybg -i ~/wallpaper.png",
})
