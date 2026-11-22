//! X11 application support through `xwayland-satellite`, as niri does.
//!
//! xwayland-satellite runs Xwayland rootless and is the X11 window manager
//! itself: every X11 window reaches spitfire as an ordinary Wayland client
//! (`xdg_toplevel`), and X11 menus and popups as `xdg_popup`s placed relative
//! to their parent. They go through the same code as native Wayland
//! clients, instead of spitfire's own XWM (`shell/x11.rs`), where X11
//! coordinates kept drifting from the tiling layout (Steam's menus opened
//! at the screen's top-left, then shifted while hovered).
//!
//! The trade-off, documented upstream: X11 clients that place windows at
//! absolute screen coordinates (Steam's slide-in notifications) can't, since
//! every toplevel believes it sits at 0,0.
//!
//! Used when `xwayland-satellite` is in `$PATH`; `SPITFIRE_XWAYLAND=builtin`
//! keeps the built-in Xwayland (when compiled with the `xwayland` feature).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// How many times in a row a crashing satellite is restarted before giving
/// up, and the delay before each restart.
const MAX_RESTARTS: u32 = 5;
const RESTART_DELAY: Duration = Duration::from_secs(1);
/// A satellite that ran at least this long resets the restart count.
const STABLE_RUN: Duration = Duration::from_secs(60);

/// `xwayland-satellite` in `$PATH`, unless the built-in Xwayland is asked
/// for.
pub fn binary() -> Option<PathBuf> {
    if std::env::var("SPITFIRE_XWAYLAND").as_deref() == Ok("builtin") {
        return None;
    }
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|dir| Path::new(dir).join("xwayland-satellite"))
        .find(|path| path.is_file())
}

/// The first X display number with neither a socket nor a lock file.
fn free_display() -> Option<u32> {
    (0..64).find(|n| {
        !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
            && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
    })
}

/// Starts `binary` on a free display for the Wayland socket `wayland_display`
/// and exports `DISPLAY` for everything spitfire launches from now on
/// (autostart included). The satellite is restarted if it exits while
/// spitfire still runs. Returns the display number.
pub fn start(binary: PathBuf, wayland_display: &str, running: Arc<AtomicBool>) -> Option<u32> {
    let Some(display_number) = free_display() else {
        warn!("No free X display_number number for xwayland-satellite");
        return None;
    };
    let spawn = {
        let binary = binary.clone();
        let wayland_display = wayland_display.to_string();
        move || {
            Command::new(&binary)
                .arg(format!(":{display_number}"))
                .env("WAYLAND_DISPLAY", &wayland_display)
                .stdin(Stdio::null())
                .spawn()
        }
    };
    let mut child = match spawn() {
        Ok(child) => child,
        Err(err) => {
            warn!(%err, ?binary, "Failed to start xwayland-satellite");
            return None;
        }
    };
    // Set now, like the built-in Xwayland: a client connecting before the
    // satellite has its socket up just waits for it.
    std::env::set_var("DISPLAY", format!(":{display_number}"));
    info!(display_number, "X11 apps through xwayland-satellite");

    let _ = std::thread::Builder::new()
        .name("xwayland-satellite".into())
        .spawn(move || {
            let mut restarts = 0;
            loop {
                let started = Instant::now();
                let status = child.wait();
                if !running.load(Ordering::SeqCst) {
                    return;
                }
                if started.elapsed() >= STABLE_RUN {
                    restarts = 0;
                }
                restarts += 1;
                if restarts > MAX_RESTARTS {
                    warn!("xwayland-satellite keeps exiting; X11 apps won't work this session");
                    return;
                }
                warn!(?status, "xwayland-satellite exited; restarting");
                std::thread::sleep(RESTART_DELAY);
                match spawn() {
                    Ok(next) => child = next,
                    Err(err) => {
                        warn!(%err, "Failed to restart xwayland-satellite");
                        return;
                    }
                }
            }
        });
    Some(display_number)
}
