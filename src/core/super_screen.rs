//! Manual super-screen mode uses its own RPC and reported screen type.
//! An RPC acknowledgement alone never changes the displayed mode.
use super::{pb, session::Output, wire};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct SuperScreen {
    active: bool,
    current: Option<pb::Screen>,
    pending: Option<Pending>,
    restore_on_close: bool,
}
struct Pending {
    id: i64,
    enter: bool,
    accepted: bool,
    started: Instant,
}
impl SuperScreen {
    pub fn ordinary_screen_selected(&self) -> bool {
        self.pending.is_none()
            && self
                .current
                .as_ref()
                .is_some_and(|screen| screen.screen_type == 0)
    }
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
    /// Apply a remembered mode once, after the desktop is requested and both
    /// metadata and the application handshake are available. Tool-only
    /// sessions retain the preference without changing the remote display.
    pub fn restore_preference(
        &mut self,
        preference: &mut Option<bool>,
        video_visible: bool,
        protocol_ready: bool,
        out: &Output,
    ) -> Result<Option<Vec<u8>>> {
        if !video_visible || !protocol_ready || self.current.is_none() || self.pending.is_some() {
            return Ok(None);
        }
        let Some(enabled) = preference.take() else {
            return Ok(None);
        };
        self.command(&json!({"enabled":enabled}), out)
    }
    fn publish(&self, out: &Output, phase: &str, error: Option<i32>) {
        out.native("super_screen", json!({"active":self.active,"phase":phase,
            "pending":self.pending.is_some(),"enter":self.pending.as_ref().map(|p|p.enter),"error":error}));
    }
    fn complete_if_observed(&mut self) -> bool {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.accepted && p.enter == self.active)
        {
            self.pending = None;
            if !self.active {
                self.restore_on_close = false;
            }
            true
        } else {
            false
        }
    }
    pub fn screens(&mut self, screens: &pb::ScreenSources, out: &Output) {
        let previous = self.current.as_ref().map(|_| self.active);
        self.current = screens
            .screens
            .iter()
            .find(|s| s.id == screens.current_screen_id)
            .cloned();
        if let Some(screen) = &self.current {
            self.active = screen.screen_type == pb::ScreenType::Super as i32;
            let completed = self.complete_if_observed();
            if previous == Some(self.active) && !completed {
                return;
            }
            self.publish(
                out,
                if self.pending.is_some() {
                    "switching"
                } else {
                    "ready"
                },
                None,
            );
        }
    }
    pub fn command(&mut self, command: &Value, out: &Output) -> Result<Option<Vec<u8>>> {
        ensure!(self.pending.is_none(), "正在切换屏幕模式，请稍候");
        let enter = command["enabled"].as_bool().unwrap_or(true);
        if self.current.is_some() && enter == self.active {
            self.publish(out, "ready", None);
            return Ok(None);
        }
        let which = if enter {
            let screen = self
                .current
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("正在等待远端屏幕信息"))?;
            let original = screen.current_resolution.as_ref();
            let width = command["width"]
                .as_i64()
                .unwrap_or(original.map_or(0, |r| r.width as i64));
            let height = command["height"]
                .as_i64()
                .unwrap_or(original.map_or(0, |r| r.height as i64));
            let dpi = command["dpi"].as_i64().unwrap_or_else(|| {
                screen
                    .dpi_scale
                    .as_ref()
                    .map_or(100, |d| d.current_dpi.max(100)) as i64
            });
            ensure!(
                (320..=3840).contains(&width) && (200..=2160).contains(&height),
                "超级屏分辨率超出支持范围"
            );
            ensure!((100..=300).contains(&dpi), "超级屏缩放比例超出支持范围");
            pb::rpc_request::Which::EnterSuperScreenReq(pb::EnterSuperScreenRequest {
                reason: pb::enter_super_screen_request::EnterReason::ReasonManual as i32,
                resolution: Some(pb::ScreenResolution {
                    width: width as i32,
                    height: height as i32,
                }),
                dpi_scale: dpi as i32,
                params: None,
            })
        } else {
            pb::rpc_request::Which::QuitSuperScreen(pb::QuitSuperScreenRequest {})
        };
        let (id, message) = wire::rpc(which);
        if enter {
            self.restore_on_close = true;
        }
        self.pending = Some(Pending {
            id,
            enter,
            accepted: false,
            started: Instant::now(),
        });
        crate::perf::trace(
            "super_screen_request",
            json!({"request_id":id,"enter":enter}),
        );
        self.publish(out, "switching", None);
        Ok(Some(message))
    }
    pub fn response(&mut self, id: i64, enter: bool, error: i32, out: &Output) {
        let Some(pending) = self
            .pending
            .as_mut()
            .filter(|p| p.id == id && p.enter == enter)
        else {
            return;
        };
        crate::perf::trace(
            "super_screen_result",
            json!({"request_id":id,"enter":enter,"error":error,"elapsed_ms":pending.started.elapsed().as_millis() as u64}),
        );
        if error != 0 {
            self.pending = None;
            self.publish(out, "failed", Some(error));
        } else {
            pending.accepted = true;
            let complete = self.complete_if_observed();
            self.publish(out, if complete { "ready" } else { "waiting" }, None);
        }
    }
    /// Only undo a super-screen entry requested by this session, never a mode
    /// that was already active when it connected. Signaling release still runs
    /// independently if this best-effort request cannot be delivered.
    pub fn close_request(&mut self) -> Option<Vec<u8>> {
        std::mem::take(&mut self.restore_on_close).then(|| {
            wire::rpc(pb::rpc_request::Which::QuitSuperScreen(
                pb::QuitSuperScreenRequest {},
            ))
            .1
        })
    }
    pub fn tick(&mut self, out: &Output) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.started.elapsed() > Duration::from_secs(45))
        {
            self.pending = None;
            self.publish(out, "timeout", None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex, mpsc};
    fn output() -> Output {
        Output {
            media: Default::default(),
            generation: Some(1),
            events: mpsc::channel().0,
            latest: Arc::new(Mutex::new(None)),
            ctx: Default::default(),
        }
    }
    fn screens(active: bool) -> pb::ScreenSources {
        pb::ScreenSources {
            current_screen_id: 3,
            screens: vec![pb::Screen {
                id: 3,
                screen_type: if active { 2 } else { 0 },
                current_resolution: Some(pb::WinRect {
                    width: 1280,
                    height: 720,
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }
    }
    #[test]
    fn remembered_mode_waits_for_desktop_and_handshake_then_applies_once() {
        let out = output();
        let mut state = SuperScreen::default();
        let mut wanted = Some(true);
        assert!(
            state
                .restore_preference(&mut wanted, true, true, &out)
                .unwrap()
                .is_none()
        );
        state.screens(&screens(false), &out);
        assert!(
            state
                .restore_preference(&mut wanted, false, true, &out)
                .unwrap()
                .is_none()
        );
        assert!(
            state
                .restore_preference(&mut wanted, true, false, &out)
                .unwrap()
                .is_none()
        );
        assert_eq!(wanted, Some(true));
        let bytes = state
            .restore_preference(&mut wanted, true, true, &out)
            .unwrap()
            .unwrap();
        use prost::Message;
        let message = pb::Message::decode(bytes.as_slice()).unwrap();
        assert!(matches!(
            message.which,
            Some(pb::message::Which::RpcRequest(pb::RpcRequest {
                which: Some(pb::rpc_request::Which::EnterSuperScreenReq(_)),
                ..
            }))
        ));
        assert!(wanted.is_none());
        assert!(
            state
                .restore_preference(&mut wanted, true, true, &out)
                .unwrap()
                .is_none()
        );
        assert!(state.close_request().is_some());
    }
    #[test]
    fn remembered_physical_mode_exits_super_without_claiming_an_existing_super_screen() {
        let out = output();
        let mut state = SuperScreen::default();
        state.screens(&screens(true), &out);
        assert!(
            state
                .restore_preference(&mut Some(true), true, true, &out)
                .unwrap()
                .is_none()
        );
        assert!(state.close_request().is_none());
        assert!(
            state
                .restore_preference(&mut Some(false), true, true, &out)
                .unwrap()
                .is_some()
        );
        assert_eq!(state.pending.as_ref().map(|p| p.enter), Some(false));
    }
    #[test]
    fn acknowledgement_and_screen_can_arrive_in_either_order() {
        for screen_first in [true, false] {
            let out = output();
            let mut state = SuperScreen::default();
            state.screens(&screens(false), &out);
            state.command(&json!({"enabled":true}), &out).unwrap();
            let id = state.pending.as_ref().unwrap().id;
            assert!(state.command(&json!({"enabled":true}), &out).is_err());
            state.response(id + 1, true, 0, &out);
            assert!(!state.active);
            if screen_first {
                state.screens(&screens(true), &out);
            } else {
                state.response(id, true, 0, &out);
            }
            assert!(state.pending.is_some());
            if screen_first {
                state.response(id, true, 0, &out);
            } else {
                state.screens(&screens(true), &out);
            }
            assert!(state.active && state.pending.is_none());
            state.command(&json!({"enabled":false}), &out).unwrap();
            let id = state.pending.as_ref().unwrap().id;
            state.response(id, false, 0, &out);
            assert!(state.active);
            state.screens(&screens(false), &out);
            assert!(!state.active && state.pending.is_none());
        }
    }
    #[test]
    fn rejection_timeout_and_stale_response_do_not_fake_mode_changes() {
        let out = output();
        let mut state = SuperScreen::default();
        state.screens(&screens(false), &out);
        state.command(&json!({"enabled":true}), &out).unwrap();
        let old = state.pending.as_ref().unwrap().id;
        state.response(old, true, 7, &out);
        assert!(!state.active && state.pending.is_none());
        state.command(&json!({"enabled":true}), &out).unwrap();
        state.response(old, true, 0, &out);
        assert!(!state.pending.as_ref().unwrap().accepted);
        state.pending.as_mut().unwrap().started = Instant::now() - Duration::from_secs(46);
        state.tick(&out);
        assert!(!state.active && state.pending.is_none());
        state.screens(&screens(true), &out);
        assert!(state.active);
    }
    #[test]
    fn cleanup_only_restores_mode_entered_by_this_session() {
        let out = output();
        let mut state = SuperScreen::default();
        state.screens(&screens(true), &out);
        assert!(state.close_request().is_none());
        state.screens(&screens(false), &out);
        state.command(&json!({"enabled":true}), &out).unwrap();
        assert!(state.close_request().is_some());
        assert!(state.close_request().is_none());
    }
}
