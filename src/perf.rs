//! Local stage measurements, deliberately excluding account and remote content.
use serde_json::{Value, json};
use std::{
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Default)]
struct Window {
    count: u64,
    micros: u64,
    max: u64,
}

#[derive(Default)]
pub struct Timing(Mutex<Window>);

/// Report slow main-thread stages without logging input or window contents.
pub struct UiScope {
    stage: &'static str,
    started: Instant,
}
impl UiScope {
    pub fn new(stage: &'static str) -> Self {
        Self {
            stage,
            started: Instant::now(),
        }
    }
}
impl Drop for UiScope {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        if elapsed >= Duration::from_millis(50) {
            trace(
                "ui_slow_stage",
                json!({"stage":self.stage,"elapsed_ms":elapsed.as_secs_f64()*1000.0}),
            );
        }
    }
}

pub fn trace_focus(ctx: &eframe::egui::Context, window: &'static str) {
    ctx.input(|input| {
        for event in &input.events {
            if let eframe::egui::Event::WindowFocused(focused) = event {
                trace("ui_focus", json!({"window":window,"focused":focused}));
            }
        }
    });
}

/// A single read-only RPC in flight. Correlation excludes unrelated replies
/// from the measured round trip, which also includes remote request handling.
pub struct RoundTripProbe {
    sequence: i64,
    id: u64,
    started: Instant,
}
impl RoundTripProbe {
    pub fn new(sequence: i64, id: u64, started: Instant) -> Self {
        Self {
            sequence,
            id,
            started,
        }
    }
    pub fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= Duration::from_secs(6)
    }
    pub fn reply(&self, sequence: i64, now: Instant) -> Option<Value> {
        (sequence == self.sequence && !self.expired(now)).then(|| {
            json!({"id":self.id,"matched":true,
                "round_trip_ms":now.saturating_duration_since(self.started).as_secs_f64()*1000.0})
        })
    }
    pub fn timeout(&self) -> Value {
        json!({"id":self.id,"matched":false,"round_trip_ms":null})
    }
}
impl Timing {
    pub fn record(&self, duration: Duration) {
        let us = duration.as_micros().min(u64::MAX as u128) as u64;
        let mut w = self.0.lock().unwrap();
        w.count += 1;
        w.micros = w.micros.saturating_add(us);
        w.max = w.max.max(us);
    }
    pub fn take(&self) -> Value {
        let w = std::mem::take(&mut *self.0.lock().unwrap());
        json!({"count":w.count,"mean_ms":(w.count > 0).then(||w.micros as f64/w.count as f64/1000.0),
            "max_ms":(w.count > 0).then(||w.max as f64/1000.0)})
    }
}

/// Opt-in, bounded, off-thread JSONL. Never serialize arbitrary protocol events.
pub fn trace(kind: &str, data: Value) {
    static WRITER: OnceLock<Option<mpsc::SyncSender<Value>>> = OnceLock::new();
    static DROPPED: AtomicU64 = AtomicU64::new(0);
    let writer = WRITER.get_or_init(|| {
        let path = std::env::var_os("UU_PERF_LOG")?;
        use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => file,
            Err(error) => {
                eprintln!("无法打开性能日志：{error}");
                return None;
            }
        };
        let (tx, rx) = mpsc::sync_channel::<Value>(128);
        std::thread::spawn(move || {
            for value in rx {
                if writeln!(file, "{value}").is_err() {
                    break;
                }
            }
        });
        Some(tx)
    });
    if let Some(writer) = writer {
        trace_enqueue(
            writer,
            &DROPPED,
            json!({"kind":kind,"unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),"data":data}),
        );
    }
}

fn trace_enqueue(writer: &mpsc::SyncSender<Value>, dropped: &AtomicU64, mut value: Value) {
    let missed = dropped.swap(0, Ordering::Relaxed);
    value["dropped_before"] = json!(missed);
    if writer.try_send(value).is_err() {
        dropped.fetch_add(missed.saturating_add(1), Ordering::Relaxed);
    }
}

/// Codec capability diagnostics: only known numeric fields, never whole SDP.
pub fn h264_format(fmtp: &str) -> Value {
    let mut result = json!({});
    for parameter in fmtp.split(';') {
        let Some((key, value)) = parameter.trim().split_once('=') else {
            continue;
        };
        if key == "profile-level-id"
            && value.len() == 6
            && value.bytes().all(|v| v.is_ascii_hexdigit())
        {
            if let Ok(profile) = u32::from_str_radix(value, 16) {
                result["profile_idc"] = json!(profile >> 16);
                result["constraint_flags"] = json!((profile >> 8) & 255);
                result["level_idc"] = json!(profile & 255);
            }
        } else if matches!(
            key,
            "level-asymmetry-allowed" | "packetization-mode" | "max-fs" | "max-mbps"
        ) && let Ok(number) = value.parse::<u32>()
        {
            result[key] = json!(number);
        }
    }
    result
}

/// Keep only published numeric capability fields, never identities or addresses.
pub fn streamer_capabilities(text: &str) -> Value {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return json!({"parsed":false});
    };
    let caps = &value["device_capability"];
    let codecs: Vec<Value> = caps["video_codec_capability"]
        .as_array()
        .into_iter()
        .flatten()
        .take(32)
        .map(|codec| {
            let mut result = json!({});
            for key in [
                "video_codec",
                "width",
                "height",
                "chroma_sampling",
                "bit_depth",
                "codec_impl",
            ] {
                if let Some(number) = codec[key].as_i64() {
                    result[key] = json!(number);
                }
            }
            result
        })
        .collect();
    let fps: Vec<i64> = caps["display_info"]
        .as_array()
        .into_iter()
        .flatten()
        .take(32)
        .filter_map(|display| display["fps"].as_i64())
        .collect();
    json!({"parsed":caps.is_object(),"complete":caps["video_codec_capability"].as_array().is_some_and(|v|v.len()<=32),"video_codecs":codecs,"display_fps":fps})
}

/// The internal streamer channel carries JSON statistics in the official
/// implementation. Observe bounded numeric metrics without serializing payload,
/// unknown keys, string values, addresses, identities or timestamps.
pub fn streamer_event(data: &[u8]) -> Value {
    let mut result = json!({"bytes":data.len(),"parsed":false});
    if data.len() > 512 * 1024 {
        return result;
    }
    let Ok(value) = serde_json::from_slice::<Value>(data) else {
        return result;
    };
    result["parsed"] = json!(true);
    result["event"] = json!(if value.get("connection_period_stats_event").is_some() {
        "connection_period_stats_event"
    } else if value.get("connection_ending_stats_event").is_some() {
        "connection_ending_stats_event"
    } else {
        "other"
    });
    fn metrics(value: &Value, depth: usize, remaining: &mut usize, out: &mut Value) {
        if depth == 0 || *remaining == 0 {
            return;
        }
        *remaining -= 1;
        match value {
            Value::Object(object) => {
                for (key, value) in object.iter().take(64) {
                    if matches!(
                        key.as_str(),
                        "encoder_input_fps"
                            | "encoder_fps"
                            | "encoder_actual_input_fps"
                            | "media_bitrate_bps"
                            | "padding_bitrate_bps"
                            | "total_bitrate_bps"
                            | "avg_encoder_input_fps"
                            | "avg_encoder_fps"
                            | "avg_encoder_actual_input_fps"
                            | "avg_media_bitrate_bps"
                            | "avg_padding_bitrate_bps"
                            | "avg_total_bitrate_bps"
                    ) && value.is_number()
                    {
                        if out.get(key).is_none() {
                            out[key] = json!([]);
                        }
                        let values = out[key].as_array_mut().unwrap();
                        if values.len() < 16 {
                            values.push(value.clone());
                        }
                    } else {
                        metrics(value, depth - 1, remaining, out);
                    }
                }
            }
            Value::Array(values) => {
                for value in values.iter().take(16) {
                    metrics(value, depth - 1, remaining, out);
                }
            }
            _ => {}
        }
    }
    let mut numeric = json!({});
    metrics(&value, 8, &mut 256, &mut numeric);
    result["metrics"] = numeric;
    result
}

/// Display refresh capability and selected source, distinct from received FPS.
/// Screen labels, identifiers, and resolution lists remain outside diagnostics.
pub fn screen_parameters(sources: &crate::core::pb::ScreenSources) -> Value {
    json!({
        "screen_count":sources.screens.len(),
        "selected_available":sources.screens.iter().any(|s|s.id == sources.current_screen_id),
        "screens":sources.screens.iter().take(32).map(|s|json!({
            "selected":s.id == sources.current_screen_id,
            "primary":s.is_primary_screen,
            "refresh_fps":s.fps,
            "screen_type":s.screen_type,
            "builtin_screen_type":s.builtin_screen_type,
            "resolution_type":s.resolution_type,
            "width":s.current_resolution.as_ref().map(|r|r.width),
            "height":s.current_resolution.as_ref().map(|r|r.height),
            "initial_width":s.init_resolution.as_ref().map(|r|r.width),
            "initial_height":s.init_resolution.as_ref().map(|r|r.height),
            "track_available":s.video_track_index >= 0
        })).collect::<Vec<_>>()
    })
}

/// SenderParaInfo carries implementation descriptions, not measured FPS.
/// Keep only known implementation names and numeric track indices; a remote
/// string outside the whitelist must never become diagnostic output.
pub fn sender_parameters(value: &Value) -> Value {
    fn implementation(value: &Value) -> &'static str {
        match value.as_str() {
            Some("DXGI" | "dxgi") => "dxgi",
            Some("GDI" | "gdi") => "gdi",
            Some("WGC" | "wgc") => "wgc",
            Some("OpenH264") => "openh264",
            Some("Nvenc V11") => "nvenc_v11",
            Some("Nvenc V8") => "nvenc_v8",
            _ => "other",
        }
    }
    let rows: Vec<&Value> = if let Some(rows) = value["sender_media_infos"].as_array() {
        rows.iter().take(16).collect()
    } else if value.is_object() {
        vec![value]
    } else {
        vec![]
    };
    json!({"tracks":rows.into_iter().map(|row| json!({
        "video_track_index":row["video_track_index"].as_i64(),
        "capture_impl":implementation(&row["capture_impl"]),
        "encoder_impl":implementation(&row["encoder_impl"])
    })).collect::<Vec<_>>()})
}

/// Inspect recognized envelope keys and value types only. Names not in the
/// protocol allowlist and all string contents remain private.
pub fn signal_shape(value: &Value, depth: usize) -> Value {
    match value {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(_) => json!("number"),
        Value::String(text) => json!({"string_bytes":text.len()}),
        Value::Array(values) if depth > 0 => json!({
            "array_items":values.len(),
            "items":values.iter().take(8).map(|v|signal_shape(v,depth-1)).collect::<Vec<_>>()
        }),
        Value::Object(object) if depth > 0 => {
            let mut fields = json!({});
            for key in [
                "data",
                "app_data",
                "streamer_data",
                "_placeholder",
                "num",
                "command",
                "cmd",
                "type",
                "action",
                "name",
                "event",
                "setting",
                "settings",
                "msg_type",
                "message",
                "client_id",
                "ice_id",
                "app_control_id",
                "content",
                "payload",
                "id",
                "device_capability",
                "signal_app_data",
                "binary_data",
                "sender_para_info",
                "sender_media_infos",
                "video_track_index",
                "capture_impl",
                "encoder_impl",
            ] {
                if let Some(value) = object.get(key) {
                    fields[key] = signal_shape(value, depth - 1);
                }
            }
            json!({"object_fields":object.len(),"fields":fields})
        }
        Value::Array(values) => json!({"array_items":values.len()}),
        Value::Object(object) => json!({"object_fields":object.len()}),
    }
}

/// Read only the fixed SPS capability prefix, not encoded picture content.
pub fn h264_sps(nal: &[u8]) -> Option<Value> {
    (nal.len() >= 4 && nal[0] & 31 == 7).then(|| {
        json!({
            "profile_idc":nal[1],"constraint_flags":nal[2],"level_idc":nal[3]
        })
    })
}

/// webrtc-sctp currently exposes these counters only through its logger.
/// Enable explicitly for investigations; persist only allowlisted numbers.
pub fn init_transport_diagnostics() {
    static LOGGER: SctpDiagnostics = SctpDiagnostics;
    if std::env::var("UU_SCTP_TRACE").as_deref() == Ok("1")
        && std::env::var_os("UU_PERF_LOG").is_some()
        && log::set_logger(&LOGGER).is_ok()
    {
        log::set_max_level(log::LevelFilter::Trace);
    }
}

struct SctpDiagnostics;
impl log::Log for SctpDiagnostics {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        if metadata.target() == "webrtc_sctp::diagnostics" {
            static DETAIL: OnceLock<bool> = OnceLock::new();
            return *DETAIL.get_or_init(|| std::env::var("UU_SCTP_DETAIL").as_deref() == Ok("1"));
        }
        matches!(
            metadata.target(),
            "webrtc_sctp::association::association_internal"
                | "dtls::conn"
                | "webrtc_sctp::timer::rtx_timer"
        )
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let message = record.args().to_string();
            if record.target() == "webrtc_sctp::diagnostics" {
                if let Some(numbers) = sctp_detail(&message) {
                    trace("sctp_detail", numbers);
                }
            } else if record.target() == "dtls::conn" {
                if let Some(event) = dtls_event(&message) {
                    trace("dtls", json!({"event":event}));
                }
            } else if record.target() == "webrtc_sctp::timer::rtx_timer" {
                if let Some(event) = sctp_timer_event(&message) {
                    trace("sctp", json!({"event":event}));
                }
            } else if let Some(numbers) = sctp_numbers(&message) {
                trace("sctp", numbers);
            }
        }
    }
    fn flush(&self) {}
}

fn sctp_detail(message: &str) -> Option<Value> {
    let mut parts = message.split_whitespace();
    let event = parts.next()?;
    let fields: &[&str] = match event {
        "packet" => &["packet", "bytes", "marshal_us", "send_us"],
        "tx" => &[
            "packet", "index", "attempt", "bytes", "stream", "begin", "end",
        ],
        "ack" => &["index", "attempt", "gap"],
        "duplicate" => &["index"],
        "send_error" => &["packet"],
        _ => return None,
    };
    let mut result = json!({"event":event});
    for field in fields {
        let (key, value) = parts.next()?.split_once('=')?;
        if key != *field {
            return None;
        }
        let number = value.parse::<u64>().ok()?;
        if matches!(key, "begin" | "end" | "gap") && number > 1 {
            return None;
        }
        result[key] = json!(number);
    }
    parts.next().is_none().then_some(result)
}

fn sctp_timer_event(message: &str) -> Option<&'static str> {
    match message {
        "cancelled expired retransmission timer" => Some("cancelled_expired_timer"),
        "retained restarted retransmission timer" => Some("retained_restarted_timer"),
        _ => None,
    }
}

fn dtls_event(message: &str) -> Option<&'static str> {
    // Do not persist the library's error text; it can contain peer data.
    let (_, message) = message.split_once(": ")?;
    [
        ("discarded broken packet:", "invalid_record"),
        ("discarded future packet (", "future_epoch"),
        ("discarded duplicated packet (", "replay_rejected"),
        ("decrypt failed:", "decrypt_failed"),
        ("defragment failed:", "defragment_failed"),
    ]
    .into_iter()
    .find_map(|(prefix, event)| message.starts_with(prefix).then_some(event))
}

fn sctp_numbers(message: &str) -> Option<Value> {
    let (_, message) = message.split_once("] ")?;
    match message {
        "use ForwardTSN (on init)" | "use ForwardTSN (on initAck)" => {
            return Some(json!({"event":"extensions","forward_tsn":true}));
        }
        "not using ForwardTSN (on init)" | "not using ForwardTSN (on initAck)" => {
            return Some(json!({"event":"extensions","forward_tsn":false}));
        }
        _ => {}
    }
    if let Some(event) = match message {
        "sending INIT" => Some("init_sent"),
        "sending COOKIE-ECHO" => Some("cookie_echo_sent"),
        _ => None,
    } {
        return Some(json!({"event":event}));
    }
    if let Some(change) = message.strip_prefix("state change: '")
        && let Some((from, to)) = change.strip_suffix('\'')?.split_once("' => '")
    {
        let states = [
            "Closed",
            "CookieWait",
            "CookieEchoed",
            "Established",
            "ShutdownPending",
            "ShutdownSent",
            "ShutdownReceived",
            "ShutdownAckSent",
        ];
        if states.contains(&from) && states.contains(&to) {
            return Some(json!({"event":"state","from":from,"to":to}));
        }
        return None;
    }
    let number = |key: &str| -> Option<f64> {
        let value = message
            .split_whitespace()
            .find_map(|part| part.strip_prefix(key))?
            .parse::<f64>()
            .ok()?;
        (value.is_finite() && value >= 0.0).then_some(value)
    };
    if message.starts_with("SACK: measured-rtt=") {
        return Some(json!({"event":"rtt","rtt_ms":number("measured-rtt=")?,
            "srtt_ms":number("srtt=")?,"rto_ms":number("new-rto=")?}));
    }
    if message.starts_with("updated cwnd=") {
        let mode = ["INI", "SS", "CA", "FR", "RTO"]
            .into_iter()
            .find(|mode| message.ends_with(&format!("({mode})")))?;
        return Some(
            json!({"event":"congestion","mode":mode,"cwnd":number("cwnd=")?,
            "ssthresh":number("ssthresh=")?,"inflight":number("inflight="),"acked":number("acked=")}),
        );
    }
    if message.contains(", SACK: cumTSN=") {
        return Some(json!({"event":"ack","receiver_window":number("a_rwnd=")?}));
    }
    if message.starts_with("SACK gaps=") {
        let acknowledged = number("acked_chunks=")?;
        let highest_offset = number("last_offset=")?;
        return Some(json!({"event":"ack_gaps", "blocks":number("gaps=")?,
            "acknowledged_chunks":acknowledged,
            "missing_chunks":(highest_offset-acknowledged).max(0.0),
            "duplicate_chunks":number("duplicates=")?}));
    }
    for (marker, name) in [
        ("stats nDATAs (in) : ", "data_received"),
        ("stats nSACKs (in) : ", "sacks_received"),
        ("stats nT3Timeouts : ", "retransmission_timeouts"),
        ("stats nAckTimeouts: ", "ack_timeouts"),
        ("stats nFastRetrans: ", "fast_retransmissions"),
    ] {
        if let Some(value) = message.strip_prefix(marker) {
            return Some(
                json!({"event":"closed_stats","counter":name,"value":value.trim().parse::<u64>().ok()?}),
            );
        }
    }
    None
}

pub struct DisplayMetrics {
    since: Instant,
    pub upload: Timing,
    pub decoded_to_ui: Timing,
    pub assembled_to_ui: Timing,
    drawn: u64,
    pub root_paints: u64,
    pub remote_calls: u64,
    sequence: (u64, u64),
}
impl Default for DisplayMetrics {
    fn default() -> Self {
        Self {
            since: Instant::now(),
            upload: Timing::default(),
            decoded_to_ui: Timing::default(),
            assembled_to_ui: Timing::default(),
            drawn: 0,
            root_paints: 0,
            remote_calls: 0,
            sequence: (0, 0),
        }
    }
}
impl DisplayMetrics {
    /// Counts unique frames submitted to the canvas, not monitor presentation.
    pub fn draw(&mut self, generation: u64, sequence: u64) {
        if self.sequence != (generation, sequence) {
            self.drawn += 1;
            self.sequence = (generation, sequence);
        }
    }
    pub fn report(&mut self, generation: u64, remote_open: bool, files_open: bool) {
        let elapsed = self.since.elapsed().as_secs_f64();
        if elapsed < 1.0 {
            return;
        }
        trace(
            "display",
            json!({"generation":generation,"interval_s":elapsed,"remote_open":remote_open,"files_open":files_open,
                "root_paints":self.root_paints,"remote_calls":self.remote_calls,
            "canvas_fps":self.drawn as f64/elapsed,"texture_prepare":self.upload.take(),
            "decoded_to_ui":self.decoded_to_ui.take(),"assembled_to_ui":self.assembled_to_ui.take()}),
        );
        self.since = Instant::now();
        self.drawn = 0;
        self.root_paints = 0;
        self.remote_calls = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detailed_transport_trace_rejects_unexpected_fields() {
        assert_eq!(
            sctp_detail("ack index=17 attempt=2 gap=1").unwrap(),
            json!({"event":"ack","index":17,"attempt":2,"gap":1})
        );
        assert_eq!(
            sctp_detail("tx packet=12 index=17 attempt=2 bytes=1160 stream=0 begin=0 end=1")
                .unwrap()["index"],
            17
        );
        assert!(sctp_detail("ack index=17 attempt=2 gap=1 payload=private").is_none());
        assert!(sctp_detail("ack index=17 attempt=2 gap=7").is_none());
        assert!(sctp_detail("packet packet=1 bytes=-5 marshal_us=2 send_us=1").is_none());
        assert!(sctp_detail("packet packet=1 bytes=5 marshal_us=NaN send_us=1").is_none());
        assert!(
            sctp_detail("tx peer=private index=17 attempt=2 bytes=1160 stream=0 begin=0 end=1")
                .is_none()
        );
        assert!(sctp_detail("private message").is_none());
    }
    #[test]
    fn bounded_trace_reports_dropped_records_on_next_success() {
        let (tx, rx) = mpsc::sync_channel(1);
        let dropped = AtomicU64::new(0);
        for index in 0..4 {
            trace_enqueue(&tx, &dropped, json!({"index":index}));
        }
        assert_eq!(rx.try_recv().unwrap()["dropped_before"], 0);
        trace_enqueue(&tx, &dropped, json!({"index":4}));
        assert_eq!(rx.try_recv().unwrap()["dropped_before"], 3);
        trace_enqueue(&tx, &dropped, json!({"index":5}));
        assert_eq!(rx.try_recv().unwrap()["dropped_before"], 0);
    }
    #[test]
    fn transport_diagnostics_keep_only_allowlisted_numbers() {
        let rtt =
            sctp_numbers("[private connection name] SACK: measured-rtt=8 srtt=9.25 new-rto=1000")
                .unwrap();
        assert_eq!(rtt["srtt_ms"], 9.25);
        assert!(!rtt.to_string().contains("private"));
        assert_eq!(
            sctp_numbers("[] updated cwnd=1228 ssthresh=4912 inflight=1160 (RTO)").unwrap()["mode"],
            "RTO"
        );
        assert_eq!(
            sctp_numbers("[] 1234, SACK: cumTSN=1235 a_rwnd=0").unwrap()["receiver_window"],
            0.0
        );
        let gaps =
            sctp_numbers("[private peer] SACK gaps=2 acked_chunks=5 last_offset=8 duplicates=1")
                .unwrap();
        assert_eq!(gaps["missing_chunks"], 3.0);
        assert_eq!(gaps["duplicate_chunks"], 1.0);
        assert!(!gaps.to_string().contains("private"));
        assert!(
            sctp_numbers("[] SACK gaps=1 acked_chunks=NaN last_offset=8 duplicates=0").is_none()
        );
        assert!(sctp_numbers("[] DATA payload=private message").is_none());
        assert_eq!(
            sctp_numbers("[private peer] sending INIT").unwrap()["event"],
            "init_sent"
        );
        assert_eq!(
            sctp_numbers("[private peer] state change: 'CookieWait' => 'CookieEchoed'").unwrap(),
            json!({"event":"state","from":"CookieWait","to":"CookieEchoed"})
        );
        assert!(sctp_numbers("[] state change: 'private peer' => 'Established'").is_none());
        assert_eq!(
            sctp_numbers("[private peer] not using ForwardTSN (on initAck)").unwrap(),
            json!({"event":"extensions","forward_tsn":false})
        );
        assert!(sctp_numbers("[] use ForwardTSN (on initAck) private payload").is_none());
        assert!(sctp_numbers("[] sending INIT private data").is_none());
        assert!(sctp_numbers("[] SACK: measured-rtt=NaN srtt=1 new-rto=1000").is_none());
        assert_eq!(
            dtls_event("client: decrypt failed: private details"),
            Some("decrypt_failed")
        );
        assert_eq!(
            dtls_event("client: discarded duplicated packet (epoch: 1, seq: 19)"),
            Some("replay_rejected")
        );
        assert!(dtls_event("client: unrelated private details").is_none());
        assert_eq!(
            sctp_timer_event("cancelled expired retransmission timer"),
            Some("cancelled_expired_timer")
        );
        assert!(
            sctp_timer_event("cancelled expired retransmission timer private details").is_none()
        );
    }
    #[test]
    fn windows_reset_without_inventing_zero_latency() {
        let timing = Timing::default();
        timing.record(Duration::from_millis(2));
        timing.record(Duration::from_millis(6));
        let report = timing.take();
        assert_eq!(report["count"], 2);
        assert_eq!(report["mean_ms"], 4.0);
        assert_eq!(report["max_ms"], 6.0);
        assert!(timing.take()["mean_ms"].is_null());
    }
    #[test]
    fn signaling_shapes_never_include_string_contents_or_unknown_keys() {
        assert_eq!(
            signal_shape(
                &json!({"data":{"app_data":{"_placeholder":true,"num":0},
            "ice_id":"secret","private_key":"secret"}}),
                6
            ),
            json!({
                "object_fields":1,"fields":{"data":{"object_fields":3,"fields":{
                    "app_data":{"object_fields":2,"fields":{"_placeholder":"bool","num":"number"}},
                    "ice_id":{"string_bytes":6}
                }}}
            })
        );
        assert_eq!(
            signal_shape(&json!({"data":{"id":"secret"}}), 0),
            json!({"object_fields":1})
        );
    }
    #[test]
    fn screen_diagnostics_exclude_names_and_identifiers_and_bound_rows() {
        use crate::core::pb::{Screen, ScreenSources, WinRect};
        let source = Screen {
            id: 12345,
            display_name: "private display label".into(),
            video_track_index: 54321,
            fps: 60,
            current_resolution: Some(WinRect {
                width: 1920,
                height: 1080,
                ..Default::default()
            }),
            init_resolution: Some(WinRect {
                width: 1680,
                height: 1050,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut sources = ScreenSources {
            screens: vec![source; 100],
            current_screen_id: 12345,
        };
        let report = screen_parameters(&sources);
        assert_eq!(report["screens"].as_array().unwrap().len(), 32);
        assert_eq!(report["screen_count"], 100);
        assert_eq!(report["screens"][0]["refresh_fps"], 60);
        assert_eq!(report["screens"][0]["selected"], true);
        assert_eq!(report["screens"][0]["width"], 1920);
        assert_eq!(report["screens"][0]["initial_width"], 1680);
        assert_eq!(report["screens"][0]["initial_height"], 1050);
        for private in ["private display label", "12345", "54321"] {
            assert!(!report.to_string().contains(private));
        }
        sources.current_screen_id = 7;
        assert_eq!(screen_parameters(&sources)["selected_available"], false);
        sources.screens[0].current_resolution = None;
        assert!(screen_parameters(&sources)["screens"][0]["width"].is_null());
    }
    #[test]
    fn sender_diagnostics_allow_only_known_names_and_numeric_tracks() {
        let row = json!({"video_track_index":0,"capture_impl":"DXGI",
            "encoder_impl":"private host name","ice_id":"private"});
        let expected = json!({"video_track_index":0,"capture_impl":"dxgi","encoder_impl":"other"});
        assert_eq!(
            sender_parameters(&row),
            json!({"tracks":[expected.clone()]})
        );
        let many = json!({"sender_media_infos":vec![row;100]});
        assert_eq!(
            sender_parameters(&many),
            json!({"tracks":vec![expected;16]})
        );
        assert_eq!(sender_parameters(&Value::Null), json!({"tracks":[]}));
    }
    #[test]
    fn streamer_diagnostics_exclude_unknown_fields_and_bound_output() {
        let data = json!({"connection_period_stats_event":{
            "private_address":"secret", "private_device":123456,
            "encoder_fps":60,"encoder_input_fps":"secret",
            "streams":[{"encoder_fps":30,"total_bitrate_bps":2000000,"timestamp":123}]
        }})
        .to_string();
        assert_eq!(
            streamer_event(data.as_bytes()),
            json!({
                "bytes":data.len(),"parsed":true,"event":"connection_period_stats_event",
                "metrics":{"encoder_fps":[60,30],"total_bitrate_bps":[2000000]}
            })
        );
        let oversized = vec![b' '; 512 * 1024 + 1];
        assert_eq!(
            streamer_event(&oversized),
            json!({"bytes":oversized.len(),"parsed":false})
        );
        assert_eq!(
            streamer_event(b"private"),
            json!({"bytes":7,"parsed":false})
        );
        let many = json!({"streams":vec![json!({"encoder_fps":60});100]}).to_string();
        assert_eq!(
            streamer_event(many.as_bytes())["metrics"]["encoder_fps"]
                .as_array()
                .unwrap()
                .len(),
            16
        );
    }
    #[test]
    fn codec_diagnostics_keep_only_capability_numbers() {
        let capabilities = json!({"control_id":"private", "device_capability":{
            "ice_id":"private", "display_info":[{"id":"private","fps":60}],
            "video_codec_capability":[{"video_codec":1,"width":1440,"height":1080,
                "codec_impl":37,"bit_depth":"private","name":"private","unknown":99}]
        }});
        assert_eq!(
            streamer_capabilities(&capabilities.to_string()),
            json!({
                "parsed":true,"complete":true,"display_fps":[60],"video_codecs":[{
                    "video_codec":1,"width":1440,"height":1080,"codec_impl":37
                }]
            })
        );
        assert_eq!(streamer_capabilities("invalid"), json!({"parsed":false}));
        assert_eq!(
            h264_format(
                "profile-level-id=42e034;packetization-mode=1;address=private;max-fs=36864"
            ),
            json!({"profile_idc":66,"constraint_flags":224,"level_idc":52,
                "packetization-mode":1,"max-fs":36864})
        );
        assert_eq!(
            h264_format("profile-level-id=secret;max-mbps=private;other=123"),
            json!({})
        );
        assert_eq!(
            h264_sps(&[0x67, 66, 224, 31, 1, 2, 3]).unwrap(),
            json!({"profile_idc":66,"constraint_flags":224,"level_idc":31})
        );
        assert!(h264_sps(&[0x67, 66, 224]).is_none());
        assert!(h264_sps(&[0x65, 66, 224, 31]).is_none());
        assert!(h264_sps(&[]).is_none());
    }
    #[test]
    fn round_trip_measurements_exclude_unrelated_and_late_replies() {
        let start = Instant::now();
        let probe = RoundTripProbe::new(42, 7, start);
        let now = start + Duration::from_millis(25);
        assert!(probe.reply(0, now).is_none()); // handshake response
        assert!(probe.reply(41, now).is_none()); // previous request
        assert_eq!(
            probe.reply(42, now).unwrap(),
            json!({"id":7,"matched":true,"round_trip_ms":25.0})
        );
        let late = start + Duration::from_secs(6);
        assert!(probe.expired(late));
        assert!(probe.reply(42, late).is_none());
        assert_eq!(
            probe.timeout(),
            json!({"id":7,"matched":false,"round_trip_ms":null})
        );
    }
    #[test]
    fn repainting_the_same_frame_does_not_count_as_video_progress() {
        let mut metrics = DisplayMetrics::default();
        metrics.draw(1, 10);
        metrics.draw(1, 10);
        metrics.draw(1, 11);
        metrics.draw(2, 1);
        assert_eq!(metrics.drawn, 3);
    }
}
