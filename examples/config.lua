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

-- `radius` (logical pixels, default 0) rounds the border's corners — and,
-- since it's drawn on top of the window, masks the window's own square
-- corners along with it. 0 keeps the classic square-cornered look. Keep
-- `width` comfortably thick relative to `radius` (5:12 below) — too thin a
-- border can't fully cover the window's own square corner tips at the
-- radius's curve, leaving a sliver of square corner poking out.
spitfire.border = { width = 5, active = "#7aa2f7", inactive = "#414868", radius = 12 }
spitfire.gaps = { inner = 20, outer = 10 }

-- Keyboard layout — empty string means "let xkbcommon pick its own
-- default" (in practice, "us"). Same fields/meaning as setxkbmap's flags
-- of the same names. Applied at startup and again on every
-- spitfire.reload() (no restart needed to try a different layout).
-- spitfire.keyboard = { layout = "pt", variant = "", model = "", options = "" }

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
