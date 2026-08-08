#![warn(rust_2018_idioms)]

pub mod bar;
// Loads the default XCursor pointer image — used by XWayland (the default
// cursor handed to X11 clients that don't set their own) and by the
// DRM/KMS backend (the only backends that ever have to draw the cursor
// themselves; a nested winit window gets it for free from the host).
#[cfg(any(feature = "xwayland", feature = "udev"))]
pub mod cursor;
pub mod drawing;
pub mod ext_workspace;
pub mod focus;
pub mod input_handler;
pub mod ipc;
pub mod layout;
pub mod render;
pub mod shell;
pub mod state;
#[cfg(feature = "udev")]
pub mod udev;
#[cfg(feature = "winit")]
pub mod winit;
pub mod workspace;

pub use state::{ClientState, SpitfireState};
