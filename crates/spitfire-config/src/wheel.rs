use tracing::warn;

use crate::bind::Modifiers;

/// Which way a mouse wheel notch turned — vertical wheel up/down, or
/// horizontal (tilt, or a second wheel) left/right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelDirection {
    Up,
    Down,
    Left,
    Right,
}

impl WheelDirection {
    /// Parses `"up"`/`"down"`/`"left"`/`"right"` (case-insensitive).
    /// Unknown strings are ignored with a warning, same treatment
    /// `GestureDirection::parse` gives a misspelled gesture direction.
    pub fn parse(spec: &str) -> Option<WheelDirection> {
        match spec.to_ascii_lowercase().as_str() {
            "up" => Some(WheelDirection::Up),
            "down" => Some(WheelDirection::Down),
            "left" => Some(WheelDirection::Left),
            "right" => Some(WheelDirection::Right),
            other => {
                warn!(direction = other, "unknown direction in a spitfire.wheel");
                None
            }
        }
    }
}

/// An already-resolved `spitfire.wheel(mods, direction, function)` — same
/// shape as [`crate::bind::Bind`], matched on modifiers + wheel direction
/// instead of modifiers + keysym.
pub struct WheelBind {
    pub mods: Modifiers,
    pub direction: WheelDirection,
    pub(crate) callback: mlua::RegistryKey,
}

impl WheelBind {
    pub(crate) fn matches(&self, mods: Modifiers, direction: WheelDirection) -> bool {
        self.mods == mods && self.direction == direction
    }
}

impl std::fmt::Debug for WheelBind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WheelBind")
            .field("mods", &self.mods)
            .field("direction", &self.direction)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_directions_case_insensitively() {
        assert_eq!(WheelDirection::parse("Up"), Some(WheelDirection::Up));
        assert_eq!(WheelDirection::parse("DOWN"), Some(WheelDirection::Down));
        assert_eq!(WheelDirection::parse("left"), Some(WheelDirection::Left));
        assert_eq!(WheelDirection::parse("Right"), Some(WheelDirection::Right));
    }

    #[test]
    fn unknown_direction_is_none_not_fatal() {
        assert_eq!(WheelDirection::parse("sideways"), None);
    }
}
