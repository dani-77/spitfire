use tracing::warn;

/// Direction a touchpad swipe gesture ended up classified as.
///
/// A raw swipe is just an accumulated `(dx, dy)` in logical pixels, summed
/// across every `GestureSwipeUpdate` between `GestureSwipeBegin` and
/// `GestureSwipeEnd` — see [`GestureDirection::classify`] for how that
/// becomes one of these four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureDirection {
    Left,
    Right,
    Up,
    Down,
}

impl GestureDirection {
    /// Parses `"left"`/`"right"`/`"up"`/`"down"` (case-insensitive).
    /// Unknown strings are ignored with a warning — a misspelled direction
    /// in `spitfire.gesture` ends up harmless (never registered, never
    /// fires) instead of aborting the whole config load, same treatment
    /// `Modifiers::parse` gives an unknown modifier name.
    pub fn parse(spec: &str) -> Option<GestureDirection> {
        match spec.to_ascii_lowercase().as_str() {
            "left" => Some(GestureDirection::Left),
            "right" => Some(GestureDirection::Right),
            "up" => Some(GestureDirection::Up),
            "down" => Some(GestureDirection::Down),
            other => {
                warn!(direction = other, "unknown direction in a spitfire.gesture");
                None
            }
        }
    }

    /// Classifies a swipe by its dominant axis: whichever of `dx`/`dy`
    /// moved further (in absolute value) decides horizontal vs vertical,
    /// its sign decides which way. Matches the classifier the user's other
    /// compositor (wasp, `~/Projectos/wasp`) already uses for its own
    /// touchpad gestures, and the same "classify once, at the end" approach
    /// niri/GNOME take — a gesture can wobble diagonally mid-swipe without
    /// flipping its eventual direction back and forth.
    pub fn classify(dx: f64, dy: f64) -> GestureDirection {
        if dx.abs() >= dy.abs() {
            if dx >= 0.0 {
                GestureDirection::Right
            } else {
                GestureDirection::Left
            }
        } else if dy >= 0.0 {
            GestureDirection::Down
        } else {
            GestureDirection::Up
        }
    }
}

/// An already-resolved `spitfire.gesture(fingers, direction, function)`:
/// finger count and direction ready to compare against a real swipe, plus
/// the Lua closure kept in the registry (only ever invoked, never copied
/// out) — same shape as [`crate::bind::Bind`], just matched on
/// fingers+direction instead of modifiers+keysym.
///
/// `fingers == 0` matches any finger count — same "0/omitted = any"
/// convention wasp documents for its own `wasp.gestures`.
pub struct Gesture {
    pub fingers: u32,
    pub direction: GestureDirection,
    pub(crate) callback: mlua::RegistryKey,
}

impl Gesture {
    /// Whether this entry's finger count would ever match a real swipe with
    /// this many fingers — checked alone (direction not knowable yet) at
    /// `GestureSwipeBegin`, to decide whether the whole sequence should be
    /// intercepted before a single update has happened.
    pub(crate) fn matches_fingers(&self, fingers: u32) -> bool {
        self.fingers == 0 || self.fingers == fingers
    }

    pub(crate) fn matches(&self, fingers: u32, direction: GestureDirection) -> bool {
        self.matches_fingers(fingers) && self.direction == direction
    }
}

impl std::fmt::Debug for Gesture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gesture")
            .field("fingers", &self.fingers)
            .field("direction", &self.direction)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_directions_case_insensitively() {
        assert_eq!(
            GestureDirection::parse("Left"),
            Some(GestureDirection::Left)
        );
        assert_eq!(
            GestureDirection::parse("RIGHT"),
            Some(GestureDirection::Right)
        );
        assert_eq!(GestureDirection::parse("up"), Some(GestureDirection::Up));
        assert_eq!(
            GestureDirection::parse("Down"),
            Some(GestureDirection::Down)
        );
    }

    #[test]
    fn unknown_direction_is_none_not_fatal() {
        assert_eq!(GestureDirection::parse("sideways"), None);
    }

    #[test]
    fn classifies_dominant_horizontal_axis() {
        assert_eq!(
            GestureDirection::classify(50.0, 10.0),
            GestureDirection::Right
        );
        assert_eq!(
            GestureDirection::classify(-50.0, 10.0),
            GestureDirection::Left
        );
    }

    #[test]
    fn classifies_dominant_vertical_axis() {
        assert_eq!(
            GestureDirection::classify(5.0, 40.0),
            GestureDirection::Down
        );
        assert_eq!(GestureDirection::classify(5.0, -40.0), GestureDirection::Up);
    }

    // `Gesture::matches_fingers`/`matches` (including the `fingers == 0`
    // "any" case) can't be exercised here without a real `mlua::RegistryKey`
    // — see `lib.rs`'s own test module for end-to-end coverage via
    // `Config::has_gesture_for_fingers`/`find_gesture`.
}
