//! Basic mangowm-style window animations: fade+scale-in on open, and a
//! tween when the tiling engine re-flows a window to a new rect. Both are
//! **purely a render-time visual transform** — the real compositor state
//! (`Space`-mapped geometry, `xdg_toplevel` configure size, focus,
//! hit-testing) is applied immediately and unchanged from before this
//! module existed; only what gets *drawn* is interpolated for
//! `spitfire.anim.duration` after the triggering event. See `render.rs`'s
//! `output_elements` for where the interpolated rect/alpha this module
//! computes actually gets turned into render elements.
//!
//! Deliberately out of scope: close-fade (no dedicated Wayland "window
//! destroyed" hook exists today, and a client's buffer can become
//! unrenderable mid-fade with no clean recovery) and workspace-switch slide
//! (needs simultaneous multi-workspace rendering). Interactive drag/resize
//! via `shell/grabs.rs` is never animated — it's already 1:1 with the
//! pointer.

use std::time::{Duration, Instant};

use smithay::{
    desktop::Space,
    utils::{IsAlive, Logical, Rectangle},
};

use crate::{
    shell::WindowElement,
    state::{Backend, SpitfireState},
};

/// Open animation starts at this fraction of the window's real size,
/// scaling up to `1.0` (about its own center) as it fades in.
const OPEN_START_SCALE: f64 = 0.9;

#[derive(Debug, Clone, Copy)]
enum AnimKind {
    Open,
    /// `to` isn't stored — it's always the window's *live* geometry,
    /// fetched fresh every frame (see `WindowAnimations::resolve_all`), so
    /// a pure resize naturally sits still until the client actually commits
    /// a new-size buffer, then finishes the remaining duration once it does.
    Move {
        from: Rectangle<i32, Logical>,
    },
}

#[derive(Debug, Clone)]
struct WindowAnim {
    window: WindowElement,
    kind: AnimKind,
    start: Instant,
    duration: Duration,
}

impl WindowAnim {
    /// `None` once finished (elapsed >= duration) or if `duration` is zero.
    fn progress(&self, now: Instant) -> Option<f32> {
        if self.duration.is_zero() {
            return None;
        }
        let elapsed = now.saturating_duration_since(self.start);
        if elapsed >= self.duration {
            return None;
        }
        let t = elapsed.as_secs_f32() / self.duration.as_secs_f32();
        Some(ease_out_cubic(t))
    }
}

/// Standard ease-out cubic, shared by both alpha/scale (Open) and the rect
/// lerp (Move) so both animations share one feel.
fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

fn lerp(a: i32, b: i32, t: f32) -> i32 {
    a + ((b - a) as f32 * t).round() as i32
}

fn lerp_rect(
    from: Rectangle<i32, Logical>,
    to: Rectangle<i32, Logical>,
    t: f32,
) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (lerp(from.loc.x, to.loc.x, t), lerp(from.loc.y, to.loc.y, t)).into(),
        (
            lerp(from.size.w, to.size.w, t).max(1),
            lerp(from.size.h, to.size.h, t).max(1),
        )
            .into(),
    )
}

/// Shrinks `rect` to `factor` of its size, about its own center.
fn scale_about_center(rect: Rectangle<i32, Logical>, factor: f64) -> Rectangle<i32, Logical> {
    let w = ((rect.size.w as f64) * factor).round() as i32;
    let h = ((rect.size.h as f64) * factor).round() as i32;
    let x = rect.loc.x + (rect.size.w - w) / 2;
    let y = rect.loc.y + (rect.size.h - h) / 2;
    Rectangle::new((x, y).into(), (w.max(1), h.max(1)).into())
}

/// What `render.rs` needs for one currently-animating window: the rect to
/// draw it at this frame, and its alpha.
pub struct AnimatedWindow {
    pub window: WindowElement,
    pub target: Rectangle<i32, Logical>,
    pub alpha: f32,
}

/// The set of windows currently animating. Never more than a handful at
/// once, so a linearly-scanned `Vec` (matching `focus_history`'s existing
/// shape on `SpitfireState`) is simpler than keying a map off `WindowElement`
/// (which has no `Hash` impl today).
#[derive(Debug, Default)]
pub struct WindowAnimations(Vec<WindowAnim>);

impl WindowAnimations {
    /// Registers an open (fade+scale-in) animation, replacing any in-flight
    /// Move for the same window — a window that just got its first buffer
    /// takes priority over wherever the tiling engine placed it before it
    /// had content.
    pub fn push_open(&mut self, window: WindowElement, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        self.0.retain(|a| a.window != window);
        self.0.push(WindowAnim {
            window,
            kind: AnimKind::Open,
            start: Instant::now(),
            duration,
        });
    }

    /// Registers a move/resize tween from `real_from` to the window's live
    /// geometry. Skipped entirely if an Open for the same window is still
    /// in flight (Open wins — it keeps tracking the window's live geometry
    /// every frame regardless, see `resolve_all`, so nothing is lost). If a
    /// previous Move for this window is still in flight, restarts from its
    /// *currently drawn* (interpolated) rect rather than `real_from`, so a
    /// rapid re-tile never visibly jumps.
    pub fn push_move(
        &mut self,
        window: &WindowElement,
        real_from: Rectangle<i32, Logical>,
        duration: Duration,
    ) {
        if duration.is_zero() {
            return;
        }
        let now = Instant::now();
        let from = match self.0.iter().find(|a| &a.window == window) {
            Some(existing) => match (existing.kind, existing.progress(now)) {
                (AnimKind::Open, Some(_)) => return,
                (AnimKind::Move { from }, Some(p)) => lerp_rect(from, real_from, p),
                _ => real_from,
            },
            None => real_from,
        };
        self.0.retain(|a| &a.window != window);
        self.0.push(WindowAnim {
            window: window.clone(),
            kind: AnimKind::Move { from },
            start: now,
            duration,
        });
    }

    /// Drops finished/dead entries. Call once per `arrange_tiling` — already
    /// runs every frame on both backends, so no new call site is needed.
    pub fn prune(&mut self) {
        let now = Instant::now();
        self.0
            .retain(|a| a.window.alive() && a.progress(now).is_some());
    }

    fn on_screen_rect_and_alpha(
        &self,
        window: &WindowElement,
        reference: Rectangle<i32, Logical>,
        now: Instant,
    ) -> Option<(Rectangle<i32, Logical>, f32)> {
        let anim = self.0.iter().find(|a| &a.window == window)?;
        let p = anim.progress(now)?;
        Some(match anim.kind {
            AnimKind::Move { from } => (lerp_rect(from, reference, p), 1.0),
            AnimKind::Open => (
                scale_about_center(
                    reference,
                    OPEN_START_SCALE + (1.0 - OPEN_START_SCALE) * p as f64,
                ),
                p,
            ),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Resolves every active animation against the live `Space` — what
    /// `render.rs` iterates each frame. `reference` (the window's real,
    /// already-current geometry) is fetched fresh here, never frozen at
    /// trigger time — see `AnimKind::Move`'s doc comment for why.
    pub fn resolve_all(&self, space: &Space<WindowElement>) -> Vec<AnimatedWindow> {
        let now = Instant::now();
        self.0
            .iter()
            .filter_map(|a| {
                let reference = space.element_geometry(&a.window)?;
                let (target, alpha) = self.on_screen_rect_and_alpha(&a.window, reference, now)?;
                Some(AnimatedWindow {
                    window: a.window.clone(),
                    target,
                    alpha,
                })
            })
            .collect()
    }

    /// Used by `Workspace::border_rects` so a window's `spitfire.border`
    /// tracks its animated on-screen rect instead of detaching from it
    /// mid-tween. Note: border *alpha* isn't animated (`border_elements`
    /// has no alpha input) — an opening window's border is instantly opaque
    /// around its fading/scaling content for the animation's duration.
    /// Deliberate scope cut, not an oversight.
    pub fn on_screen_rect(
        &self,
        window: &WindowElement,
        reference: Rectangle<i32, Logical>,
    ) -> Rectangle<i32, Logical> {
        self.on_screen_rect_and_alpha(window, reference, Instant::now())
            .map(|(rect, _)| rect)
            .unwrap_or(reference)
    }
}

impl<BackendData: Backend + 'static> SpitfireState<BackendData> {
    pub fn animated_windows(&self) -> Vec<AnimatedWindow> {
        self.window_anims.resolve_all(&self.space)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn ease_out_cubic_endpoints() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn ease_out_cubic_is_front_loaded() {
        // Ease-out: more progress happens early than a linear curve would.
        assert!(ease_out_cubic(0.5) > 0.5);
    }

    #[test]
    fn lerp_rect_halfway() {
        let from = rect(0, 0, 100, 100);
        let to = rect(100, 100, 200, 200);
        assert_eq!(lerp_rect(from, to, 0.5), rect(50, 50, 150, 150));
    }

    #[test]
    fn lerp_rect_at_t0_is_from_and_at_t1_is_to() {
        let from = rect(0, 0, 100, 100);
        let to = rect(40, 40, 200, 50);
        assert_eq!(lerp_rect(from, to, 0.0), from);
        assert_eq!(lerp_rect(from, to, 1.0), to);
    }

    #[test]
    fn scale_about_center_shrinks_and_recenters() {
        let r = rect(0, 0, 100, 100);
        let scaled = scale_about_center(r, 0.5);
        assert_eq!(scaled.size, (50, 50).into());
        // Centered: same center point as the original.
        assert_eq!(scaled.loc, (25, 25).into());
    }

    #[test]
    fn scale_about_center_at_factor_1_is_a_no_op() {
        let r = rect(10, 20, 100, 60);
        assert_eq!(scale_about_center(r, 1.0), r);
    }

    #[test]
    fn window_animations_starts_empty() {
        assert!(WindowAnimations::default().is_empty());
    }
}
