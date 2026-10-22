//! File drag destinations on the window's own Wayland connection.
use crate::{
    dpi::LogicalPosition,
    event::WindowEvent,
    platform_impl::wayland::{self, state::WinitState, WindowId},
};
use ahash::AHashMap;
use sctk::{
    data_device_manager::{
        data_device::{DataDevice, DataDeviceData, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::DataSourceHandler,
        DataDeviceManagerState, WritePipe,
    },
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            PostAction, RegistrationToken,
        },
        client::{
            backend::ObjectId,
            protocol::{
                wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
                wl_data_source::WlDataSource, wl_surface::WlSurface,
            },
            Connection, Proxy, QueueHandle,
        },
    },
};
use std::{io::Read, os::unix::ffi::OsStringExt, path::PathBuf, time::Duration};

#[derive(Default)]
pub struct FileDnd {
    pub manager: Option<DataDeviceManagerState>,
    pub devices: AHashMap<ObjectId, DataDevice>,
    incoming: AHashMap<ObjectId, Incoming>,
}
struct Incoming {
    offer: DragOffer,
    window: WindowId,
    paths: Option<Vec<PathBuf>>,
    dropped: bool,
    token: Option<RegistrationToken>,
}

fn parse_uris(bytes: &[u8]) -> Option<Vec<PathBuf>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut paths = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        if paths.len() >= 1000 {
            return None;
        }
        let uri = line.strip_prefix("file://")?;
        let path = if uri.starts_with('/') {
            uri
        } else {
            uri.strip_prefix("localhost")?
        };
        if !path.starts_with('/') {
            return None;
        }
        let decoded = percent_encoding::percent_decode_str(path).collect::<Vec<_>>();
        if decoded.contains(&0) {
            return None;
        }
        paths.push(PathBuf::from(std::ffi::OsString::from_vec(decoded)));
    }
    (!paths.is_empty()).then_some(paths)
}
impl WinitState {
    pub(super) fn dnd_remove_device(&mut self, key: &ObjectId) {
        if let Some(drag) = self.file_dnd.incoming.remove(key) {
            if let Some(token) = drag.token {
                self.loop_handle.remove(token);
            }
            if drag.dropped {
                drag.offer.destroy();
            }
            self.events_sink
                .push_window_event(WindowEvent::HoveredFileCancelled, drag.window);
        }
    }
    fn dnd_position(&mut self, window: WindowId, x: f64, y: f64) {
        let Some(state) = self.windows.get_mut().get(&window) else {
            return;
        };
        let scale = state.lock().unwrap().scale_factor();
        self.events_sink.push_window_event(
            WindowEvent::CursorMoved {
                device_id: crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(
                    wayland::DeviceId,
                )),
                position: LogicalPosition::new(x, y).to_physical(scale),
            },
            window,
        );
    }
    fn dnd_finish_if_ready(&mut self, key: &ObjectId) {
        if !self
            .file_dnd
            .incoming
            .get(key)
            .is_some_and(|d| d.dropped && d.paths.is_some())
        {
            return;
        }
        let drag = self.file_dnd.incoming.remove(key).unwrap();
        for path in drag.paths.unwrap() {
            self.events_sink
                .push_window_event(WindowEvent::DroppedFile(path), drag.window);
        }
        drag.offer.finish();
        drag.offer.destroy();
    }
}
impl DataDeviceHandler for WinitState {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        let window = wayland::make_wid(surface);
        if !self.windows.get_mut().contains_key(&window) {
            return;
        }
        let Some(offer) = device.data::<DataDeviceData>().and_then(|d| d.drag_offer()) else {
            return;
        };
        if !offer.with_mime_types(|m| m.iter().any(|m| m == "text/uri-list")) {
            offer.accept_mime_type(offer.serial, None);
            return;
        }
        offer.accept_mime_type(offer.serial, Some("text/uri-list".into()));
        offer.set_actions(DndAction::Copy, DndAction::Copy);
        self.dnd_position(window, x, y);
        let Ok(pipe) = offer.receive("text/uri-list".into()) else {
            return;
        };
        let key = device.id();
        if let Some(previous) = self.file_dnd.incoming.remove(&key) {
            if let Some(token) = previous.token {
                self.loop_handle.remove(token);
            }
            if previous.dropped {
                previous.offer.destroy();
            }
        }
        let timeout_offer = offer.inner().id();
        let read_key = key.clone();
        let timeout_key = key.clone();
        let mut bytes = Vec::new();
        self.file_dnd.incoming.insert(
            key.clone(),
            Incoming {
                offer,
                window,
                paths: None,
                dropped: false,
                token: None,
            },
        );
        let source = self.loop_handle.insert_source(pipe, move |_, file, state| {
            if !state.file_dnd.incoming.contains_key(&read_key) {
                return PostAction::Remove;
            }
            let mut chunk = [0; 8192];
            // The event source keeps ownership of the file descriptor.
            let read = unsafe { file.get_mut() }.read(&mut chunk);
            match read {
                Ok(0) => {
                    if let Some(paths) = parse_uris(&bytes) {
                        let drag = state.file_dnd.incoming.get_mut(&read_key).unwrap();
                        for path in &paths {
                            state.events_sink.push_window_event(
                                WindowEvent::HoveredFile(path.clone()),
                                drag.window,
                            );
                        }
                        drag.paths = Some(paths);
                        drag.token = None;
                        state.dnd_finish_if_ready(&read_key);
                    } else if let Some(drag) = state.file_dnd.incoming.remove(&read_key) {
                        if drag.dropped {
                            drag.offer.destroy();
                        }
                        state
                            .events_sink
                            .push_window_event(WindowEvent::HoveredFileCancelled, drag.window);
                    }
                    PostAction::Remove
                }
                Ok(n) if bytes.len() + n <= 1024 * 1024 => {
                    bytes.extend_from_slice(&chunk[..n]);
                    PostAction::Continue
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    PostAction::Continue
                }
                _ => {
                    if let Some(drag) = state.file_dnd.incoming.remove(&read_key) {
                        if drag.dropped {
                            drag.offer.destroy();
                        }
                        state
                            .events_sink
                            .push_window_event(WindowEvent::HoveredFileCancelled, drag.window);
                    }
                    PostAction::Remove
                }
            }
        });
        match source {
            Ok(token) => self.file_dnd.incoming.get_mut(&key).unwrap().token = Some(token),
            Err(_) => {
                self.file_dnd.incoming.remove(&key);
                return;
            }
        }
        // A source that never closes its pipe must not retain window resources.
        let _ = self.loop_handle.insert_source(
            Timer::from_duration(Duration::from_secs(15)),
            move |_, _, state| {
                if state
                    .file_dnd
                    .incoming
                    .get(&timeout_key)
                    .is_some_and(|d| d.token.is_some() && d.offer.inner().id() == timeout_offer)
                {
                    let drag = state.file_dnd.incoming.remove(&timeout_key).unwrap();
                    if let Some(token) = drag.token {
                        state.loop_handle.remove(token);
                    }
                    if drag.dropped {
                        drag.offer.destroy();
                    }
                    state
                        .events_sink
                        .push_window_event(WindowEvent::HoveredFileCancelled, drag.window);
                }
                TimeoutAction::Drop
            },
        );
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        let key = device.id();
        if self.file_dnd.incoming.get(&key).is_some_and(|d| !d.dropped) {
            let drag = self.file_dnd.incoming.remove(&key).unwrap();
            if let Some(token) = drag.token {
                self.loop_handle.remove(token);
            }
            self.events_sink
                .push_window_event(WindowEvent::HoveredFileCancelled, drag.window);
        }
    }
    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
    ) {
        if let Some(drag) = self.file_dnd.incoming.get(&device.id()) {
            self.dnd_position(drag.window, x, y);
        }
    }
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        let Some(offer) = device.data::<DataDeviceData>().and_then(|d| d.drag_offer()) else {
            return;
        };
        if offer.inner().version() >= 3 && offer.selected_action != DndAction::Copy {
            if let Some(drag) = self.file_dnd.incoming.remove(&device.id()) {
                if let Some(token) = drag.token {
                    self.loop_handle.remove(token);
                }
                drag.offer.destroy();
                self.events_sink
                    .push_window_event(WindowEvent::HoveredFileCancelled, drag.window);
            }
            return;
        }
        if let Some(drag) = self.file_dnd.incoming.get_mut(&device.id()) {
            drag.dropped = true;
        }
        self.dnd_finish_if_ready(&device.id());
    }
}
impl DataOfferHandler for WinitState {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        offer: &mut DragOffer,
        _: DndAction,
    ) {
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}
impl DataSourceHandler for WinitState {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: String,
        _: WritePipe,
    ) {
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        source.destroy();
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        source.destroy();
    }
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}
sctk::delegate_data_device!(WinitState);
