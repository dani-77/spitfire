use tracing::warn;
use xkbcommon::xkb::Keysym;

/// A bind's modifiers — comparison is always exact (same as dwm):
/// `spitfire.bind("Mod4", "t", ...)` only fires with *exactly* Super held,
/// not Super+Shift.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub logo: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    /// Parses strings like `"Mod4"`, `"Mod4+Shift"`, `"Ctrl+Alt"`.
    /// Unknown names are ignored with a warning — a bind with a misspelled
    /// modifier ends up harmless (never fires) instead of aborting the
    /// whole config load.
    pub fn parse(spec: &str) -> Modifiers {
        let mut mods = Modifiers::default();
        for part in spec.split('+') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match part.to_ascii_lowercase().as_str() {
                "mod4" | "super" | "logo" | "cmd" => mods.logo = true,
                "shift" => mods.shift = true,
                "ctrl" | "control" => mods.ctrl = true,
                "alt" | "mod1" => mods.alt = true,
                other => warn!(modifier = other, "unknown modifier in a spitfire.bind"),
            }
        }
        mods
    }
}

/// An already-resolved `spitfire.bind(mods, key, function)`: modifiers and
/// keysym ready to compare against real keyboard events, plus the Lua
/// closure kept in the registry (only ever invoked, never copied out).
pub struct Bind {
    pub mods: Modifiers,
    pub keysym: Keysym,
    pub(crate) callback: mlua::RegistryKey,
}

impl Bind {
    pub(crate) fn matches(&self, mods: Modifiers, keysym: Keysym) -> bool {
        self.mods == mods && normalize_keysym(self.keysym) == normalize_keysym(keysym)
    }
}

/// Folds a letter keysym to lowercase before comparing.
///
/// XKB reports the *shifted* keysym once Shift is actually held down — for
/// a letter that's the uppercase form (`Keysym::R`, not `Keysym::r`), even
/// though Shift is already its own bit in [`Modifiers`]. Without this,
/// `spitfire.bind("Mod4+Shift", "r", ...)` — every config so far spells
/// keys lowercase — could never fire: the parsed bind keysym is lowercase
/// `r`, but the real event's keysym is uppercase `R` while Shift is down,
/// and `Bind::matches` used to compare them literally. Only touches
/// letters; digits, `Return`, non-Latin keysyms, etc. pass through as-is.
fn normalize_keysym(sym: Keysym) -> Keysym {
    match sym.key_char() {
        Some(c) if c.is_ascii_alphabetic() => Keysym::from_char(c.to_ascii_lowercase()),
        _ => sym,
    }
}

impl std::fmt::Debug for Bind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bind")
            .field("mods", &self.mods)
            .field("keysym", &self.keysym)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_modifier() {
        let mods = Modifiers::parse("Mod4");
        assert_eq!(
            mods,
            Modifiers {
                logo: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn parses_combined_modifiers_case_insensitively() {
        let mods = Modifiers::parse("mod4+SHIFT");
        assert_eq!(
            mods,
            Modifiers {
                logo: true,
                shift: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn unknown_modifier_is_ignored_not_fatal() {
        let mods = Modifiers::parse("Mod4+Hyper");
        assert_eq!(
            mods,
            Modifiers {
                logo: true,
                ..Default::default()
            }
        );
    }
}
