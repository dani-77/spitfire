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

-- Workspaces (Phase 5) — dynamic, niri-style: asking to focus workspace 5
-- when only 2 exist just creates 3, 4, and 5. 1-based. Advertised over
-- ext-workspace-v1, so a bar (Utumno's Workspaces.qml, waybar, ...) sees
-- them without any spitfire-specific code.
for i = 1, 9 do
  spitfire.bind("Mod4", tostring(i), function() spitfire.workspace.focus(i) end)
  spitfire.bind("Mod4+Shift", tostring(i), function() spitfire.workspace.move_window(i) end)
end

-- Launching things — this is you configuring which terminal to use,
-- spitfire itself has no built-in/hardcoded default (see spitfire.spawn
-- below); swap "Mod4" for "Mod1" here, or anywhere else, freely.
spitfire.bind("Mod4", "Return", function() spitfire.spawn("alacritty") end)
spitfire.bind("Mod4+Shift", "q", function() spitfire.quit() end)
spitfire.bind("Mod4+Shift", "r", function() spitfire.reload() end)

-- Window rules — floating windows are left out of the tiling arrangement
-- entirely (their geometry is never touched).
spitfire.rule({ app_id = "pavucontrol", floating = true })

-- Autostart — spitfire has no opinion on which frontend/shell you use.
-- Point this at whatever draws your bar/launcher/wallpaper/lockscreen: a
-- wlr-layer-shell-v1 client is all that's required (waybar, eww, a plain
-- swaybg + swaylock + wlogout combo, a Quickshell-based shell, ...).
--
-- Example, if you happen to have Utumno (github.com/dani-77/utumno) checked
-- out as a sibling repo:
--   spitfire.autostart({ "qs -p " .. os.getenv("HOME") .. "/Projectos/utumno" })
-- or, once installed system-wide (`sudo make install` from within that
-- repo) so `-c utumno` resolves: "qsd77 run -c utumno" (dani77/qsd77,
-- github.com/dani-77/qsd77 — a small Go CLI wrapper around quickshell,
-- also gives you its `launcher`/`session`/`locker` IPC subcommands for
-- binds).
spitfire.autostart({
  -- "your-bar-here",
  -- "swaybg -i ~/wallpaper.png",
})
