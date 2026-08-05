//! spitfire — a Wayland compositor (Smithay), v1: winit backend only.
//!
//! Adapted from Smithay's own `anvil` example (MIT/Apache-2.0). The DRM/KMS
//! backend and XWayland are out of scope for now — see the plan at
//! `/home/dani77/.claude/plans/sparkling-shimmying-jellyfish.md`.

fn main() {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt()
            .compact()
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt().compact().init();
    }

    // So that autostarted clients (and anything else that cares) can tell
    // they're running under spitfire, the same convention niri/Hyprland/sway
    // follow. Only affects processes we spawn after this point — has no
    // effect on whatever launched spitfire itself.
    // Safety: single-threaded at this point in startup, nothing else reads
    // or writes the environment concurrently yet.
    unsafe {
        std::env::set_var("XDG_CURRENT_DESKTOP", "spitfire");
    }

    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        // No arguments, or "--winit": run as a nested window in the current session.
        None | Some("--winit") => {
            tracing::info!("Starting spitfire with the winit backend");
            spitfire::winit::run_winit();
        }
        Some(other) => {
            tracing::error!("Unknown backend: {other}");
            eprintln!("USAGE: spitfire [--winit]");
        }
    }
}
