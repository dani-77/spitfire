#![warn(rust_2018_idioms)]

pub mod drawing;
pub mod ext_workspace;
pub mod focus;
pub mod input_handler;
pub mod ipc;
pub mod layout;
pub mod render;
pub mod shell;
pub mod state;
pub mod winit;
pub mod workspace;

pub use state::{ClientState, SpitfireState};
