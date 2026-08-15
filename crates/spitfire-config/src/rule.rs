/// A `spitfire.rule({ app_id = "...", floating = true, centered = true,
/// hide_from_capture = true })` rule.
///
/// `floating`: windows whose `app_id` matches the rule are left out of the
/// tiling order entirely (the layout engine never touches their geometry).
/// `centered`: only meaningful alongside `floating` — the window is placed
/// in the middle of the output's usable area the first time it maps,
/// instead of wherever `place_new_window`'s cascade would have put it.
/// `hide_from_capture`: the window is skipped entirely (not composited at
/// all, leaving whatever's behind it — wallpaper, another window) in
/// `wlr-screencopy` captures, while staying fully visible on the real
/// screen — see `crate::screencopy`'s use of this via `render::
/// output_elements`'s `hidden_windows` param. A privacy flag for a window
/// with sensitive on-screen content (a password manager, a DM) that you
/// still want visible to you but never in a screenshot/recording/share.
/// `workspace = n` is left for Phase 5, once more than one workspace
/// exists.
#[derive(Debug, Clone, Default)]
pub struct WindowRule {
    pub app_id: Option<String>,
    pub floating: bool,
    pub centered: bool,
    pub hide_from_capture: bool,
}

impl WindowRule {
    /// `None` in `app_id` acts as a universal pattern (matches every
    /// window) — only useful for app_id-less rules that touch something
    /// else; that's never the case yet, but this stays ready for it.
    pub fn matches(&self, app_id: Option<&str>) -> bool {
        match (&self.app_id, app_id) {
            (Some(pattern), Some(actual)) => pattern == actual,
            (Some(_), None) => false,
            (None, _) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_exact_app_id() {
        let rule = WindowRule {
            app_id: Some("pavucontrol".into()),
            floating: true,
            centered: false,
            hide_from_capture: false,
        };
        assert!(rule.matches(Some("pavucontrol")));
        assert!(!rule.matches(Some("foot")));
        assert!(!rule.matches(None));
    }

    #[test]
    fn rule_without_app_id_matches_everything() {
        let rule = WindowRule {
            app_id: None,
            floating: true,
            centered: false,
            hide_from_capture: false,
        };
        assert!(rule.matches(Some("anything")));
        assert!(rule.matches(None));
    }

    #[test]
    fn hide_from_capture_defaults_off() {
        assert!(!WindowRule::default().hide_from_capture);
    }
}
