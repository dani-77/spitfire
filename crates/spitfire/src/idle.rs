//! Idle: ext-idle-notify-v1 and zwp-idle-inhibit-v1.
//!
//! ext-idle-notify lets an idle daemon such as swayidle find out that the
//! user has stopped touching the keyboard, mouse or touchpad, so it can lock
//! the screen (through ext-session-lock, Utumno's lockscreen) after a while.
//! Every input event resets the timers (`notify_idle_activity`, called from
//! the input handlers).
//!
//! zwp-idle-inhibit lets a client keep the session awake, e.g. a video
//! player or a browser playing a video. Simplest possible rule: while any
//! still-alive surface holds an inhibitor, idle timers don't fire — whether
//! or not that surface is visible right now.

use smithay::{
    delegate_idle_inhibit, delegate_idle_notify,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    wayland::{
        idle_inhibit::IdleInhibitHandler,
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
    },
};

use crate::state::{Backend, SpitfireState};

impl<BackendData: Backend + 'static> IdleNotifierHandler for SpitfireState<BackendData> {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
    }
}
delegate_idle_notify!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend + 'static> IdleInhibitHandler for SpitfireState<BackendData> {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibiting_surfaces.push(surface);
        self.update_idle_inhibit();
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibiting_surfaces.retain(|s| s != &surface);
        self.update_idle_inhibit();
    }
}
delegate_idle_inhibit!(@<BackendData: Backend + 'static> SpitfireState<BackendData>);

impl<BackendData: Backend + 'static> SpitfireState<BackendData> {
    /// Inhibited while any surface that asked for it is still alive.
    pub fn update_idle_inhibit(&mut self) {
        self.idle_inhibiting_surfaces.retain(|s| s.is_alive());
        let inhibited = !self.idle_inhibiting_surfaces.is_empty();
        self.idle_notifier_state.set_is_inhibited(inhibited);
    }

    /// Any input: the user is not idle, restart every idle timer.
    pub fn notify_idle_activity(&mut self) {
        let seat = self.seat.clone();
        self.idle_notifier_state.notify_activity(&seat);
    }
}
