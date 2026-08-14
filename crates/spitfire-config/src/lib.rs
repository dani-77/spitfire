//! spitfire's Lua config loader — a single declarative file, dwm
//! `config.h`-style, but in Lua and reloadable at runtime without a
//! recompile (`spitfire.reload()` / `spitfirectl reload` in Phase 3).
//!
//! Default path: [`Config::default_path`]
//! (`$XDG_CONFIG_HOME/spitfire/config.lua`, falling back to
//! `~/.config/spitfire/config.lua`).
//!
//! The functions exposed in Lua (`spitfire.bind`, `spitfire.spawn`, ...)
//! don't have access to compositor state — they only push [`Command`]s onto
//! a shared queue. Whoever loads the config is responsible for draining
//! that queue (via [`Config::invoke_bind`] or right after [`Config::load`])
//! and applying each `Command` to the compositor.

mod api;
mod bind;
mod command;
mod rule;

pub use bind::{Bind, Modifiers};
pub use command::Command;
pub use rule::WindowRule;
pub use xkbcommon::xkb::Keysym;

use mlua::Lua;
use std::{cell::RefCell, path::PathBuf, rc::Rc};
use tracing::warn;

/// `spitfire.border = { width = 2, active = "#7aa2f7", inactive = "#414868", radius = 8 }`.
///
/// `radius`: corner radius in logical pixels, `0` (the default) draws the
/// classic square-cornered border exactly as before — see
/// `render::border_elements`'s doc comment for how a nonzero radius changes
/// what gets drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderConfig {
    pub width: i32,
    pub active: u32,
    pub inactive: u32,
    pub radius: i32,
}

impl Default for BorderConfig {
    fn default() -> Self {
        BorderConfig {
            width: 2,
            active: 0x7aa2f7,
            inactive: 0x414868,
            radius: 0,
        }
    }
}

/// `spitfire.anim = { enabled = true, duration = 150 }` — open/move-resize
/// window animations. `duration` is milliseconds. `enabled = false` or
/// `duration <= 0` disables both; purely visual, doesn't affect layout,
/// focus, or hit-testing (see `crate::anim` in the `spitfire` crate).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimConfig {
    pub enabled: bool,
    pub duration_ms: i32,
}

impl Default for AnimConfig {
    fn default() -> Self {
        AnimConfig {
            enabled: true,
            duration_ms: 150,
        }
    }
}

impl AnimConfig {
    pub fn duration(&self) -> std::time::Duration {
        if self.enabled && self.duration_ms > 0 {
            std::time::Duration::from_millis(self.duration_ms as u64)
        } else {
            std::time::Duration::ZERO
        }
    }
}

/// `spitfire.bar = { enable = true, height = 24, bg = "#1e1e2e", fg = "#6c7086", fg_active = "#cdd6f4" }`.
///
/// Phase 8, off by default. Drawn by the compositor itself, not a client —
/// see `crate::bar` in the `spitfire` crate (there is no protocol/IPC
/// involved, so nothing to expose from this crate beyond these colors).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarConfig {
    pub enabled: bool,
    pub height: i32,
    pub bg: u32,
    pub fg: u32,
    pub fg_active: u32,
}

impl Default for BarConfig {
    fn default() -> Self {
        BarConfig {
            enabled: false,
            height: 24,
            bg: 0x1e1e2e,
            fg: 0x6c7086,
            fg_active: 0xcdd6f4,
        }
    }
}

/// `spitfire.output = { scale = 1.0 }`.
///
/// Niri-style output scale: a fractional multiplier applied to every output
/// at startup (`1.0` is the classic 1:1 behavior, unchanged from before this
/// existed). Purely a starting value — `Mod+Shift+P`/`M` already rescale
/// outputs live at runtime (see `KeyAction::ScaleUp`/`ScaleDown` in
/// `input_handler.rs`); this just seeds that same mechanism instead of
/// always starting at `1.0`, and re-applies on `spitfire.reload()` too.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputConfig {
    pub scale: f64,
}

impl Default for OutputConfig {
    fn default() -> Self {
        OutputConfig { scale: 1.0 }
    }
}

/// `spitfire.keyboard = { layout = "pt", variant = "", model = "", options = "", rules = "" }`.
///
/// Same fields/meaning as `xkbcommon`'s `XkbConfig` (and `setxkbmap`'s
/// flags of the same names) — empty string means "let xkbcommon pick its
/// own default", which for `layout` in practice resolves to "us". Applied
/// once at startup and again on every `spitfire.reload()` (the keyboard
/// isn't recreated, just re-keymapped — see `KeyboardHandle::set_xkb_config`
/// in the `spitfire` crate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardConfig {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: Option<String>,
    /// `spitfire.keyboard.repeat_delay` (ms) and `.repeat_rate` (repeats
    /// per second) — sent to clients via `wl_keyboard.repeat_info`; the
    /// compositor itself never synthesizes repeat key events, each client
    /// runs its own repeat timer off these two numbers. Defaults (600ms /
    /// 25) match typical desktop norms (GNOME/KDE/X11 sit in the 450-660ms
    /// range). A too-tight `repeat_delay` isn't cosmetic: measured against
    /// a live session log, ~8% of ordinary keystrokes hold the key for
    /// 200ms or longer (median hold ~120ms, but the tail is long) — with a
    /// 200ms delay, every one of those spuriously starts the client's repeat
    /// timer, which reads as "keys repeating randomly" even though the
    /// compositor forwarded one clean press/release pair, every time.
    pub repeat_delay: i32,
    pub repeat_rate: i32,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        KeyboardConfig {
            rules: String::new(),
            model: String::new(),
            layout: String::new(),
            variant: String::new(),
            options: None,
            repeat_delay: 600,
            repeat_rate: 25,
        }
    }
}

/// A loaded Lua config: keeps the Lua interpreter alive (binds keep
/// closures in its registry) plus the data already extracted from the
/// global `spitfire` table after the script has run.
pub struct Config {
    lua: Lua,
    binds: Rc<RefCell<Vec<Bind>>>,
    rules: Rc<RefCell<Vec<WindowRule>>>,
    commands: Rc<RefCell<Vec<Command>>>,
    pub autostart: Vec<String>,
    pub gaps: spitfire_layout::Gaps,
    pub border: BorderConfig,
    pub bar: BarConfig,
    pub keyboard: KeyboardConfig,
    pub output: OutputConfig,
    pub anim: AnimConfig,
    /// `spitfire.focus_follows_mouse = true` — sloppy focus: hovering a
    /// window gives it keyboard focus without raising/reordering it
    /// (raising stays click-only). Hovering empty space (gaps, wallpaper,
    /// a layer-surface like a bar) leaves focus wherever it was. Off by
    /// default — click-to-focus keeps working exactly as before unless
    /// this is turned on. See `SpitfireState::update_keyboard_focus_hover`
    /// in the `spitfire` crate.
    pub focus_follows_mouse: bool,
}

impl Config {
    /// `$XDG_CONFIG_HOME/spitfire/config.lua`, falling back to
    /// `~/.config/spitfire/config.lua`.
    pub fn default_path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join("spitfire/config.lua");
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        PathBuf::from(home).join(".config/spitfire/config.lua")
    }

    /// Loads and runs the file at `path`.
    ///
    /// If it doesn't exist, that's not an error — returns a config with
    /// default values (no binds, default gaps/border) and a log warning:
    /// spitfire keeps starting with no Lua config at all, same as dwm
    /// starting with the sample `config.h` if you never touch it.
    pub fn load(path: &std::path::Path) -> mlua::Result<Config> {
        let lua = Lua::new();
        let commands = Rc::new(RefCell::new(Vec::new()));
        let binds = Rc::new(RefCell::new(Vec::new()));
        let autostart = Rc::new(RefCell::new(Vec::new()));
        let rules = Rc::new(RefCell::new(Vec::new()));

        api::install(
            &lua,
            commands.clone(),
            binds.clone(),
            autostart.clone(),
            rules.clone(),
        )?;

        match std::fs::read_to_string(path) {
            Ok(src) => {
                lua.load(&src).set_name(path.to_string_lossy()).exec()?;
            }
            Err(err) => {
                warn!(
                    path = %path.display(),
                    %err,
                    "no Lua config file found, starting with default values"
                );
            }
        }

        let (gaps, border, bar, keyboard, output, anim, focus_follows_mouse) =
            api::read_globals(&lua)?;
        // Can't `Rc::try_unwrap` here: the `spitfire.autostart` closure kept
        // inside `lua` holds a live reference to this Rc for as long as
        // `lua` exists. `autostart` only ever gets filled in the script's
        // top-level body (not from binds that fire later), so a plain
        // clone of its contents is enough.
        let autostart = autostart.borrow().clone();

        Ok(Config {
            lua,
            binds,
            rules,
            commands,
            autostart,
            gaps,
            border,
            bar,
            keyboard,
            output,
            anim,
            focus_follows_mouse,
        })
    }

    /// Every `Command` pushed since the last call — by the script's
    /// top-level body (calls outside of any bind) or by the last
    /// [`Config::invoke_bind`].
    pub fn drain_commands(&self) -> Vec<Command> {
        self.commands.borrow_mut().drain(..).collect()
    }

    /// Finds the first bind whose modifiers and key match the event
    /// exactly — returns an index, not the `Bind` itself, so callers never
    /// need to see `mlua::RegistryKey`.
    pub fn find_bind(&self, mods: Modifiers, keysym: Keysym) -> Option<usize> {
        self.binds
            .borrow()
            .iter()
            .position(|b| b.matches(mods, keysym))
    }

    /// Invokes bind `idx` (calls the Lua closure) and returns the
    /// `Command`s it — and anything it called — pushed.
    pub fn invoke_bind(&self, idx: usize) -> mlua::Result<Vec<Command>> {
        {
            let binds = self.binds.borrow();
            let bind = binds
                .get(idx)
                .ok_or_else(|| mlua::Error::RuntimeError(format!("invalid bind: {idx}")))?;
            let func: mlua::Function = self.lua.registry_value(&bind.callback)?;
            func.call::<()>(())?;
        }
        Ok(self.drain_commands())
    }

    /// The registered `spitfire.rule({...})` rules.
    pub fn rules(&self) -> std::cell::Ref<'_, Vec<WindowRule>> {
        self.rules.borrow()
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("binds", &self.binds.borrow().len())
            .field("rules", &self.rules.borrow().len())
            .field("autostart", &self.autostart)
            .field("gaps", &self.gaps)
            .field("border", &self.border)
            .field("bar", &self.bar)
            .field("keyboard", &self.keyboard)
            .field("output", &self.output)
            .field("anim", &self.anim)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_str(src: &str) -> Config {
        let mut file = tempfile();
        file.write_all(src.as_bytes()).unwrap();
        Config::load(file.path()).unwrap()
    }

    // A tiny tempfile helper so we don't need the `tempfile` crate just
    // for tests: a file in the test process's own temp directory.
    struct TempFile {
        path: PathBuf,
        file: std::fs::File,
    }
    impl TempFile {
        fn path(&self) -> &std::path::Path {
            &self.path
        }
        fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
            use std::io::Write as _;
            self.file.write_all(buf)?;
            self.file.flush()
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
    fn tempfile() -> TempFile {
        let path = std::env::temp_dir().join(format!(
            "spitfire-config-test-{}-{:?}.lua",
            std::process::id(),
            std::thread::current().id()
        ));
        let file = std::fs::File::create(&path).unwrap();
        TempFile { path, file }
    }

    #[test]
    fn missing_config_file_falls_back_to_defaults() {
        let config =
            Config::load(std::path::Path::new("/nonexistent/spitfire/config.lua")).unwrap();
        assert!(config.drain_commands().is_empty());
        assert_eq!(config.gaps, spitfire_layout::Gaps::default());
        assert_eq!(config.autostart, Vec::<String>::new());
    }

    #[test]
    fn reads_gaps_and_border_tables() {
        let config = load_str(
            r##"
            spitfire.gaps = { inner = 4, outer = 12 }
            spitfire.border = { width = 3, active = "#ff0000", inactive = "#00ff00" }
            "##,
        );
        assert_eq!(config.gaps.inner, 4);
        assert_eq!(config.gaps.outer, 12);
        assert_eq!(config.border.width, 3);
        assert_eq!(config.border.active, 0xff0000);
        assert_eq!(config.border.inactive, 0x00ff00);
        assert_eq!(config.border.radius, 0);
    }

    #[test]
    fn reads_border_radius() {
        let config = load_str(r#"spitfire.border = { radius = 10 }"#);
        assert_eq!(config.border.radius, 10);
    }

    #[test]
    fn bar_is_disabled_by_default() {
        let config = load_str("");
        assert!(!config.bar.enabled);
    }

    #[test]
    fn reads_bar_table() {
        let config = load_str(
            r##"
            spitfire.bar = { enable = true, height = 30, bg = "#000000", fg = "#ffffff", fg_active = "#ff00ff" }
            "##,
        );
        assert!(config.bar.enabled);
        assert_eq!(config.bar.height, 30);
        assert_eq!(config.bar.bg, 0x000000);
        assert_eq!(config.bar.fg, 0xffffff);
        assert_eq!(config.bar.fg_active, 0xff00ff);
    }

    #[test]
    fn output_scale_defaults_to_one() {
        let config = load_str("");
        assert_eq!(config.output.scale, 1.0);
    }

    #[test]
    fn reads_output_scale() {
        let config = load_str(r#"spitfire.output = { scale = 1.25 }"#);
        assert_eq!(config.output.scale, 1.25);
    }

    #[test]
    fn rejects_sub_one_output_scale() {
        let config = load_str(r#"spitfire.output = { scale = 0.5 }"#);
        assert_eq!(config.output.scale, 1.0);
    }

    #[test]
    fn keyboard_repeat_defaults_are_not_too_tight() {
        let config = load_str("");
        assert_eq!(config.keyboard.repeat_delay, 600);
        assert_eq!(config.keyboard.repeat_rate, 25);
    }

    #[test]
    fn reads_keyboard_repeat_settings() {
        let config = load_str(r#"spitfire.keyboard = { repeat_delay = 450, repeat_rate = 30 }"#);
        assert_eq!(config.keyboard.repeat_delay, 450);
        assert_eq!(config.keyboard.repeat_rate, 30);
    }

    #[test]
    fn rejects_non_positive_keyboard_repeat_settings() {
        let config = load_str(r#"spitfire.keyboard = { repeat_delay = 0, repeat_rate = -1 }"#);
        assert_eq!(config.keyboard.repeat_delay, 600);
        assert_eq!(config.keyboard.repeat_rate, 25);
    }

    #[test]
    fn collects_autostart_commands() {
        let config = load_str(r#"spitfire.autostart({ "foo", "bar" })"#);
        assert_eq!(config.autostart, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn collects_rules() {
        let config = load_str(r#"spitfire.rule({ app_id = "pavucontrol", floating = true })"#);
        let rules = config.rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].app_id.as_deref(), Some("pavucontrol"));
        assert!(rules[0].floating);
        assert!(!rules[0].centered);
    }

    #[test]
    fn collects_centered_rule() {
        let config = load_str(
            r#"spitfire.rule({ app_id = "pavucontrol", floating = true, centered = true })"#,
        );
        let rules = config.rules();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].floating);
        assert!(rules[0].centered);
    }

    #[test]
    fn bind_fires_and_produces_commands() {
        let config = load_str(
            r#"
            spitfire.bind("Mod4", "t", function()
                spitfire.layout.set("tile")
                spitfire.spawn("echo hi")
            end)
            "#,
        );
        let mods = Modifiers {
            logo: true,
            ..Default::default()
        };
        let keysym = xkbcommon::xkb::keysym_from_name("t", xkbcommon::xkb::KEYSYM_NO_FLAGS);
        let idx = config.find_bind(mods, keysym).expect("bind not found");
        let commands = config.invoke_bind(idx).unwrap();
        assert_eq!(
            commands,
            vec![
                Command::LayoutSet(spitfire_layout::LayoutMode::Tile),
                Command::Spawn("echo hi".to_string()),
            ]
        );
    }

    #[test]
    fn bind_does_not_match_with_extra_modifiers_held() {
        let config = load_str(r#"spitfire.bind("Mod4", "t", function() end)"#);
        let mods = Modifiers {
            logo: true,
            shift: true,
            ..Default::default()
        };
        let keysym = xkbcommon::xkb::keysym_from_name("t", xkbcommon::xkb::KEYSYM_NO_FLAGS);
        assert!(config.find_bind(mods, keysym).is_none());
    }

    #[test]
    fn bind_with_shift_matches_the_shifted_uppercase_keysym() {
        // Real key events report the *shifted* keysym while Shift is held
        // (uppercase R, not lowercase r) — every config spells keys
        // lowercase, so this has to match anyway. Regression test for a
        // real-hardware bug: Mod4+Shift+r (spitfire.reload()) silently
        // never fired, only the nested --winit path had happened to be
        // tested before.
        let config =
            load_str(r#"spitfire.bind("Mod4+Shift", "r", function() spitfire.reload() end)"#);
        let mods = Modifiers {
            logo: true,
            shift: true,
            ..Default::default()
        };
        let shifted_keysym = xkbcommon::xkb::keysym_from_name("R", xkbcommon::xkb::KEYSYM_NO_FLAGS);
        let idx = config
            .find_bind(mods, shifted_keysym)
            .expect("bind not found");
        assert_eq!(
            config.invoke_bind(idx).unwrap(),
            vec![Command::ReloadConfig]
        );
    }

    #[test]
    fn top_level_calls_outside_any_bind_are_queued_too() {
        let config = load_str(r#"spitfire.spawn("qs -p frontend/utumno")"#);
        assert_eq!(
            config.drain_commands(),
            vec![Command::Spawn("qs -p frontend/utumno".to_string())]
        );
    }
}
