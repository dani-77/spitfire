//! spitfire — a Wayland compositor (Smithay).
//!
//! Adapted from Smithay's own `anvil` example (MIT/Apache-2.0). Two
//! backends: `--winit` (default, nested inside an existing session) and
//! `--udev` (opt-in `udev` cargo feature — DRM/KMS + libinput, runs as the
//! real login session). XWayland (X11 application support) is a separate
//! opt-in `xwayland` cargo feature, orthogonal to either backend. See the
//! plan at `/home/dani77/.claude/plans/sparkling-shimmying-jellyfish.md`.

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
        #[cfg(feature = "winit")]
        None | Some("--winit") => {
            tracing::info!("Starting spitfire with the winit backend");
            spitfire::winit::run_winit();
        }
        #[cfg(feature = "udev")]
        Some("--udev") => {
            tracing::info!("Starting spitfire with the DRM/KMS (udev) backend");
            spitfire::udev::run_udev();
        }
        // Same "no arguments" default as above, but for a "winit"-less
        // build (e.g. a minimal --udev-only binary) — there's no nested
        // mode to fall back to, so go straight for the real session
        // backend instead of erroring out on the plain, no-flag invocation.
        #[cfg(all(not(feature = "winit"), feature = "udev"))]
        None => {
            tracing::info!("Starting spitfire with the DRM/KMS (udev) backend");
            spitfire::udev::run_udev();
        }
        #[cfg(not(any(feature = "winit", feature = "udev")))]
        None => {
            tracing::error!(
                "Built without both the \"winit\" and \"udev\" features — no backend to start"
            );
        }
        Some(other) => {
            tracing::error!("Unknown backend: {other}");
            #[cfg(feature = "udev")]
            eprintln!("USAGE: spitfire [--winit | --udev]");
            #[cfg(not(feature = "udev"))]
            eprintln!("USAGE: spitfire [--winit]");
        }
    }
}
