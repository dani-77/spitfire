//! Registers the global `spitfire` table in the Lua interpreter: every
//! function (`bind`, `spawn`, `layout.set`, ...) only pushes data into
//! shared cells (`Rc<RefCell<Vec<_>>>>`) — none of them has access to
//! compositor state. Whoever loads the config (`Config::load`) and whoever
//! uses it afterward (the `spitfire` crate) is the one that reads those
//! cells and applies the effects.

use std::{cell::RefCell, rc::Rc};

use mlua::{Lua, Table};
use tracing::warn;
use xkbcommon::xkb;

use crate::{
    bind::{Bind, Modifiers},
    command::Command,
    rule::WindowRule,
    BarConfig, BorderConfig, KeyboardConfig,
};

pub(crate) fn install(
    lua: &Lua,
    commands: Rc<RefCell<Vec<Command>>>,
    binds: Rc<RefCell<Vec<Bind>>>,
    autostart: Rc<RefCell<Vec<String>>>,
    rules: Rc<RefCell<Vec<WindowRule>>>,
) -> mlua::Result<()> {
    let spitfire = lua.create_table()?;

    // spitfire.bind(mods, key, function)
    {
        let binds = binds.clone();
        let bind_fn = lua.create_function(
            move |lua, (mods, key, callback): (String, String, mlua::Function)| {
                let mods = Modifiers::parse(&mods);
                let keysym = parse_keysym(&key);
                let callback = lua.create_registry_value(callback)?;
                binds.borrow_mut().push(Bind {
                    mods,
                    keysym,
                    callback,
                });
                Ok(())
            },
        )?;
        spitfire.set("bind", bind_fn)?;
    }

    // spitfire.spawn(cmd)
    {
        let commands = commands.clone();
        let spawn_fn = lua.create_function(move |_, cmd: String| {
            commands.borrow_mut().push(Command::Spawn(cmd));
            Ok(())
        })?;
        spitfire.set("spawn", spawn_fn)?;
    }

    // spitfire.autostart({ "cmd1", "cmd2" })
    {
        let autostart = autostart.clone();
        let autostart_fn = lua.create_function(move |_, cmds: Vec<String>| {
            autostart.borrow_mut().extend(cmds);
            Ok(())
        })?;
        spitfire.set("autostart", autostart_fn)?;
    }

    // spitfire.quit()
    {
        let commands = commands.clone();
        let quit_fn = lua.create_function(move |_, ()| {
            commands.borrow_mut().push(Command::Quit);
            Ok(())
        })?;
        spitfire.set("quit", quit_fn)?;
    }

    // spitfire.reload()
    {
        let commands = commands.clone();
        let reload_fn = lua.create_function(move |_, ()| {
            commands.borrow_mut().push(Command::ReloadConfig);
            Ok(())
        })?;
        spitfire.set("reload", reload_fn)?;
    }

    // spitfire.layout.set(mode) / spitfire.layout.cycle()
    {
        let layout_table = lua.create_table()?;
        {
            let commands = commands.clone();
            let set_fn = lua.create_function(move |_, mode: String| {
                match parse_layout_mode(&mode) {
                    Some(mode) => commands.borrow_mut().push(Command::LayoutSet(mode)),
                    None => warn!(mode, "spitfire.layout.set: unknown layout mode"),
                }
                Ok(())
            })?;
            layout_table.set("set", set_fn)?;
        }
        {
            let commands = commands.clone();
            let cycle_fn = lua.create_function(move |_, ()| {
                commands.borrow_mut().push(Command::LayoutCycle);
                Ok(())
            })?;
            layout_table.set("cycle", cycle_fn)?;
        }
        spitfire.set("layout", layout_table)?;
    }

    // spitfire.mfact.inc(delta) — dwm: MODKEY+h/l
    {
        let mfact_table = lua.create_table()?;
        let commands = commands.clone();
        let inc_fn = lua.create_function(move |_, delta: f64| {
            commands.borrow_mut().push(Command::MfactInc(delta));
            Ok(())
        })?;
        mfact_table.set("inc", inc_fn)?;
        spitfire.set("mfact", mfact_table)?;
    }

    // spitfire.nmaster.inc(delta) — dwm: MODKEY+i/d
    {
        let nmaster_table = lua.create_table()?;
        let commands = commands.clone();
        let inc_fn = lua.create_function(move |_, delta: i32| {
            commands.borrow_mut().push(Command::NmasterInc(delta));
            Ok(())
        })?;
        nmaster_table.set("inc", inc_fn)?;
        spitfire.set("nmaster", nmaster_table)?;
    }

    // spitfire.workspace.focus(n) / spitfire.workspace.move_window(n) — 1-based
    {
        let workspace_table = lua.create_table()?;
        {
            let commands = commands.clone();
            let focus_fn = lua.create_function(move |_, n: usize| {
                if n == 0 {
                    warn!("spitfire.workspace.focus: workspaces are 1-based, 0 is not valid");
                } else {
                    commands.borrow_mut().push(Command::WorkspaceFocus(n));
                }
                Ok(())
            })?;
            workspace_table.set("focus", focus_fn)?;
        }
        {
            let commands = commands.clone();
            let move_window_fn = lua.create_function(move |_, n: usize| {
                if n == 0 {
                    warn!("spitfire.workspace.move_window: workspaces are 1-based, 0 is not valid");
                } else {
                    commands.borrow_mut().push(Command::WorkspaceMoveWindow(n));
                }
                Ok(())
            })?;
            workspace_table.set("move_window", move_window_fn)?;
        }
        spitfire.set("workspace", workspace_table)?;
    }

    // spitfire.window.close() / .focus_next() / .focus_prev() /
    // .toggle_scratchpad()
    {
        let window_table = lua.create_table()?;
        {
            let commands = commands.clone();
            let close_fn = lua.create_function(move |_, ()| {
                commands.borrow_mut().push(Command::WindowClose);
                Ok(())
            })?;
            window_table.set("close", close_fn)?;
        }
        {
            let commands = commands.clone();
            let focus_next_fn = lua.create_function(move |_, ()| {
                commands.borrow_mut().push(Command::WindowFocusNext);
                Ok(())
            })?;
            window_table.set("focus_next", focus_next_fn)?;
        }
        {
            let commands = commands.clone();
            let focus_prev_fn = lua.create_function(move |_, ()| {
                commands.borrow_mut().push(Command::WindowFocusPrev);
                Ok(())
            })?;
            window_table.set("focus_prev", focus_prev_fn)?;
        }
        {
            let commands = commands.clone();
            let toggle_scratchpad_fn = lua.create_function(move |_, ()| {
                commands.borrow_mut().push(Command::WindowToggleScratchpad);
                Ok(())
            })?;
            window_table.set("toggle_scratchpad", toggle_scratchpad_fn)?;
        }
        spitfire.set("window", window_table)?;
    }

    // spitfire.scratchpad.toggle(name, spawn_cmd, app_id, width_frac?,
    // height_frac?) — named, app-specific scratchpad (see
    // spitfire.window.toggle_scratchpad() right above for the simpler,
    // unnamed single-slot version). width_frac/height_frac are each
    // optional (0.0..=1.0, fraction of the output's usable area) —
    // omitted, the window just keeps whatever size it opened itself at.
    {
        let scratchpad_table = lua.create_table()?;
        let commands = commands.clone();
        let toggle_fn = lua.create_function(
            move |_,
                  (name, spawn_cmd, app_id, width_frac, height_frac): (
                String,
                String,
                String,
                Option<f64>,
                Option<f64>,
            )| {
                commands.borrow_mut().push(Command::ScratchpadToggle {
                    name,
                    spawn_cmd,
                    app_id,
                    width_frac,
                    height_frac,
                });
                Ok(())
            },
        )?;
        scratchpad_table.set("toggle", toggle_fn)?;
        spitfire.set("scratchpad", scratchpad_table)?;
    }

    // spitfire.rule({ app_id = "...", floating = true, centered = true })
    {
        let rules = rules.clone();
        let rule_fn = lua.create_function(move |_, tbl: Table| {
            let app_id: Option<String> = tbl.get("app_id").unwrap_or(None);
            let floating: bool = tbl.get("floating").unwrap_or(false);
            let centered: bool = tbl.get("centered").unwrap_or(false);
            rules.borrow_mut().push(WindowRule {
                app_id,
                floating,
                centered,
            });
            Ok(())
        })?;
        spitfire.set("rule", rule_fn)?;
    }

    // spitfire.gaps/border/bar/keyboard = {...} are plain Lua table
    // assignments — they don't need any function here, they're read back
    // in `read_globals` after the script runs.

    lua.globals().set("spitfire", spitfire)?;
    Ok(())
}

fn parse_keysym(name: &str) -> xkb::Keysym {
    let sym = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
    if sym.raw() != 0 {
        return sym;
    }
    // Fallback: accepts "Return"/"return", "Space"/"space", etc.
    let sym = xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE);
    if sym.raw() == 0 {
        warn!(
            key = name,
            "spitfire.bind: unknown key, this bind will never fire"
        );
    }
    sym
}

fn parse_layout_mode(name: &str) -> Option<spitfire_layout::LayoutMode> {
    use spitfire_layout::LayoutMode;
    match name.to_ascii_lowercase().as_str() {
        "tile" => Some(LayoutMode::Tile),
        "floating" => Some(LayoutMode::Floating),
        "fibonacci" | "spiral" => Some(LayoutMode::Fibonacci),
        "monocle" => Some(LayoutMode::Monocle),
        _ => None,
    }
}

/// Reads `spitfire.gaps`/`spitfire.border`/`spitfire.bar`/`spitfire.keyboard`
/// from the globals after the Lua script runs — they're plain table
/// assignments, not function calls, so they never go through the `Command`
/// queue.
#[allow(clippy::type_complexity)]
pub(crate) fn read_globals(
    lua: &Lua,
) -> mlua::Result<(
    spitfire_layout::Gaps,
    BorderConfig,
    BarConfig,
    KeyboardConfig,
)> {
    let mut gaps = spitfire_layout::Gaps::default();
    let mut border = BorderConfig::default();
    let mut bar = BarConfig::default();
    let mut keyboard = KeyboardConfig::default();

    let spitfire: Table = lua.globals().get("spitfire")?;

    if let Ok(g) = spitfire.get::<Table>("gaps") {
        if let Ok(v) = g.get::<i32>("inner") {
            gaps.inner = v;
        }
        if let Ok(v) = g.get::<i32>("outer") {
            gaps.outer = v;
        }
    }

    if let Ok(b) = spitfire.get::<Table>("border") {
        if let Ok(v) = b.get::<i32>("width") {
            border.width = v;
        }
        if let Ok(v) = b.get::<String>("active") {
            if let Some(c) = parse_hex_color(&v) {
                border.active = c;
            } else {
                warn!(
                    value = v,
                    "spitfire.border.active: invalid color, keeping the default"
                );
            }
        }
        if let Ok(v) = b.get::<String>("inactive") {
            if let Some(c) = parse_hex_color(&v) {
                border.inactive = c;
            } else {
                warn!(
                    value = v,
                    "spitfire.border.inactive: invalid color, keeping the default"
                );
            }
        }
        if let Ok(v) = b.get::<i32>("radius") {
            border.radius = v;
        }
    }

    if let Ok(t) = spitfire.get::<Table>("bar") {
        if let Ok(v) = t.get::<bool>("enable") {
            bar.enabled = v;
        }
        if let Ok(v) = t.get::<i32>("height") {
            bar.height = v;
        }
        if let Ok(v) = t.get::<String>("bg") {
            match parse_hex_color(&v) {
                Some(c) => bar.bg = c,
                None => warn!(
                    value = v,
                    "spitfire.bar.bg: invalid color, keeping the default"
                ),
            }
        }
        if let Ok(v) = t.get::<String>("fg") {
            match parse_hex_color(&v) {
                Some(c) => bar.fg = c,
                None => warn!(
                    value = v,
                    "spitfire.bar.fg: invalid color, keeping the default"
                ),
            }
        }
        if let Ok(v) = t.get::<String>("fg_active") {
            match parse_hex_color(&v) {
                Some(c) => bar.fg_active = c,
                None => warn!(
                    value = v,
                    "spitfire.bar.fg_active: invalid color, keeping the default"
                ),
            }
        }
    }

    if let Ok(k) = spitfire.get::<Table>("keyboard") {
        if let Ok(v) = k.get::<String>("layout") {
            keyboard.layout = v;
        }
        if let Ok(v) = k.get::<String>("variant") {
            keyboard.variant = v;
        }
        if let Ok(v) = k.get::<String>("model") {
            keyboard.model = v;
        }
        if let Ok(v) = k.get::<String>("options") {
            keyboard.options = Some(v);
        }
        if let Ok(v) = k.get::<String>("rules") {
            keyboard.rules = v;
        }
    }

    Ok((gaps, border, bar, keyboard))
}

fn parse_hex_color(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim_start_matches('#'), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_color_accepts_with_and_without_hash() {
        assert_eq!(parse_hex_color("#7aa2f7"), Some(0x7aa2f7));
        assert_eq!(parse_hex_color("7aa2f7"), Some(0x7aa2f7));
        assert_eq!(parse_hex_color("not-a-color"), None);
    }
}
