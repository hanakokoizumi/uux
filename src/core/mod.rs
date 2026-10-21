pub mod clipboard;
pub mod clipboard_wire;
pub mod cloud;
pub mod device_actions;
pub mod drag_files;
pub mod files;
pub mod media;
mod network;
mod pixels;
pub mod ports;
pub mod repair;
pub mod runtime;
pub mod session;
pub mod signal;
mod startup_recovery;
pub mod super_screen;
pub mod terminal;
pub mod wire;

#[allow(clippy::all, dead_code)]
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/gv_pb.rs"));
}
