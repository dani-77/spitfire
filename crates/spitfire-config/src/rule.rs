/// A `spitfire.rule({ app_id = "...", floating = true })` rule.
///
/// Phase 2 only implements `floating`: windows whose `app_id` matches the
/// rule are left out of the tiling order entirely (the layout engine never
/// touches their geometry). `workspace = n` is left for Phase 5, once more
/// than one workspace exists.
#[derive(Debug, Clone, Default)]
pub struct WindowRule {
    pub app_id: Option<String>,
    pub floating: bool,
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
        };
        assert!(rule.matches(Some("anything")));
        assert!(rule.matches(None));
    }
}
