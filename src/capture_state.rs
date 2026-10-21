//! Keep RPC acceptance separate from observing a frame at the requested size.
#[derive(Default)]
pub struct CaptureState {
    token: u64,
    pending: Option<Pending>,
}
struct Pending {
    request_id: Option<i64>,
    target: Option<[usize; 2]>,
    accepted: bool,
    screen: i32,
    screen_confirmed: bool,
}
impl CaptureState {
    pub fn begin(&mut self, size: [i32; 2], screen: i32) -> u64 {
        self.token = self.token.wrapping_add(1);
        self.pending = Some(Pending {
            request_id: None,
            target: (size[0] > 0 && size[1] > 0).then_some([size[0] as usize, size[1] as usize]),
            accepted: false,
            screen,
            screen_confirmed: false,
        });
        self.token
    }
    pub fn submitted(&mut self, token: u64, request_id: i64) {
        if token == self.token
            && let Some(pending) = &mut self.pending
        {
            pending.request_id = Some(request_id);
        }
    }
    pub fn acknowledge(
        &mut self,
        request_id: i64,
        accepted: bool,
        size: [usize; 2],
    ) -> Option<String> {
        let pending = self.pending.as_mut()?;
        if pending.request_id != Some(request_id) {
            return None;
        }
        if !accepted {
            self.pending = None;
            return Some("远端未接受参数修改".into());
        }
        pending.accepted = true;
        if let Some(target) = pending.target
            && target != size
        {
            if pending.screen_confirmed {
                return Some(display_confirmed(target, size));
            }
            return Some(format!(
                "远端已接受，正在等待 {} × {} 画面",
                target[0], target[1]
            ));
        }
        self.pending = None;
        Some("远端参数已更新".into())
    }
    /// Screen geometry is the desktop mode, not necessarily the encoded size.
    /// Keep waiting for exact-size video separately, but never describe a
    /// confirmed desktop mode as an unacknowledged change just due to scaling.
    pub fn observe_screen(
        &mut self,
        screen: i32,
        source: [usize; 2],
        video: [usize; 2],
    ) -> Option<String> {
        let pending = self.pending.as_mut()?;
        if pending.screen != screen || pending.target != Some(source) {
            return None;
        }
        pending.screen_confirmed = true;
        pending.accepted.then(|| display_confirmed(source, video))
    }
    pub fn observe_frame(&mut self, size: [usize; 2]) -> bool {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.accepted && p.target == Some(size))
        {
            self.pending = None;
            return true;
        }
        false
    }
    pub fn clear(&mut self) {
        self.pending = None;
    }
}

fn display_confirmed(source: [usize; 2], video: [usize; 2]) -> String {
    if source == video {
        "远端分辨率已更新".into()
    } else {
        format!(
            "远端分辨率已切换至 {} × {}；当前视频为 {} × {}（画质设置可能缩小视频）",
            source[0], source[1], video[0], video[1]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepted_resolution_waits_for_matching_video() {
        let mut state = CaptureState::default();
        let token = state.begin([1280, 720], 1);
        state.submitted(token, 31);
        assert!(
            state
                .acknowledge(31, true, [1920, 1080])
                .unwrap()
                .contains("正在等待")
        );
        assert!(!state.observe_frame([1920, 1080]));
        assert!(state.observe_frame([1280, 720]));
        assert!(!state.observe_frame([1280, 720]));
    }
    #[test]
    fn newer_selection_ignores_older_submission_and_ack() {
        let mut state = CaptureState::default();
        let old = state.begin([1280, 720], 1);
        let new = state.begin([1600, 900], 1);
        state.submitted(old, 31);
        assert!(state.acknowledge(31, true, [1280, 720]).is_none());
        state.submitted(new, 33);
        assert!(state.acknowledge(31, false, [1280, 720]).is_none());
        assert!(
            state
                .acknowledge(33, true, [1280, 720])
                .unwrap()
                .contains("1600")
        );
        assert!(!state.observe_frame([1280, 720]));
        assert!(state.observe_frame([1600, 900]));
    }
    #[test]
    fn frame_before_ack_and_disconnect_do_not_leave_false_pending_state() {
        let mut state = CaptureState::default();
        let token = state.begin([1280, 720], 1);
        state.submitted(token, 31);
        assert!(!state.observe_frame([1280, 720]));
        assert_eq!(
            state.acknowledge(31, true, [1280, 720]).as_deref(),
            Some("远端参数已更新")
        );
        let token = state.begin([1600, 900], 1);
        state.submitted(token, 33);
        state.clear();
        assert!(state.acknowledge(33, true, [1280, 720]).is_none());
    }
    #[test]
    fn scaled_video_does_not_hide_confirmed_desktop_mode() {
        let mut state = CaptureState::default();
        let token = state.begin([1600, 1200], 3);
        state.submitted(token, 31);
        assert!(state.observe_screen(4, [1600, 1200], [1280, 960]).is_none());
        assert!(state.observe_screen(3, [1600, 1200], [1280, 960]).is_none());
        let note = state.acknowledge(31, true, [1440, 1080]).unwrap();
        assert!(note.contains("远端分辨率已切换至 1600 × 1200"));
        assert!(note.contains("当前视频为 1440 × 1080"));
        assert!(!note.contains("正在等待"));
        assert!(!state.observe_frame([1440, 1080]));
        assert!(state.observe_frame([1600, 1200]));
    }
}
