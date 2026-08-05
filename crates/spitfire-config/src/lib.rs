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

/// `spitfire.border = { width = 2, active = "#7aa2f7", inactive = "#414868" }`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderConfig {
    pub width: i32,
    pub active: u32,
    pub inactive: u32,
}

impl Default for BorderConfig {
    fn default() -> Self {
        BorderConfig {
            width: 2,
            active: 0x7aa2f7,
            inactive: 0x414868,
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

        api::install(&lua, commands.clone(), binds.clone(), autostart.clone(), rules.clone())?;

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

        let (gaps, border, bar) = api::read_gaps_border_bar(&lua)?;
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
        self.binds.borrow().iter().position(|b| b.matches(mods, keysym))
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
        let config = Config::load(std::path::Path::new("/nonexistent/spitfire/config.lua")).unwrap();
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
    fn top_level_calls_outside_any_bind_are_queued_too() {
        let config = load_str(r#"spitfire.spawn("qs -p frontend/utumno")"#);
        assert_eq!(
            config.drain_commands(),
            vec![Command::Spawn("qs -p frontend/utumno".to_string())]
        );
    }
}
