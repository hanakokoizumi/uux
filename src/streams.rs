//! Per-consumer cursors over bounded, shared original-media storage.
use crate::{
    mcp_api::{sid, str_arg},
    media_bus::Sample,
    service::{Shared, uid},
};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

pub struct Subscription {
    pub owner: String,
    pub session: String,
    pub screen: i32,
    pub video: bool,
    pub audio: bool,
    pub raw_video: bool,
    pub raw_audio: bool,
    pub state: Mutex<Cursor>,
}
pub struct Cursor {
    pub cursor: u64,
    pub version: u64,
    pub needs_keyframe: bool,
    pub decoder: crate::stream_decode::Decoder,
    pub pending: Option<Arc<Sample>>,
}
pub type Handle = Arc<Subscription>;

pub fn open(hub: &Shared, client: &str, args: &Value) -> Result<Value> {
    let id = sid(args)?;
    let tracks = args["tracks"]
        .as_array()
        .context("tracks 必须为 video/audio 数组")?;
    ensure!(
        !tracks.is_empty()
            && tracks.len() <= 2
            && tracks.iter().all(|t| t == "video" || t == "audio"),
        "tracks 只能包含 video 和 audio"
    );
    let video = tracks.iter().any(|t| t == "video");
    let audio = tracks.iter().any(|t| t == "audio");
    let video_format = args["video_format"].as_str().unwrap_or("encoded");
    let audio_format = args["audio_format"].as_str().unwrap_or("encoded");
    ensure!(
        ["encoded", "rgba"].contains(&video_format) && ["encoded", "pcm"].contains(&audio_format),
        "媒体输出格式无效"
    );
    let raw_video = video && video_format == "rgba";
    let raw_audio = audio && audio_format == "pcm";
    let screen = i32::try_from(args["screen_id"].as_i64().context("缺少 screen_id")?)?;
    let stream = uid();
    let mut h = hub.lock().unwrap();
    ensure!(h.streams.len() < 64, "媒体订阅已达到上限");
    ensure!(
        !raw_video || h.streams.values().filter(|s| s.raw_video).count() < 4,
        "解码帧订阅最多同时 4 个"
    );
    let s = h.ready(client, id)?;
    let bus = s.backend.media.clone();
    ensure!(
        screen == bus.geometry().screen_id,
        "此会话当前只接收选中的屏幕；请先用 uu_display_configure 选择目标屏幕，订阅不会自动切屏"
    );
    let cursor = bus.start_cursor(video);
    let subscription = Subscription {
        owner: client.into(),
        session: id.into(),
        screen,
        video,
        audio,
        raw_video,
        raw_audio,
        state: Mutex::new(Cursor {
            cursor,
            version: 0,
            needs_keyframe: video,
            decoder: Default::default(),
            pending: None,
        }),
    };
    h.retain_resource(client, id, &stream, "stream")?;
    h.streams.insert(stream.clone(), Arc::new(subscription));
    if let Err(e) = h.refresh_media(id) {
        h.streams.remove(&stream);
        h.release_resource(id, &stream);
        return Err(e);
    }
    if video {
        bus.request_keyframe.store(true, Ordering::Relaxed);
    }
    Ok(
        json!({"stream_id":stream,"session_id":id,"screen_id":screen,"cursor":cursor,"tracks":tracks,"video_format":video_format,"audio_format":audio_format,"format":bus.format(),"decoded_format":{"rgba":"packed RGBA8 SDR; stride=width*4","pcm":"48000 Hz stereo interleaved float32 little-endian"},"transport":{"mcp":"uu_stream_read; data_base64 contains the indicated sample format, not a playable container","ipc":"same-user Unix socket: op=media; length-prefixed JSON metadata followed by binary sample payloads in order"}}),
    )
}

pub fn close(hub: &Shared, client: &str, args: &Value) -> Result<Value> {
    let id = str_arg(args, "stream_id")?;
    let mut h = hub.lock().unwrap();
    let sub = h.streams.get(id).context("订阅不存在或已释放")?;
    ensure!(sub.owner == client, "订阅属于其他客户端");
    let sid = sub.session.clone();
    h.streams.remove(id);
    let _ = h.refresh_media(&sid);
    h.release_resource(&sid, id);
    Ok(json!({"stream_id":id,"closed":true}))
}

pub fn read(hub: &Shared, client: &str, args: &Value) -> Result<(Value, Vec<Arc<Sample>>)> {
    let id = str_arg(args, "stream_id")?;
    let limit = args["max_bytes"].as_u64().unwrap_or(1024 * 1024);
    let duration = args["max_duration_ms"].as_u64().unwrap_or(200);
    let wait = args["wait_ms"].as_u64().unwrap_or(0);
    ensure!(
        (1..=64 * 1024 * 1024).contains(&limit) && (1..=1000).contains(&duration) && wait <= 5000,
        "读取上限为 64 MiB、1000 ms 数据、5000 ms 等待"
    );
    let (handle, bus) = {
        let h = hub.lock().unwrap();
        let handle = h.streams.get(id).context("订阅不存在或已释放")?.clone();
        ensure!(handle.owner == client, "订阅属于其他客户端");
        let bus = h.ready(client, &handle.session)?.backend.media.clone();
        (handle, bus)
    };
    // A slow reader locks only its own cursor, never the hub or receiver.
    let mut sub = handle
        .state
        .try_lock()
        .map_err(|_| anyhow::anyhow!("同一订阅已有读取请求"))?;
    let cursor = args["cursor"].as_u64().context("缺少 cursor")?;
    ensure!(
        cursor == sub.cursor,
        "cursor 不匹配；下一游标为 {}",
        sub.cursor
    );
    let started = Instant::now();
    if sub.version != bus.geometry().version {
        sub.pending = None;
        sub.decoder = Default::default();
    }
    if let Some(sample) = sub.pending.take() {
        let mut samples = Vec::new();
        let mut required = None;
        if sample.data.len() > limit as usize {
            required = Some(sample.data.len());
            sub.pending = Some(sample);
        } else {
            samples.push(sample);
        }
        return Ok((
            json!({"stream_id":id,"cursor":cursor,"next_cursor":cursor,"format_changed":false,"geometry":bus.geometry(),"discontinuity":false,"waiting_keyframe":false,"required_bytes":required,"samples":samples.iter().map(|s|s.metadata()).collect::<Vec<_>>()}),
            samples,
        ));
    }
    let mut batch = loop {
        let current = bus.geometry();
        ensure!(
            current.screen_id == handle.screen,
            "会话已切换屏幕；此订阅仍绑定原 screen_id，请切回或关闭并重新订阅"
        );
        let batch = bus.read(
            cursor,
            handle.video,
            handle.audio,
            sub.needs_keyframe || sub.version != current.version,
            (limit as usize).min(16 * 1024 * 1024),
            duration,
        );
        if !batch.samples.is_empty()
            || batch.required_bytes.is_some()
            || started.elapsed() >= Duration::from_millis(wait)
        {
            break batch;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let changed = sub.version != batch.geometry.version;
    if handle.raw_video || handle.raw_audio {
        if changed || batch.gap {
            sub.decoder = Default::default();
        }
        let mut output = Vec::new();
        let mut bytes = 0;
        let mut consumed = cursor;
        let mut key_seen = false;
        for sample in &batch.samples {
            consumed = sample.sequence;
            key_seen |= sample.track == "video" && sample.keyframe;
            let converted =
                sub.decoder
                    .convert(sample.clone(), handle.raw_video, handle.raw_audio)?;
            if let Some(sample) = converted {
                if bytes + sample.data.len() > limit as usize {
                    if output.is_empty() {
                        batch.required_bytes = Some(sample.data.len());
                    }
                    sub.pending = Some(sample);
                    break;
                }
                bytes += sample.data.len();
                output.push(sample);
            }
        }
        batch.next_cursor = consumed;
        batch.waiting_keyframe =
            handle.video && !key_seen && (sub.needs_keyframe || changed || batch.gap);
        batch.samples = output;
    }
    sub.cursor = batch.next_cursor;
    sub.version = batch.geometry.version;
    sub.needs_keyframe = batch.waiting_keyframe;
    let metadata = json!({"stream_id":id,"cursor":cursor,"next_cursor":batch.next_cursor,"format_changed":changed,"format":if changed {bus.format()}else{Value::Null},"geometry":batch.geometry,"discontinuity":batch.gap || changed,"waiting_keyframe":batch.waiting_keyframe,"required_bytes":batch.required_bytes,"samples":batch.samples.iter().map(|s|s.metadata()).collect::<Vec<_>>()});
    Ok((metadata, batch.samples))
}
pub fn read_json(hub: &Shared, client: &str, args: &Value) -> Result<Value> {
    // Base64 adds one third; keep the complete JSON response below IPC's 32 MiB.
    // Large raw frames use the binary transport, whose metadata stays small.
    if let Some(limit) = args.get("max_bytes") {
        ensure!(
            limit
                .as_u64()
                .is_some_and(|n| (1..=64 * 1024 * 1024).contains(&n)),
            "max_bytes 须在 1–67108864 之间"
        );
    }
    let mut args = args.clone();
    args["max_bytes"] = json!(
        args["max_bytes"]
            .as_u64()
            .unwrap_or(1024 * 1024)
            .min(20 * 1024 * 1024)
    );
    let (mut metadata, samples) = read(hub, client, &args)?;
    metadata["transport_payload_limit"] = json!(20 * 1024 * 1024);
    for (entry, sample) in metadata["samples"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(samples)
    {
        entry["data_base64"] =
            json!(base64::engine::general_purpose::STANDARD.encode(&sample.data));
    }
    Ok(metadata)
}
