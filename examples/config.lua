-- spitfire default config — the dwm config.h equivalent, but Lua and
-- reloadable at runtime (spitfire.reload(), or `spitfirectl reload`).
--
-- Install at ~/.config/spitfire/config.lua.

-- Appearance ---------------------------------------------------------
-- radius rounds the border's corners; keep width comfortably thick
-- relative to it or a sliver of the window's own square corner pokes out.
spitfire.border = { width = 5, active = "#7aa2f7", inactive = "#414868", radius = 12 }
spitfire.gaps = { inner = 20, outer = 10 }
spitfire.anim = { enabled = true, duration = 150 } -- pop-in on open, tween on re-flow, slide on workspace switch

-- Blur-behind for a window with real per-pixel transparency of its own
-- (alacritty's window.opacity, kitty's background_opacity, foot's alpha)
-- — radius is the one global strength knob, opt in per-window with the
-- `blur = true` rule below.
-- spitfire.blur = { radius = 20 }

-- Inputs ---------------------------------------------------------------
-- Empty string = let xkbcommon pick its own default ("us" in practice).
-- repeat_delay (ms) / repeat_rate (repeats/sec) — if typing ever feels
-- like it drops in doubled letters, raise repeat_delay rather than
-- assuming a bug.
-- spitfire.keyboard = { layout = "pt", variant = "", model = "", options = "", repeat_delay = 600, repeat_rate = 25 }

-- Hovering a window focuses it without raising/reordering it (click stays
-- what raises). Off by default.
-- spitfire.focus_follows_mouse = true

-- Output scale (niri-style), >= 1.0 — a starting value; Mod+Shift+P/M
-- already rescales live at runtime.
-- spitfire.output = { scale = 1.0 }

-- Bar ------------------------------------------------------------------
-- Drawn by spitfire itself, no client needed. Set enable = false if
-- you're running a client bar instead (e.g. Utumno's own).
spitfire.bar = {
  enable = true,
  height = 28,
  bg = "#1e1e2e",
  fg = "#6c7086",
  fg_active = "#cdd6f4",
}

-- Autostart --------------------------------------------------------------
-- Whatever draws your bar/launcher/wallpaper/lockscreen — spitfire has no
-- opinion on which frontend you use.
spitfire.autostart({
  -- "your-bar-here",
  -- "swaybg -i ~/wallpaper.png",
})

-- Window rules -----------------------------------------------------------
-- app_id matching is exact and case-sensitive — check `spitfirectl
-- list-windows` if a rule doesn't fire. A window can match more than one
-- rule; every matching field applies.
spitfire.rule({ app_id = "pavucontrol", floating = true, centered = true })
-- spitfire.rule({ app_id = "signal", hide_from_capture = true }) -- invisible to screenshots/recordings, still visible to you
-- spitfire.rule({ app_id = "abiword", workspace = 2 }) -- always opens on workspace 2
-- spitfire.rule({ app_id = "Alacritty", blur = true })
-- spitfire.rule({ app_id = "scratchterm", blur = true }) -- the --class scratchpad terminal below has its own app_id, needs its own line

-- Keybindings --------------------------------------------------------
-- spitfire.bind(mods, key, fn) — mods is "Mod4" (Super)/"Mod1" (Alt)/
-- "Shift"/"Ctrl", combined with "+" (e.g. "Mod4+Shift"); "" = no modifier.

-- Layouts: tile (dwm master-stack), floating, fibonacci (spiral), monocle.
spitfire.bind("Mod4", "t", function() spitfire.layout.set("tile") end)
spitfire.bind("Mod4", "f", function() spitfire.layout.set("fibonacci") end)
spitfire.bind("Mod4", "m", function() spitfire.layout.set("monocle") end)
spitfire.bind("Mod4+Shift", "space", function() spitfire.layout.set("floating") end)
spitfire.bind("Mod4", "space", function() spitfire.layout.cycle() end)

-- Master area size / window count (dwm: MODKEY+h/l and MODKEY+i/d)
spitfire.bind("Mod4", "l", function() spitfire.mfact.inc(0.05) end)
spitfire.bind("Mod4", "h", function() spitfire.mfact.inc(-0.05) end)
spitfire.bind("Mod4", "i", function() spitfire.nmaster.inc(1) end)
spitfire.bind("Mod4", "d", function() spitfire.nmaster.inc(-1) end)

-- Window management
spitfire.bind("Mod4", "q", function() spitfire.window.close() end)
spitfire.bind("Mod4", "j", function() spitfire.window.focus_next() end)
spitfire.bind("Mod4", "k", function() spitfire.window.focus_prev() end)
spitfire.bind("Mod4+Shift", "j", function() spitfire.window.swap_next() end)
spitfire.bind("Mod4+Shift", "k", function() spitfire.window.swap_prev() end)

-- Workspaces — dynamic: focusing workspace 5 with only 2 open just creates
-- 3, 4 and 5. Advertised over ext-workspace-v1, so any bar that knows the
-- protocol sees them (spitfire.bar above, or a client bar instead).
for i = 1, 9 do
  spitfire.bind("Mod4", tostring(i), function() spitfire.workspace.focus(i) end)
  spitfire.bind("Mod1", tostring(i), function() spitfire.workspace.move_window(i) end)
end
-- Ceiling for the gesture-bound next()/prev() below (a repeated swipe has
-- no natural "stop" the way typing a number does) — not a startup floor,
-- workspaces still only ever exist once asked for. A field on the
-- *existing* spitfire.workspace table — spitfire.workspace = {...} would
-- wipe out focus/move_window/next/prev above it.
-- spitfire.workspace.max = 9

-- Scratchpad — one hidden slot, toggled both ways by the same bind.
spitfire.bind("Mod4+Shift", "grave", function() spitfire.window.toggle_scratchpad() end)
-- Named scratchpad — a drop-down terminal: spawns the first time, then
-- shows/hides that exact instance. --class gives it its own app_id so it
-- isn't confused with a plain alacritty window. Last two args (optional)
-- size it as a fraction of the screen when shown.
spitfire.bind("Mod4", "grave", function()
  spitfire.scratchpad.toggle("term", "alacritty --class scratchterm", "scratchterm", 1.0, 0.5)
end)

-- Launching things — spitfire has no built-in/hardcoded terminal.
spitfire.bind("Mod4", "Return", function() spitfire.spawn("alacritty") end)
spitfire.bind("Mod4+Shift", "q", function() spitfire.quit() end)
spitfire.bind("Mod4+Shift", "r", function() spitfire.reload() end)

-- Media keys (bare, no modifier — dedicated hardware keys). Via ALSA
-- (amixer); swap for wpctl (PipeWire) or pactl (PulseAudio) if that's not
-- what your system mixes through.
spitfire.bind("", "XF86AudioRaiseVolume", function() spitfire.spawn("amixer -q set Master 5%+") end)
spitfire.bind("", "XF86AudioLowerVolume", function() spitfire.spawn("amixer -q set Master 5%-") end)
spitfire.bind("", "XF86AudioMute", function() spitfire.spawn("amixer -q set Master toggle") end)

-- Touchpad gestures ----------------------------------------------------
-- spitfire.gesture(fingers, direction, fn) — direction is "left"/"right"/
-- "up"/"down". udev/real-hardware only, a no-op under --winit. Kept on
-- separate finger counts (3 vs 4) so neither set collides with the other.
-- next()/prev() move relative to whichever workspace is active, unlike
-- focus(n)'s fixed target, so a repeated swipe keeps advancing.
spitfire.gesture(3, "left", function() spitfire.workspace.next() end)
spitfire.gesture(3, "right", function() spitfire.workspace.prev() end)
spitfire.gesture(3, "up", function() spitfire.window.toggle_scratchpad() end)
spitfire.gesture(3, "down", function() spitfire.layout.cycle() end)
spitfire.gesture(4, "left", function() spitfire.window.focus_prev() end)
spitfire.gesture(4, "right", function() spitfire.window.focus_next() end)
