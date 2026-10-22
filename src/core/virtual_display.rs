//! Virtual-display resources are separate from screen selection and super-screen mode.
use super::{pb, session::Output, wire};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

struct Pending {
    request: i64,
    create: bool,
    count: usize,
    ids: BTreeSet<i32>,
    before: BTreeSet<i32>,
    started: Instant,
    accepted: bool,
}
#[derive(Default)]
pub struct VirtualDisplays {
    screens: Option<pb::ScreenSources>,
    pending: Option<Pending>,
    headless_attempted: bool,
}
impl VirtualDisplays {
    pub fn headless_bootstrap(
        &mut self,
        requested: bool,
        protocol_ready: bool,
        out: &Output,
    ) -> Result<Option<Vec<u8>>> {
        if !requested
            || !protocol_ready
            || self.headless_attempted
            || self.pending.is_some()
            || self.screens.as_ref().is_none_or(|s| !s.screens.is_empty())
        {
            return Ok(None);
        }
        self.headless_attempted = true;
        self.command(
            &json!({"op":"virtual_display_create","count":1,"width":1920,"height":1080}),
            out,
        )
        .map(Some)
    }
    pub fn failed(&mut self, message: &str, out: &Output) {
        if let Some(p) = self.pending.take() {
            out.native(
                "virtual_display",
                json!({"phase":"failed","request_id":p.request,"message":message}),
            );
        }
    }
    pub fn command(&mut self, command: &Value, out: &Output) -> Result<Vec<u8>> {
        ensure!(self.pending.is_none(), "虚拟屏操作尚未完成");
        let screens = self.screens.as_ref().context("尚未收到屏幕列表")?;
        let before = screens.screens.iter().map(|s| s.id).collect();
        let create = command["op"] == "virtual_display_create";
        let mut ids = BTreeSet::new();
        let mut count = 0;
        let which = if create {
            ensure!(
                !screens
                    .screens
                    .iter()
                    .any(|s| s.screen_type == pb::ScreenType::Super as i32),
                "请先退出超级屏，再创建普通虚拟屏；超级屏路径尚不能确认新增普通屏资源"
            );
            count = command["count"].as_u64().unwrap_or(1) as usize;
            let width = command["width"].as_i64().unwrap_or(1920);
            let height = command["height"].as_i64().unwrap_or(1080);
            ensure!(
                (1..=4).contains(&count)
                    && (320..=3840).contains(&width)
                    && (200..=2160).contains(&height),
                "虚拟屏数量或尺寸无效"
            );
            pb::rpc_request::Which::CreateVirtualDisplayReq(pb::CreateVirtualDisplayRequest {
                virtual_display_count: count as i32,
                local_resolution: vec![
                    pb::ScreenResolution {
                        width: width as i32,
                        height: height as i32
                    };
                    count
                ],
            })
        } else {
            for id in command["screen_ids"]
                .as_array()
                .context("缺少 screen_ids")?
            {
                let id = i32::try_from(id.as_i64().context("屏幕 ID 无效")?)?;
                let screen = screens
                    .screens
                    .iter()
                    .find(|s| s.id == id)
                    .context("屏幕不存在")?;
                ensure!(
                    screen.screen_type == pb::ScreenType::Virtual as i32,
                    "只能移除普通虚拟屏；退出超级屏须使用显示配置"
                );
                ensure!(ids.insert(id), "屏幕 ID 重复");
            }
            ensure!(
                !ids.is_empty() && ids.len() <= 4,
                "一次移除须指定 1–4 个虚拟屏"
            );
            pb::rpc_request::Which::RemoveVirtualDisplayReq(pb::RemoveVirtualDisplayRequest {
                screen_id: ids.iter().copied().collect(),
            })
        };
        let (request, bytes) = wire::rpc(which);
        self.pending = Some(Pending {
            request,
            create,
            count,
            ids,
            before,
            started: Instant::now(),
            accepted: false,
        });
        out.native("virtual_display",json!({"phase":"submitted","request_id":request,"create":create,"client_request":command["client_request"]}));
        Ok(bytes)
    }
    pub fn response(&mut self, id: i64, code: i32, out: &Output) {
        let Some(p) = &mut self.pending else {
            return;
        };
        if p.request != id {
            return;
        }
        if code != 0 {
            out.native("virtual_display",json!({"phase":"failed","request_id":id,"error_code":code,"message":"远端拒绝虚拟屏操作"}));
            self.pending = None;
            return;
        }
        p.accepted = true;
        self.finish(out);
    }
    pub fn screens(&mut self, screens: &pb::ScreenSources, out: &Output) {
        self.screens = Some(screens.clone());
        self.finish(out);
    }
    fn finish(&mut self, out: &Output) {
        let (Some(p), Some(screens)) = (&self.pending, &self.screens) else {
            return;
        };
        if !p.accepted {
            return;
        }
        let actual: BTreeSet<_> = screens.screens.iter().map(|s| s.id).collect();
        let added: Vec<_> = screens
            .screens
            .iter()
            .filter(|s| {
                s.screen_type == pb::ScreenType::Virtual as i32 && !p.before.contains(&s.id)
            })
            .map(|s| s.id)
            .collect();
        if (p.create && added.len() == p.count) || (!p.create && p.ids.is_disjoint(&actual)) {
            let observed:Vec<_> = screens.screens.iter().filter(|s|added.contains(&s.id)).map(|s|json!({"id":s.id,"width":s.current_resolution.as_ref().map(|r|r.width),"height":s.current_resolution.as_ref().map(|r|r.height)})).collect();
            out.native("virtual_display",json!({"phase":"ready","request_id":p.request,"create":p.create,"screen_ids":if p.create {added} else {p.ids.iter().copied().collect()},"screens":observed,"size_semantics":"local_resolution_hint; use display_configure to request a capture mode","confirmed_by":"remote_reply_and_screen_list"}));
            self.pending = None;
        }
    }
    pub fn tick(&mut self, out: &Output) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.started.elapsed() > Duration::from_secs(25))
        {
            let p = self.pending.take().unwrap();
            out.native("virtual_display",json!({"phase":"timeout","request_id":p.request,"remote_accepted":p.accepted,"message":"未能从屏幕列表确认虚拟屏操作生效"}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn output() -> (Output, std::sync::mpsc::Receiver<crate::backend::Event>) {
        let (events, rx) = std::sync::mpsc::channel();
        (
            Output {
                media: Default::default(),
                generation: None,
                events,
                latest: Default::default(),
                ctx: Default::default(),
            },
            rx,
        )
    }
    fn screens(virtual_id: Option<i32>) -> pb::ScreenSources {
        let mut screens = vec![pb::Screen {
            id: 7,
            screen_type: 0,
            ..Default::default()
        }];
        if let Some(id) = virtual_id {
            screens.push(pb::Screen {
                id,
                screen_type: 1,
                ..Default::default()
            });
        }
        pb::ScreenSources {
            screens,
            ..Default::default()
        }
    }
    #[test]
    fn headless_waits_for_empty_report_and_video_and_attempts_only_once() {
        let (out, _) = output();
        let mut v = VirtualDisplays::default();
        assert!(v.headless_bootstrap(true, true, &out).unwrap().is_none());
        v.screens(&pb::ScreenSources::default(), &out);
        assert!(v.headless_bootstrap(false, true, &out).unwrap().is_none());
        assert!(v.headless_bootstrap(true, false, &out).unwrap().is_none());
        assert!(v.headless_bootstrap(true, true, &out).unwrap().is_some());
        v.failed("rejected", &out);
        assert!(v.headless_bootstrap(true, true, &out).unwrap().is_none());
        let mut existing = VirtualDisplays::default();
        existing.screens(&screens(None), &out);
        assert!(
            existing
                .headless_bootstrap(true, true, &out)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn creation_needs_both_remote_acceptance_and_new_screen_in_either_order() {
        for reply_first in [true, false] {
            let (out, _) = output();
            let mut v = VirtualDisplays::default();
            v.screens(&screens(None), &out);
            v.command(&json!({"op":"virtual_display_create"}), &out)
                .unwrap();
            let id = v.pending.as_ref().unwrap().request;
            if reply_first {
                v.response(id, 0, &out);
            } else {
                v.screens(&screens(Some(93)), &out);
            }
            assert!(v.pending.is_some());
            if reply_first {
                v.screens(&screens(Some(93)), &out);
            } else {
                v.response(id, 0, &out);
            }
            assert!(v.pending.is_none());
        }
    }
    #[test]
    fn removal_protects_physical_screens_and_requires_observed_disappearance() {
        let (out, _) = output();
        let mut v = VirtualDisplays::default();
        v.screens(&screens(Some(93)), &out);
        assert!(
            v.command(
                &json!({"op":"virtual_display_remove","screen_ids":[7]}),
                &out
            )
            .is_err()
        );
        assert!(
            v.command(
                &json!({"op":"virtual_display_remove","screen_ids":[94]}),
                &out
            )
            .is_err()
        );
        v.command(
            &json!({"op":"virtual_display_remove","screen_ids":[93]}),
            &out,
        )
        .unwrap();
        v.response(v.pending.as_ref().unwrap().request, 0, &out);
        assert!(v.pending.is_some());
        v.screens(&screens(None), &out);
        assert!(v.pending.is_none());
    }
}
