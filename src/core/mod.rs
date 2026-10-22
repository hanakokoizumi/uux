pub mod clipboard;
pub mod clipboard_files;
pub mod clipboard_image;
pub mod clipboard_wire;
pub mod cloud;
pub mod decoder;
pub mod device_actions;
pub mod drag_files;
pub mod files;
pub mod media;
pub mod microphone;
mod network;
pub(crate) mod pixels;
mod playback;
pub mod ports;
pub mod pulse_audio;
pub mod repair;
pub mod runtime;
pub mod session;
pub mod signal;
mod startup_recovery;
pub mod super_screen;
pub mod terminal;
mod virtual_display;
pub mod wire;

#[allow(clippy::all, dead_code)]
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/gv_pb.rs"));
}
