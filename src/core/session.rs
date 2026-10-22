use super::{
    cloud::Cloud,
    media::{self, H264Assembler, Metrics},
    pb,
    signal::{self, Signal},
    wire,
};
use crate::backend::{Event, VideoFrame};
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use bytes::Bytes;
use prost::Message;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex, atomic::Ordering, mpsc as stdmpsc},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use webrtc::{
    api::{
        APIBuilder,
        interceptor_registry::{configure_rtcp_reports, configure_twcc_receiver_only},
        media_engine::MediaEngine,
    },
    data_channel::{
        RTCDataChannel, data_channel_init::RTCDataChannelInit,
        data_channel_state::RTCDataChannelState,
    },
    ice_transport::{ice_candidate::RTCIceCandidateInit, ice_server::RTCIceServer},
    interceptor::registry::Registry,
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        peer_connection_state::RTCPeerConnectionState,
        policy::ice_transport_policy::RTCIceTransportPolicy,
        sdp::session_description::RTCSessionDescription,
    },
    rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication,
    rtp_transceiver::{
        RTCRtpTransceiverInit,
        rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType},
        rtp_transceiver_direction::RTCRtpTransceiverDirection,
    },
};

const CONTROL: &str = "CONTROL_DATA_CHANNEL";
const TEXT: &str = "TEXT_DATA_CHANNEL";

fn startup_ready(
    wait_video: bool,
    probed: bool,
    protocol: bool,
    tools_open: bool,
    frames: u64,
) -> bool {
    probed
        && if wait_video {
            frames > 0
        } else {
            protocol && tools_open
        }
}

fn set_video_wait(
    deadline: &mut Option<Instant>,
    visible: bool,
    first_frame_seen: bool,
    now: Instant,
) {
    if !visible || first_frame_seen {
        *deadline = None;
    } else if deadline.is_none() {
        *deadline = Some(now + Duration::from_secs(40));
    }
}

#[cfg(test)]
mod startup_tests {
    use super::{Duration, Instant, set_video_wait, startup_ready};
    #[test]
    fn tools_require_business_handshake_and_channels_but_not_video() {
        assert!(!startup_ready(false, false, true, true, 0));
        assert!(!startup_ready(false, true, false, true, 100));
        assert!(!startup_ready(false, true, true, false, 100));
        assert!(startup_ready(false, true, true, true, 0));
    }
    #[test]
    fn desktop_still_waits_for_real_decoded_video() {
        assert!(!startup_ready(true, true, true, true, 0));
        assert!(!startup_ready(true, false, true, true, 1));
        assert!(startup_ready(true, true, true, true, 1));
    }

    #[test]
    fn closing_video_cancels_timeout_for_remaining_tools_and_reopen_gets_a_new_deadline() {
        let start = Instant::now();
        let mut deadline = None;
        set_video_wait(&mut deadline, true, false, start);
        let first_deadline = deadline.unwrap();
        set_video_wait(&mut deadline, true, false, start + Duration::from_secs(5));
        assert_eq!(deadline, Some(first_deadline));
        set_video_wait(&mut deadline, false, false, start + Duration::from_secs(10));
        assert!(deadline.is_none());
        let reopened = first_deadline + Duration::from_secs(5);
        set_video_wait(&mut deadline, true, false, reopened);
        assert!(deadline.unwrap() > reopened);
        set_video_wait(&mut deadline, true, true, reopened + Duration::from_secs(1));
        assert!(deadline.is_none());
    }
}

#[derive(Clone)]
pub struct Output {
    pub media: Arc<crate::media_bus::MediaBus>,
    pub generation: Option<u64>,
    pub events: stdmpsc::Sender<Event>,
    pub latest: Arc<Mutex<Option<VideoFrame>>>,
    pub ctx: eframe::egui::Context,
}
impl Output {
    pub fn event(&self, event: Event) {
        let _ = self.events.send(event);
        crate::backend::repaint_ui(&self.ctx, false);
    }
    pub fn native(&self, kind: &str, mut data: Value) {
        if let Some(generation) = self.generation {
            data["generation"] = json!(generation);
        }
        if kind == "stats" || kind == "capture_result" || kind == "released" {
            crate::perf::trace(if kind == "stats" { "core" } else { kind }, data.clone());
        }
        self.event(Event::Native {
            kind: kind.into(),
            data,
        });
    }
}

pub struct Session {
    terminal: super::terminal::Terminal,
    terminal_pending: Option<(i64, Instant, Value)>,
    ports: super::ports::Ports,
    files: super::files::Files,
    super_screen: super::super_screen::SuperScreen,
    drag_files: super::drag_files::DragFiles,
    file_offer: Option<super::clipboard_files::FileOffer>,
    clipboard_files_pending: Option<(u32, Arc<tempfile::TempDir>)>,
    clipboard_cache_count: usize,
    clipboard_cache_bytes: u64,
    drag_receive: bool,
    download_dir: std::path::PathBuf,
    generation: u64,
    output: Output,
    cancel: CancellationToken,
    signal: Option<Signal>,
    signals: Option<mpsc::Receiver<Result<signal::Event>>>,
    pc: Option<Arc<RTCPeerConnection>>,
    channels: HashMap<String, Arc<RTCDataChannel>>,
    incoming: mpsc::Receiver<(String, Bytes)>,
    incoming_tx: mpsc::Sender<(String, Bytes)>,
    ice: String,
    remote: String,
    control_id: String,
    recovered: Arc<std::sync::atomic::AtomicU64>,
    metrics: Arc<Metrics>,
    audio: Option<media::Audio>,
    microphone: Option<super::microphone::Microphone>,
    microphone_transceiver: Option<Arc<webrtc::rtp_transceiver::RTCRtpTransceiver>>,
    microphone_pending: Option<(i64, bool, Instant)>,
    audio_subscription: bool,
    capture_requested: Value,
    remote_streamer: Value,
    virtual_displays: super::virtual_display::VirtualDisplays,
    clipboard: Option<super::clipboard::Clipboard>,
    clipboard_enabled: bool,
    clipboard_observe: bool,
    clipboard_offer: Option<i64>,
    clipboard_paste_offer: Option<i64>,
    files_allowed: bool,
    assistance: bool,
    inhibitor: Option<crate::platform::Inhibitor>,
    lock_on_disconnect: bool,
    local_files_allowed: bool,
    read_only: bool,
    clipboard_wire: super::clipboard_wire::ClipboardData,
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    touches: BTreeSet<u32>,
    touch_supported: Option<bool>,
    pending_candidates: Vec<RTCIceCandidateInit>,
    answered: bool,
    started: Instant,
    protocol_ready: bool,
    protobuf_text: bool,
    startup_text: bool,
    early_signal_handshake: bool,
    file_control: bool,
    request_video_track: bool,
    selected_video_track: Option<i32>,
    video_track_request: Option<i64>,
    lookup_probe: Option<crate::perf::RoundTripProbe>,
    first_frame_announced: bool,
    video_deadline: Option<Instant>,
    screen_preference: Option<bool>,
    video_visible: bool,
    startup_recovery: super::startup_recovery::StartupRecovery,
    deferred_capture_pending: bool,
}

impl Session {
    pub fn new(generation: u64, mut output: Output, cancel: CancellationToken) -> Self {
        output.generation = Some(generation);
        let (incoming_tx, incoming) = mpsc::channel(256);
        Self {
            terminal: Default::default(),
            terminal_pending: None,
            ports: Default::default(),
            files: Default::default(),
            super_screen: Default::default(),
            generation,
            output,
            cancel,
            signal: None,
            signals: None,
            pc: None,
            channels: HashMap::new(),
            incoming,
            incoming_tx,
            ice: String::new(),
            remote: String::new(),
            control_id: uuid::Uuid::new_v4().to_string(),
            recovered: Default::default(),
            metrics: Arc::new(Metrics::default()),
            audio: None,
            microphone: None,
            microphone_transceiver: None,
            microphone_pending: None,
            audio_subscription: false,
            capture_requested: json!({}),
            remote_streamer: Value::Null,
            virtual_displays: Default::default(),
            clipboard: None,
            clipboard_enabled: true,
            clipboard_observe: false,
            clipboard_offer: None,
            clipboard_paste_offer: None,
            files_allowed: true,
            assistance: false,
            inhibitor: None,
            lock_on_disconnect: false,
            local_files_allowed: true,
            read_only: false,
            clipboard_wire: Default::default(),
            drag_files: Default::default(),
            clipboard_files_pending: None,
            clipboard_cache_count: 0,
            clipboard_cache_bytes: 0,
            file_offer: None,
            drag_receive: false,
            download_dir: Default::default(),
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            touches: BTreeSet::new(),
            touch_supported: None,
            pending_candidates: vec![],
            answered: false,
            started: Instant::now(),
            protocol_ready: false,
            protobuf_text: false,
            startup_text: false,
            early_signal_handshake: false,
            file_control: false,
            request_video_track: false,
            selected_video_track: None,
            video_track_request: None,
            lookup_probe: None,
            first_frame_announced: false,
            video_deadline: None,
            screen_preference: None,
            video_visible: true,
            startup_recovery: Default::default(),
            deferred_capture_pending: false,
        }
    }
    fn phase(&self, phase: &str) {
        self.phase_at(phase, Instant::now());
    }
    fn phase_at(&self, phase: &str, at: Instant) {
        crate::perf::trace(
            "connection_phase",
            json!({
                "generation":self.generation,"phase":phase,
                "elapsed_ms":at.saturating_duration_since(self.started).as_secs_f64()*1000.0
            }),
        );
        self.output.event(Event::Phase {
            generation: self.generation,
            phase: phase.into(),
        });
    }
    fn observe_first_frame(&mut self) {
        if !self.first_frame_announced
            && let Some(at) = *self.metrics.first_frame.lock().unwrap()
        {
            self.first_frame_announced = true;
            self.video_deadline = None;
            self.phase_at("first_frame", at);
        }
    }
    pub async fn run(
        mut self,
        cloud: Cloud,
        target: super::cloud::Target,
        mut settings: Value,
        mut commands: mpsc::Receiver<Value>,
    ) {
        self.output.event(Event::Connecting {
            generation: self.generation,
        });
        if !settings.is_object() {
            settings = json!({});
        }
        if std::env::var("UU_STARTUP_DEFER_CAPTURE").as_deref() == Ok("1")
            && std::env::var_os("UU_PERF_LOG").is_some()
        {
            settings["startup_file_only"] = json!(true);
            settings["startup_capture_after_handshake"] = json!(true);
            // Isolate the alternate startup from stop/restart recovery.
            settings["startup_capture_recovery"] = json!(false);
        }
        self.deferred_capture_pending = settings["startup_file_only"] == true
            && settings["startup_capture_after_handshake"] == true;
        // Diagnostic comparison only: muting normally keeps audio negotiated so
        // it can be enabled during the session without renegotiating.
        if settings["video_only"] == true {
            settings["audio"] = json!(false);
        }
        settings["assistance"] = json!(matches!(target, super::cloud::Target::Assistance { .. }));
        self.assistance = settings["assistance"].as_bool().unwrap_or(false);
        self.lock_on_disconnect = settings["lock_on_disconnect"].as_bool().unwrap_or(false);
        self.local_files_allowed = settings["allow_files"].as_bool().unwrap_or(true);
        self.read_only = settings["read_only"].as_bool().unwrap_or(false);
        self.screen_preference = settings["super_screen"].as_bool();
        self.capture_requested = settings.clone();
        if let Some(preference) = settings["decoder_preference"]
            .as_str()
            .filter(|p| super::decoder::preferences().contains(p))
        {
            *self.output.media.decoder_preference.lock().unwrap() = preference.into();
        }
        self.video_visible = settings["wait_for_video"].as_bool().unwrap_or(true);
        if settings["startup_capture_recovery"] == false {
            self.startup_recovery.disable();
        }
        // Opt-in comparisons until the official TEXT/track startup path has
        // passed real-device compatibility and latency checks.
        self.protobuf_text = settings["protobuf_text"].as_bool().unwrap_or(false);
        self.startup_text = settings["startup_text"].as_bool().unwrap_or(false);
        self.early_signal_handshake =
            settings["early_signal_handshake"] == true && std::env::var_os("UU_PERF_LOG").is_some();
        self.file_control = settings["file_control"].as_bool().unwrap_or(false);
        self.request_video_track = settings["request_video_track"].as_bool().unwrap_or(false);
        crate::perf::trace(
            "protocol_route",
            json!({
                "protobuf_text":self.protobuf_text,"startup_text":self.startup_text,
                "early_signal_handshake":self.early_signal_handshake,
                "file_control":self.file_control,"request_video_track":self.request_video_track,
                "file_channel":if self.file_control { CONTROL } else { TEXT }
            }),
        );
        self.files_allowed = !self.assistance;
        self.output
            .native("file_permission", json!({"enabled":self.files_allowed}));
        let cancel = self.cancel.clone();
        let result = tokio::select! {
            _=cancel.cancelled()=>Ok(()),
            result=tokio::time::timeout(Duration::from_secs(60),self.start(&cloud,&target,&settings))=>result.unwrap_or_else(|_|Err(anyhow::anyhow!("建立连接超时"))),
        };
        drop(target);
        if result.is_ok() && !cancel.is_cancelled() && settings["keep_awake"] == true {
            self.keep_awake(true).await;
        }
        let result = if result.is_ok() && !cancel.is_cancelled() {
            self.connected_loop(&mut commands).await
        } else {
            result
        };
        self.close().await;
        if let Err(e) = result {
            self.output.event(Event::Error {
                scope: "connection".into(),
                message: format!("{e}"),
                generation: Some(self.generation),
                login_id: None,
            });
        }
        self.output.event(Event::Disconnected {
            generation: self.generation,
        });
    }
    async fn start(
        &mut self,
        cloud: &Cloud,
        target: &super::cloud::Target,
        settings: &Value,
    ) -> Result<()> {
        self.clipboard_enabled = settings["clipboard"].as_bool().unwrap_or(true);
        self.drag_receive = settings["drag_receive"].as_bool().unwrap_or(false);
        self.download_dir = settings["download_dir"].as_str().unwrap_or("").into();
        if let Some(bytes) = settings["upload_block_bytes"].as_u64() {
            self.files.set_upload_block_bytes(bytes)?;
        }
        self.clipboard = Some(super::clipboard::Clipboard::start(
            self.clipboard_enabled,
            self.incoming_tx.clone(),
        ));
        self.phase("room_join_started");
        let grant = cloud.join_target(target).await?;
        self.phase("room_joined");
        let (signal, mut events) = Signal::connect(
            grant["signaling_server"].as_str().context("缺少信令地址")?,
            grant["token"].as_str().context("缺少房间凭据")?,
        )
        .await?;
        self.phase("signaling_connected");
        self.signal = Some(signal.clone());
        let (room, _) = signal.emit("room_info", vec![], vec![], true).await?;
        self.phase("room_info");
        let (result,attachments)=signal.emit("control",vec![json!({
            "app_control_id":self.control_id,
            "app_data":{"_placeholder":true,"num":0},
            "streamer_data":wire::streamer(&self.control_id,settings["fps"].as_i64().unwrap_or(60))
        })],vec![wire::options(&cloud.device_id(),settings)],true).await?;
        ensure!(result[0] == "success", "远端拒绝控制连接");
        let result = &result[1];
        if std::env::var_os("UU_PERF_LOG").is_some() {
            let app_data = (result["app_data"]["_placeholder"] == true)
                .then(|| result["app_data"]["num"].as_u64())
                .flatten()
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| attachments.get(index));
            crate::perf::trace(
                "control_reply",
                json!({
                    "attachments":attachments.len(),"app_data_bytes":app_data.map(Vec::len),
                    "streamer_capabilities":crate::perf::streamer_capabilities(
                        result["streamer_data"].as_str().unwrap_or("")
                    )
                }),
            );
        }
        self.output.native(
            "remote_streamer",
            crate::perf::streamer_capabilities(result["streamer_data"].as_str().unwrap_or("")),
        );
        self.ice = result["ice_id"].as_str().context("缺少 ICE 标识")?.into();
        self.remote = result["client_id"].as_str().context("缺少远端标识")?.into();
        self.phase("control_accepted");
        if settings["startup_file_only"] == true && settings["startup_signal_capture"] == true {
            self.send_signal_app(wire::action(8)).await?;
            self.phase("deferred_capture_requested");
        }
        if self.early_signal_handshake {
            self.send_signal_app(wire::action(0)).await?;
            self.send_signal_app(wire::action(
                if settings["audio"].as_bool().unwrap_or(true) {
                    25
                } else {
                    26
                },
            ))
            .await?;
            self.phase("signal_handshake_sent");
        }
        let raw = result.get("iceServers").unwrap_or(&room[0]["iceServers"]);
        let mut servers = vec![];
        if let Some(list) = raw.as_array() {
            for value in list {
                let urls = if let Some(s) = value["urls"].as_str().or(value["url"].as_str()) {
                    vec![s.to_owned()]
                } else {
                    value["urls"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default()
                };
                servers.push(RTCIceServer {
                    urls,
                    username: value["username"].as_str().unwrap_or("").into(),
                    credential: value["credential"].as_str().unwrap_or("").into(),
                });
            }
        }
        let h264_fmtp = match settings["h264_level"].as_str().unwrap_or("3.1") {
            "3.1" => "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
            "5.2" => "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e034",
            _ => bail!("H.264 能力等级无效"),
        };
        crate::perf::trace("h264_offer", crate::perf::h264_format(h264_fmtp));
        let mut engine = MediaEngine::default();
        for (mime, pt, fmt, rate, channels, kind) in [
            ("video/H264", 102, h264_fmtp, 90000, 0, RTPCodecType::Video),
            ("video/rtx", 103, "apt=102", 90000, 0, RTPCodecType::Video),
            (
                "video/H265",
                104,
                "profile-id=1;sprop-max-don-diff=0",
                90000,
                0,
                RTPCodecType::Video,
            ),
            ("video/rtx", 105, "apt=104", 90000, 0, RTPCodecType::Video),
            (
                "audio/opus",
                111,
                "minptime=10;useinbandfec=1",
                48000,
                2,
                RTPCodecType::Audio,
            ),
        ] {
            engine.register_codec(
                RTCRtpCodecParameters {
                    capability: RTCRtpCodecCapability {
                        mime_type: mime.into(),
                        clock_rate: rate,
                        channels,
                        sdp_fmtp_line: fmt.into(),
                        ..Default::default()
                    },
                    payload_type: pt,
                    ..Default::default()
                },
                kind,
            )?;
        }
        for parameter in ["", "pli"] {
            engine.register_feedback(
                webrtc::rtp_transceiver::RTCPFeedback {
                    typ: "nack".into(),
                    parameter: parameter.into(),
                },
                RTPCodecType::Video,
            );
        }
        let mut registry =
            configure_twcc_receiver_only(configure_rtcp_reports(Registry::new()), &mut engine)?;
        let (video_tx, mut video_rx) = mpsc::channel(1024);
        let primary_ssrc = Arc::new(std::sync::atomic::AtomicU32::new(0));
        registry.add(Box::new(super::repair::Repair {
            packets: video_tx,
            recovered: self.recovered.clone(),
            primary_ssrc: primary_ssrc.clone(),
        }));
        let mut setting = webrtc::api::setting_engine::SettingEngine::default();
        let udp_receive_bytes = super::network::requested_receive_buffer_bytes(
            settings,
            std::env::var_os("UU_PERF_LOG").is_some(),
        );
        setting.set_vnet(Some(Arc::new(super::network::system_network(
            udp_receive_bytes,
            |buffer| crate::perf::trace("udp_socket", json!(buffer)),
        ))));
        setting.detach_data_channels();
        setting.set_sctp_max_message_size_can_send(
            webrtc::api::setting_engine::SctpMaxMessageSize::Bounded(2 * 1024 * 1024),
        );
        let api = APIBuilder::new()
            .with_media_engine(engine)
            .with_interceptor_registry(registry)
            .with_setting_engine(setting)
            .build();
        let relay_only = settings["relay_only"] == true
            || std::env::var("UU_ICE_RELAY_ONLY").as_deref() == Ok("1");
        let turn_urls = servers
            .iter()
            .flat_map(|s| &s.urls)
            .filter(|url| url.starts_with("turn:") || url.starts_with("turns:"))
            .count();
        crate::perf::trace(
            "ice_config",
            json!({"relay_only":relay_only,"turn_urls":turn_urls}),
        );
        ensure!(
            !relay_only || turn_urls > 0,
            "远端未提供可用中继，请关闭“使用中继连接”后重试"
        );
        let pc = Arc::new(
            api.new_peer_connection(RTCConfiguration {
                ice_servers: servers,
                ice_transport_policy: if relay_only {
                    RTCIceTransportPolicy::Relay
                } else {
                    RTCIceTransportPolicy::All
                },
                ..Default::default()
            })
            .await?,
        );
        self.pc = Some(pc.clone());
        if std::env::var_os("UU_PERF_LOG").is_some() {
            let peer = Arc::downgrade(&pc);
            let cancel = self.cancel.clone();
            let metrics = self.metrics.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(1));
                loop {
                    tokio::select! { _ = cancel.cancelled() => break, _ = tick.tick() => {} }
                    let Some(peer) = peer.upgrade() else { break };
                    let report = tokio::select! {
                        _ = cancel.cancelled() => break,
                        report = tokio::time::timeout(Duration::from_millis(250), peer.get_stats()) => report,
                    };
                    if let Ok(report) = report {
                        for stat in report.reports.into_values() {
                            if let webrtc::stats::StatsReportType::Transport(stat) = stat {
                                crate::perf::trace(
                                    "startup_transport",
                                    json!({
                                        "received_bytes":stat.bytes_received,"sent_bytes":stat.bytes_sent,
                                        "video_packets":metrics.rtp_packets.load(Ordering::Relaxed),
                                        "decoded_frames":metrics.decoded.load(Ordering::Relaxed)
                                    }),
                                );
                            }
                        }
                    } else {
                        crate::perf::trace("startup_stats_timeout", json!({}));
                    }
                    if metrics.decoded.load(Ordering::Relaxed) > 0 {
                        break;
                    }
                }
            });
        }
        pc.on_peer_connection_state_change(Box::new(|state| {
            Box::pin(async move {
                crate::perf::trace("connection_state", json!({"state":state.to_string()}));
            })
        }));
        pc.on_ice_connection_state_change(Box::new(|state| {
            Box::pin(async move {
                crate::perf::trace("ice_state", json!({"state":state.to_string()}));
            })
        }));
        let video = media::video_decoder(
            self.generation,
            self.output.latest.clone(),
            self.metrics.clone(),
            self.output.ctx.clone(),
            self.output.media.clone(),
        );
        let audio = media::Audio::start(self.metrics.clone());
        audio.enabled.store(
            settings["audio"].as_bool().unwrap_or(true),
            Ordering::Relaxed,
        );
        if let Some(id) = settings["output_device"]
            .as_str()
            .filter(|id| *id != "default")
        {
            *audio.device.lock().unwrap() = Some(id.to_owned());
        }
        let audio_packets = audio.packets.clone();
        self.audio = Some(audio);
        let metrics = self.metrics.clone();
        let cancel = self.cancel.clone();
        let peer = Arc::downgrade(&pc);
        let media_bus = self.output.media.clone();
        tokio::spawn(async move {
            let mut assembler = H264Assembler::default();
            let mut interval = tokio::time::interval(Duration::from_millis(20));
            let mut last_pli = Instant::now() - Duration::from_secs(2);
            let mut last_loss = 0;
            let mut ssrc = 0;
            let mut last_nack = Instant::now();
            let mut last_video_timestamp = None;
            loop {
                let frames = tokio::select! {
                    _=cancel.cancelled()=>break,
                    _=interval.tick()=>assembler.expire(),
                    packet=video_rx.recv()=>{
                        let Some(packet)=packet else{break};
                        if ssrc != packet.header.ssrc {
                            crate::perf::trace("video_stream_transition", json!({
                                "initial":ssrc == 0,
                                "received_packets":metrics.rtp_packets.load(Ordering::Relaxed),
                                "decoded_frames":metrics.decoded.load(Ordering::Relaxed)
                            }));
                            ssrc=packet.header.ssrc;
                        }
                        if metrics.rtp_packets.fetch_add(1,Ordering::Relaxed) == 0 {
                            crate::perf::trace("media_start", json!({"stage":"video_packet"}));
                        }
                        metrics.rtp_bytes.fetch_add(packet.payload.len() as u64,Ordering::Relaxed);
                        assembler.push(packet)
                    }
                };
                if last_nack.elapsed() > Duration::from_millis(40) && ssrc != 0 {
                    let missing = assembler.missing();
                    if !missing.is_empty()
                        && let Some(peer) = peer.upgrade()
                    {
                        use webrtc::rtcp::transport_feedbacks::transport_layer_nack::{
                            NackPair, TransportLayerNack,
                        };
                        metrics
                            .nacks
                            .fetch_add(missing.len() as u64, Ordering::Relaxed);
                        let nacks = missing
                            .into_iter()
                            .map(|packet_id| NackPair {
                                packet_id,
                                lost_packets: 0,
                            })
                            .collect();
                        let _ = peer
                            .write_rtcp(&[Box::new(TransportLayerNack {
                                sender_ssrc: 0,
                                media_ssrc: ssrc,
                                nacks,
                            })])
                            .await;
                    }
                    last_nack = Instant::now();
                }
                metrics.losses.store(assembler.losses, Ordering::Relaxed);
                metrics
                    .late_packets
                    .store(assembler.late_packets, Ordering::Relaxed);
                for frame in frames {
                    if let Some((ssrc, previous)) =
                        last_video_timestamp.replace((frame.rtp_ssrc, frame.rtp_timestamp))
                        && ssrc == frame.rtp_ssrc
                        && let Some(interval) =
                            media::rtp_frame_interval(previous, frame.rtp_timestamp)
                    {
                        metrics.rtp_frame_interval.record(interval);
                    }
                    if metrics.samples.fetch_add(1, Ordering::Relaxed) == 0 {
                        crate::perf::trace("media_start", json!({"stage":"video_access_unit"}));
                    }
                    let geometry = media_bus.video(
                        frame.data.clone(),
                        frame.rtp_timestamp,
                        frame.rtp_ssrc,
                        frame.losses,
                        frame.codec,
                    );
                    if video
                        .try_send(media::EncodedFrame {
                            geometry,
                            unit: frame,
                            queue_loss: metrics.queue_drops.load(Ordering::Relaxed),
                        })
                        .is_err()
                    {
                        metrics.queue_drops.fetch_add(1, Ordering::Relaxed);
                    }
                }
                let loss = assembler.losses
                    + metrics.queue_drops.load(Ordering::Relaxed)
                    + metrics.decode_errors.load(Ordering::Relaxed);
                let stale = metrics
                    .last_frame
                    .lock()
                    .unwrap()
                    .is_some_and(|t| t.elapsed() > Duration::from_millis(1500));
                let pli_ssrc = if ssrc != 0 {
                    ssrc
                } else {
                    primary_ssrc.load(Ordering::Relaxed)
                };
                if (loss > last_loss
                    || metrics.decoded.load(Ordering::Relaxed) == 0
                    || stale
                    || media_bus.request_keyframe.load(Ordering::Relaxed))
                    && pli_ssrc != 0
                    && last_pli.elapsed() > Duration::from_millis(500)
                {
                    media_bus.request_keyframe.store(false, Ordering::Relaxed);
                    metrics.pli.fetch_add(1, Ordering::Relaxed);
                    last_loss = loss;
                    last_pli = Instant::now();
                    if let Some(peer) = peer.upgrade() {
                        let result = peer
                            .write_rtcp(&[Box::new(PictureLossIndication {
                                sender_ssrc: 0,
                                media_ssrc: pli_ssrc,
                            })])
                            .await;
                        if metrics.decoded.load(Ordering::Relaxed) == 0 {
                            crate::perf::trace(
                                "media_start",
                                json!({
                                    "stage":"startup_keyframe_request","submitted":result.is_ok(),
                                    "before_first_packet":ssrc == 0
                                }),
                            );
                        }
                    }
                }
            }
        });
        let cancel = self.cancel.clone();
        let report_metrics = self.metrics.clone();
        let media_bus = self.output.media.clone();
        pc.on_track(Box::new(move|track,receiver,_|{
            let audio=audio_packets.clone();let cancel=cancel.clone();
            let metrics = report_metrics.clone();
            let media_bus=media_bus.clone();
            Box::pin(async move {
                let report_cancel = cancel.clone();
                let report_media=media_bus.clone();
                // on_track runs after the receiver has installed its streams.
                // Starting this before negotiation races the empty track list.
                tokio::spawn(async move {
                    loop {
                        let packets = tokio::select! {
                            _ = report_cancel.cancelled() => break,
                            result = receiver.read_rtcp() => match result {
                                Ok((packets,_)) => packets,
                                Err(_) => break,
                            },
                        };
                        metrics.rtcp_packets.fetch_add(packets.len() as u64, Ordering::Relaxed);
                        for packet in packets {if let Some(report)=packet.as_any().downcast_ref::<webrtc::rtcp::sender_report::SenderReport>() {report_media.sender_report(report.ssrc,report.rtp_time,report.ntp_time);}}
                    }
                });
                tokio::spawn(async move {
                    loop {
                        // read_rtp is not cancellation safe. Cancel only when closing.
                        let result=tokio::select!{_=cancel.cancelled()=>break,result=track.read_rtp()=>result};
                        let Ok((packet,_))=result else{break};
                        if track.kind()==RTPCodecType::Audio {media_bus.audio(packet.payload.clone(),packet.header.timestamp,packet.header.ssrc);let _=audio.try_send(packet.payload);}
                    }
                });
            })
        }));
        for kind in [RTPCodecType::Video, RTPCodecType::Audio] {
            if kind == RTPCodecType::Audio && settings["video_only"] == true {
                continue;
            }
            if kind == RTPCodecType::Audio && settings["microphone_track"] != false {
                let track = Arc::new(webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample::new(
                    RTCRtpCodecCapability { mime_type: "audio/opus".into(), clock_rate:48000, channels:2, sdp_fmtp_line:"minptime=10;useinbandfec=1".into(), ..Default::default() },
                    "microphone".into(), "uux".into()));
                let transceiver = pc
                    .add_transceiver_from_track(
                        track.clone(),
                        Some(RTCRtpTransceiverInit {
                            direction: RTCRtpTransceiverDirection::Sendrecv,
                            send_encodings: vec![],
                        }),
                    )
                    .await?;
                self.microphone_transceiver = Some(transceiver);
                self.microphone = Some(super::microphone::Microphone::start(
                    track,
                    self.cancel.clone(),
                ));
                continue;
            }
            pc.add_transceiver_from_kind(
                kind,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                }),
            )
            .await?;
        }
        for label in [
            CONTROL,
            TEXT,
            "STREAMER_DATA_CHANNEL",
            "FILE_DATA_CHANNEL",
            "BINARY_DATA_CHANNEL",
        ] {
            let channel = pc
                .create_data_channel(
                    label,
                    Some(RTCDataChannelInit {
                        ordered: Some(true),
                        ..Default::default()
                    }),
                )
                .await?;
            let tx = self.incoming_tx.clone();
            let label = label.to_string();
            let name = label.clone();
            let weak = Arc::downgrade(&channel);
            let cancel = self.cancel.clone();
            channel.on_open(Box::new(move|| Box::pin(async move {
                let Some(channel)=weak.upgrade() else{return};
                crate::perf::trace("channel_open", json!({"channel":name}));
                let Ok(raw)=channel.detach().await else{return};
                tokio::spawn(async move {
                    let mut buffer=vec![0;2*1024*1024];
                    loop {
                        let result=tokio::select!{_=cancel.cancelled()=>break,r=raw.read_data_channel(&mut buffer)=>r};
                        let Ok((n,_))=result else{break};if n==0{break;}
                        if tx.send((name.clone(),Bytes::copy_from_slice(&buffer[..n]))).await.is_err(){break;}
                    }
                });
            })));
            self.channels.insert(label, channel);
        }
        let ice = self.ice.clone();
        let remote = self.remote.clone();
        let control = self.control_id.clone();
        let candidate_signal = signal.clone();
        pc.on_ice_candidate(Box::new(move |candidate| {
            let signal = candidate_signal.clone();
            let ice = ice.clone();
            let remote = remote.clone();
            let control = control.clone();
            Box::pin(async move {
                if let Some(candidate) = candidate
                    && let Ok(value) = candidate.to_json()
                {
                    let _=signal.emit("soac",vec![json!({"client_id":remote,"data":{
                        "ice_id":ice,"app_control_id":control,"type":"candidate","candidate":value
                    }})],vec![],false).await;
                }
            })
        }));
        // Send the offer immediately; candidate callbacks keep forwarding paths
        // discovered later. The former wait remains available for diagnostics.
        let gather_wait_ms = settings["ice_gather_wait_ms"]
            .as_u64()
            .unwrap_or(0)
            .min(2000);
        let mut gathering = if gather_wait_ms > 0 {
            Some(pc.gathering_complete_promise().await)
        } else {
            None
        };
        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer).await?;
        let gather_started = Instant::now();
        if let Some(gathering) = &mut gathering {
            let _ =
                tokio::time::timeout(Duration::from_millis(gather_wait_ms), gathering.recv()).await;
        }
        crate::perf::trace(
            "ice_gather_wait",
            json!({
                "limit_ms":gather_wait_ms,"waited_ms":gather_started.elapsed().as_secs_f64()*1000.0
            }),
        );
        let offer = pc.local_description().await.context("无法生成 SDP")?;
        crate::perf::trace(
            "offer_media",
            json!({
                "video_tracks":offer.sdp.lines().filter(|line| line.starts_with("m=video ")).count(),
                "audio_tracks":offer.sdp.lines().filter(|line| line.starts_with("m=audio ")).count()
            }),
        );
        signal
            .emit(
                "soac",
                vec![json!({"client_id":self.remote,"data":{
                    "ice_id":self.ice,"app_control_id":self.control_id,"type":"offer",
                    "sdp":wire::data_channel_limit(&offer.sdp),"ice_network_type":3
                }})],
                vec![],
                false,
            )
            .await?;
        self.phase("offer_sent");
        let wait_for_video = settings["wait_for_video"].as_bool().unwrap_or(true);
        let deadline = Instant::now() + Duration::from_secs(40);
        let mut ticker = tokio::time::interval(Duration::from_millis(50));
        let mut probed = false;
        let mut last_probe = Instant::now() - Duration::from_secs(1);
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=>return Ok(()),
                _=ticker.tick()=>{
                    ensure!(Instant::now()<deadline,if wait_for_video {"等待远端画面超时"} else {"等待远端数据通道超时"});
                    ensure!(pc.connection_state()!=RTCPeerConnectionState::Failed,"WebRTC 连接失败");
                    self.begin_deferred_capture().await?;
                    self.recover_video_startup().await?;
                    if self.channels[CONTROL].ready_state()==RTCDataChannelState::Open
                        && self.channels[self.handshake_channel()].ready_state()==RTCDataChannelState::Open
                        && !self.protocol_ready && last_probe.elapsed() >= Duration::from_millis(500) {
                        if !probed { self.phase("control_channel_open"); }
                        self.send_handshake(wire::action(0)).await?;
                        if !probed { self.send_handshake(wire::action(if settings["audio"].as_bool().unwrap_or(true){25}else{26})).await?; }
                        probed=true;
                        last_probe=Instant::now();
                    }
                    let tools_open = [CONTROL,TEXT,"BINARY_DATA_CHANNEL"].iter()
                        .all(|name| self.channels[*name].ready_state()==RTCDataChannelState::Open);
                    if (!wait_for_video || (!self.super_screen.is_pending() && !self.startup_recovery.in_flight())) && startup_ready(wait_for_video,probed,self.protocol_ready,tools_open,
                        self.metrics.decoded.load(Ordering::Relaxed)) {break;}
                },
                event=events.recv()=>self.signal_event(event.context("信令已结束")??).await?,
                data=self.incoming.recv()=>if let Some((label,data))=data {self.data(&label,&data).await?;},
            }
        }
        self.signals = Some(events);
        self.observe_first_frame();
        self.phase("session_ready");
        if let Some(pair) = pc
            .sctp()
            .transport()
            .ice_transport()
            .get_selected_candidate_pair()
            .await
        {
            crate::perf::trace(
                "transport",
                json!({"local_type":pair.local.typ.to_string(),
                "remote_type":pair.remote.typ.to_string(),"protocol":pair.local.protocol.to_string()}),
            );
        }
        self.output.event(Event::Connected {
            generation: self.generation,
        });
        Ok(())
    }
    async fn signal_event(&mut self, event: signal::Event) -> Result<()> {
        if let Some(caps) = signal::forwarded_capabilities(&event, &self.remote, &self.ice) {
            self.remote_streamer = caps.clone();
            self.output.native("remote_streamer", caps);
        }
        if event.name != "soac" && std::env::var_os("UU_PERF_LOG").is_some() {
            let name = match event.name.as_str() {
                "streamer_push" => "streamer_push",
                "forward_setting" => "forward_setting",
                "be-controlled" => "be-controlled",
                "released" => "released",
                "left" => "left",
                _ => "other",
            };
            crate::perf::trace(
                "signal_aux_event",
                json!({"event":name,"arguments":event.args.len(),"attachments":event.binary.len(),
                    "shapes":event.args.iter().take(8).map(|v|crate::perf::signal_shape(v,6)).collect::<Vec<_>>() }),
            );
            if name == "forward_setting" {
                if let Some(data) = event.args.first().and_then(|arg| arg.get("data")) {
                    let kind = match data["type"].as_str() {
                        Some("device_capability") => "device_capability",
                        Some("signal_app_data") => "signal_app_data",
                        Some("sender_para_info") => "sender_para_info",
                        Some("connection_period_stats_event") => "connection_period_stats_event",
                        Some("connection_ending_stats_event") => "connection_ending_stats_event",
                        _ => "other",
                    };
                    crate::perf::trace(
                        "forward_setting",
                        json!({
                            "type":kind,"capabilities":crate::perf::streamer_capabilities(&data.to_string()),
                            "sender":crate::perf::sender_parameters(&data["sender_para_info"])
                        }),
                    );
                }
                for data in event.binary.iter().take(8) {
                    self.trace_startup_message("receive", "SIGNAL_FORWARD_SETTING", data);
                }
            }
        }
        if let Some(bytes) = signal::forwarded_app_data(&event, &self.remote, &self.ice)
            && let Ok(message) = pb::Message::decode(bytes)
        {
            match message.which {
                Some(pb::message::Which::SimpleAction(action))
                    if action.action == 0 && self.early_signal_handshake =>
                {
                    self.send_signal_app(
                        pb::Message {
                            seq: message.seq,
                            timestamp: message.timestamp,
                            which: Some(pb::message::Which::SimpleAction(pb::SimpleAction {
                                action: 1,
                                params: Some(pb::simple_action::Params::FeatureFlag(
                                    wire::features(),
                                )),
                                ..Default::default()
                            })),
                        }
                        .encode_to_vec(),
                    )
                    .await?;
                }
                Some(pb::message::Which::SimpleAction(action))
                    if action.action == 1 && self.early_signal_handshake =>
                {
                    if let Some(pb::simple_action::Params::FeatureFlag(flags)) = action.params {
                        self.output.native("remote_features", json!({"capture_setting":flags.ff_capture_setting,"simple_action":flags.ff_simple_action,"system_metrics":flags.ff_system_metrics,"clipboard":flags.ff_clipboard,"file_transfer":flags.ff_file_transfer_ftp,"file_transfer_v2":flags.ff_file_transfer_ftp2,"virtual_mouse":flags.ff_virtual_mouse_device}));
                    }
                    // This confirms only the signaling route. The normal SCTP
                    // ECHO still establishes protocol_ready and the probe gate.
                    self.phase("signal_echo_received");
                }
                Some(
                    pb::message::Which::Screens(_)
                    | pb::message::Which::CodecNegotiation(_)
                    | pb::message::Which::SystemMetrics(_)
                    | pb::message::Which::CaptureSettingSync(_),
                ) => {
                    self.data("SIGNAL_APP_DATA", bytes).await?;
                }
                _ => {}
            }
        }
        if event.name == "released" || event.name == "left" {
            let arg = event.args.first().unwrap_or(&Value::Null);
            let ice = arg["ice_id"].as_str().or(arg["data"]["ice_id"].as_str());
            let client = arg["client_id"].as_str().or(arg.as_str());
            if ice.is_some_and(|id| id == self.ice)
                || (ice.is_none() && client.is_some_and(|id| id == self.remote))
            {
                bail!("远端已结束远控");
            }
            return Ok(());
        }
        if event.name != "soac" {
            return Ok(());
        }
        let data = event
            .args
            .first()
            .context("SOAC 缺少参数")?
            .get("data")
            .context("SOAC 格式错误")?;
        if data["ice_id"] != self.ice {
            return Ok(());
        }
        let pc = self.pc.as_ref().context("连接未初始化")?;
        match data["type"].as_str() {
            Some("answer") => {
                pc.set_remote_description(RTCSessionDescription::answer(signal::sdp(&event)?)?)
                    .await?;
                self.answered = true;
                for candidate in self.pending_candidates.drain(..) {
                    pc.add_ice_candidate(candidate).await?;
                }
                self.phase("answer_received");
                self.audio_negotiation().await;
            }
            Some("candidate") => {
                let candidate: RTCIceCandidateInit =
                    serde_json::from_value(data["candidate"].clone())?;
                if self.answered {
                    pc.add_ice_candidate(candidate).await?;
                } else if self.pending_candidates.len() < 256 {
                    self.pending_candidates.push(candidate);
                }
            }
            Some("offer") => {
                pc.set_remote_description(RTCSessionDescription::offer(signal::sdp(&event)?)?)
                    .await?;
                let answer = pc.create_answer(None).await?;
                pc.set_local_description(answer.clone()).await?;
                self.signal.as_ref().unwrap().emit("soac",vec![json!({"client_id":self.remote,"data":{
                    "ice_id":self.ice,"app_control_id":self.control_id,"type":"answer","sdp":wire::data_channel_limit(&answer.sdp),"ice_network_type":3
                }})],vec![],false).await?;
                self.audio_negotiation().await;
            }
            _ => {}
        }
        Ok(())
    }
    async fn audio_negotiation(&self) {
        if let Some(pc) = &self.pc {
            let tracks = pc.get_transceivers().await.into_iter()
                .filter(|t| t.kind() == RTPCodecType::Audio)
                .map(|t| json!({"offered":t.direction().to_string(),"negotiated":t.current_direction().to_string()}))
                .collect::<Vec<_>>();
            self.output
                .native("audio_negotiation", json!({"tracks":tracks}));
        }
    }
    async fn send_protobuf(&self, data: Vec<u8>) -> Result<()> {
        self.send_channel(self.protobuf_channel(), self.protobuf_text, data)
            .await
    }
    async fn send_signal_app(&self, data: Vec<u8>) -> Result<()> {
        self.trace_startup_message("send", "SIGNAL_APP_DATA", &data);
        tokio::time::timeout(
            Duration::from_secs(2),
            self.signal.as_ref().context("信令未建立")?.emit(
                "forward_setting",
                vec![signal::app_data_envelope(&self.remote, &self.ice)],
                vec![data],
                false,
            ),
        )
        .await??;
        Ok(())
    }
    fn handshake_channel(&self) -> &'static str {
        if self.startup_text {
            TEXT
        } else {
            self.protobuf_channel()
        }
    }
    async fn send_handshake(&self, data: Vec<u8>) -> Result<()> {
        let channel = self.handshake_channel();
        self.send_channel(channel, channel == TEXT, data).await
    }
    fn protobuf_channel(&self) -> &'static str {
        if self.protobuf_text { TEXT } else { CONTROL }
    }
    async fn send_control(&self, data: Vec<u8>) -> Result<()> {
        self.send_channel(CONTROL, false, data).await
    }
    async fn send_file(&self, data: Vec<u8>) -> Result<()> {
        if self.file_control {
            self.send_control(data).await
        } else {
            // Official protobuf uses TEXT/PPID 51. Keep file fragments off the
            // ordered CONTROL stream so a missing file chunk cannot also hold
            // already-received keyboard and mouse messages in that stream.
            self.send_channel(TEXT, true, data).await
        }
    }
    async fn send_channel(&self, label: &str, is_string: bool, data: Vec<u8>) -> Result<()> {
        let channel = self.channels.get(label).context("控制通道未建立")?;
        ensure!(
            channel.ready_state() == RTCDataChannelState::Open,
            "控制通道未就绪"
        );
        ensure!(
            channel.buffered_amount().await < 2 * 1024 * 1024,
            "控制通道发送积压"
        );
        let started = Instant::now();
        if self.metrics.decoded.load(Ordering::Relaxed) == 0 {
            self.trace_startup_message("send", label, &data);
        }
        tokio::time::timeout(
            Duration::from_secs(2),
            channel
                .detach()
                .await?
                // Official TEXT carries raw protobuf bytes with PPID 51; it
                // must not be UTF-8 converted. Key/mouse JSON stays PPID 53.
                .write_data_channel(&Bytes::from(data), is_string),
        )
        .await??;
        self.metrics.send_time.record(started.elapsed());
        Ok(())
    }
    fn trace_startup_message(&self, direction: &str, label: &str, data: &[u8]) {
        if std::env::var_os("UU_PERF_LOG").is_none() {
            return;
        }
        let (kind, action) = match pb::Message::decode(data)
            .ok()
            .and_then(|message| message.which)
        {
            Some(pb::message::Which::SimpleAction(action)) => {
                ("simple_action", Some(action.action))
            }
            Some(pb::message::Which::RpcRequest(_)) => ("rpc_request", None),
            Some(pb::message::Which::RpcResponse(_)) => ("rpc_response", None),
            Some(pb::message::Which::Screens(_)) => ("screens", None),
            Some(pb::message::Which::SystemStateChange(_)) => ("system_state", None),
            Some(pb::message::Which::SystemMetrics(_)) => ("system_metrics", None),
            Some(pb::message::Which::CodecNegotiation(_)) => ("codec_negotiation", None),
            Some(pb::message::Which::CaptureChange(_)) => ("capture_change", None),
            Some(pb::message::Which::CaptureConfig(_)) => ("capture_config", None),
            Some(pb::message::Which::CaptureConfigResponse(_)) => ("capture_config_response", None),
            Some(pb::message::Which::CaptureSettingSync(_)) => ("capture_setting_sync", None),
            Some(pb::message::Which::ReportQosStats(_)) => ("report_qos_stats", None),
            Some(_) => ("other_protobuf", None),
            None => ("other", None),
        };
        crate::perf::trace(
            "startup_message",
            json!({
                "direction":direction,"channel":label,"kind":kind,"action":action,"bytes":data.len(),
                "gzip":data.starts_with(&[0x1f,0x8b])
            }),
        );
    }
    async fn announce_download_path(&self) -> Result<()> {
        if self.drag_receive
            && self.files_allowed
            && self.local_files_allowed
            && self.download_dir.is_absolute()
            && self.download_dir.is_dir()
        {
            self.send_protobuf(wire::wrap(pb::message::Which::RemoteDownloadPath(
                pb::RemoteDownloadPath {
                    download_path: self.download_dir.to_string_lossy().into_owned(),
                },
            )))
            .await?;
        }
        Ok(())
    }
    async fn start_selected_video_track(&mut self) -> Result<()> {
        if self.request_video_track
            && self.protocol_ready
            && self.video_track_request.is_none()
            && let Some(track) = self.selected_video_track
        {
            let (id, bytes) = wire::rpc(pb::rpc_request::Which::SendVideoTrackReq(
                pb::SendVideoTrackRequest {
                    video_track_index: vec![track],
                },
            ));
            self.send_protobuf(bytes).await?;
            self.video_track_request = Some(id);
            self.phase("video_track_requested");
        }
        Ok(())
    }
    async fn restore_screen_preference(&mut self) -> Result<()> {
        if let Some(bytes) = self.super_screen.restore_preference(
            &mut self.screen_preference,
            self.video_visible,
            self.protocol_ready,
            &self.output,
        )? {
            self.send_protobuf(bytes).await?;
            self.phase("screen_mode_requested");
        }
        Ok(())
    }
    async fn recover_video_startup(&mut self) -> Result<()> {
        let eligible = self.video_visible
            && self.protocol_ready
            && self.super_screen.ordinary_screen_selected()
            && self.screen_preference != Some(true);
        if let Some(action) = self.startup_recovery.poll(
            Instant::now(),
            eligible,
            self.metrics.rtp_packets.load(Ordering::Relaxed) > 0,
        ) {
            self.send_protobuf(wire::action(action)).await?;
            self.phase(if action == 7 {
                "startup_capture_stopped"
            } else {
                "startup_capture_restarted"
            });
        }
        Ok(())
    }
    async fn begin_deferred_capture(&mut self) -> Result<()> {
        if self.deferred_capture_pending && self.protocol_ready && self.video_visible {
            self.send_protobuf(wire::action(8)).await?;
            self.phase("deferred_capture_requested");
            self.deferred_capture_pending = false;
        }
        Ok(())
    }
    async fn finish_startup_recovery(&mut self) -> Result<()> {
        if self.startup_recovery.in_flight() {
            self.send_protobuf(wire::action(8)).await?;
            self.phase("startup_capture_restarted");
        }
        self.startup_recovery.disable();
        Ok(())
    }
    async fn data(&mut self, label: &str, data: &[u8]) -> Result<()> {
        if label == "BINARY_DATA_CHANNEL" && data.starts_with(b"TERM") {
            if let Err(error) = self.terminal.receive(data, &self.output) {
                self.output.native(
                    "terminal_status",
                    json!({"kind":8,"result":{"message":error.to_string()}}),
                );
            }
            return Ok(());
        }
        if let Some(id) = label.strip_prefix("CLIPBOARD_FILES_OFFER:") {
            if let Some(offer) = &self.file_offer
                && offer.active()
                && Some(offer.id) == id.parse::<i32>().ok()
            {
                if !self.files_allowed || !self.local_files_allowed {
                    offer.reject("文件传输权限已关闭");
                } else if offer
                    .geometry
                    .is_some_and(|g| g != self.output.media.geometry())
                    || (offer.geometry.is_some() && self.read_only)
                {
                    offer.reject("拖放画面已失效，请在当前屏幕重新拖放");
                } else {
                    self.send_file(data.to_vec()).await?;
                    if let Some(offer) = &mut self.file_offer {
                        offer.input_pending = offer.paste;
                    }
                }
            }
            return Ok(());
        }
        if let Some(id) = label.strip_prefix("CLIPBOARD_FILES_PACKET:") {
            if self.files_allowed
                && self.local_files_allowed
                && self
                    .file_offer
                    .as_ref()
                    .is_some_and(|f| f.active() && Some(f.id) == id.parse::<i32>().ok())
            {
                self.send_file(data.to_vec()).await?;
            }
            return Ok(());
        }
        if label == "CLIPBOARD_APPLIED" {
            self.output.native(
                "clipboard_applied",
                json!({"bytes":String::from_utf8_lossy(data).parse::<usize>().unwrap_or(0)}),
            );
            return Ok(());
        }
        if label == "CLIPBOARD_ERROR" {
            self.output.native(
                "clipboard_error",
                json!({"message":String::from_utf8_lossy(data)}),
            );
            return Ok(());
        }
        if label == "LOCAL_CLIPBOARD_RESULT" {
            self.output
                .native("local_clipboard_result", serde_json::from_slice(data)?);
            return Ok(());
        }
        if matches!(
            label,
            "LOCAL_CLIPBOARD"
                | "LOCAL_CLIPBOARD_PASTE"
                | "LOCAL_CLIPBOARD_IMAGE"
                | "LOCAL_CLIPBOARD_IMAGE_PASTE"
                | "LOCAL_CLIPBOARD_FILES"
                | "LOCAL_CLIPBOARD_FILES_PASTE"
        ) {
            for packet in self.cancel_clipboard_files() {
                self.send_file(packet).await?;
            }
        }
        if let Some(id) = label.strip_prefix("CLIPBOARD_FILES_PASTE:") {
            if self.clipboard_enabled
                && !self.read_only
                && self
                    .file_offer
                    .as_ref()
                    .is_some_and(|f| f.active() && Some(f.id) == id.parse::<i32>().ok())
            {
                if let Some(offer) = &mut self.file_offer {
                    offer.input_pending = false;
                }
                self.paste_keys().await?;
            }
            return Ok(());
        }
        if matches!(
            label,
            "LOCAL_CLIPBOARD_FILES_PASTE" | "LOCAL_CLIPBOARD_FILES"
        ) {
            if !self.clipboard_enabled
                || (self.read_only && label == "LOCAL_CLIPBOARD_FILES_PASTE")
                || !self.files_allowed
                || !self.local_files_allowed
            {
                self.output.native(
                    "clipboard_error",
                    json!({"message":"当前会话未允许文件粘贴"}),
                );
                return Ok(());
            }
            let paths: Value = serde_json::from_slice(data)?;
            let id = -(((uuid::Uuid::new_v4().as_u128() as u32) & 0x1fff_ffff).max(1000) as i32);
            self.file_offer = None;
            self.clipboard_wire.clear_offer();
            match super::clipboard_files::FileOffer::start(
                &json!({"paths":paths,"queued_id":id,"paste":label=="LOCAL_CLIPBOARD_FILES_PASTE"}),
                self.output.clone(),
                self.incoming_tx.clone(),
            ) {
                Ok(offer) => self.file_offer = Some(offer),
                Err(e) => self
                    .output
                    .native("clipboard_error", json!({"message":e.to_string()})),
            }
            return Ok(());
        }
        if matches!(
            label,
            "LOCAL_CLIPBOARD_IMAGE" | "LOCAL_CLIPBOARD_IMAGE_PASTE"
        ) {
            if self.clipboard_enabled {
                self.file_offer = None;
                let (id, bytes) = self.clipboard_wire.offer_image(data)?;
                self.send_file(bytes).await?;
                self.clipboard_offer = Some(id);
                self.clipboard_paste_offer = if label == "LOCAL_CLIPBOARD_IMAGE_PASTE" {
                    Some(id)
                } else {
                    None
                };
            }
            return Ok(());
        }
        if label == "LOCAL_CLIPBOARD" || label == "LOCAL_CLIPBOARD_PASTE" {
            if (self.clipboard_enabled || label == "LOCAL_CLIPBOARD_PASTE")
                && let Ok(text) = std::str::from_utf8(data)
            {
                if self.clipboard_enabled {
                    self.file_offer = None;
                    let (_, bytes) = wire::rpc(pb::rpc_request::Which::TextChangeRequest(
                        pb::ClipboardTextChangeRequest {
                            format_id: 13,
                            data: text.into(),
                        },
                    ));
                    self.send_protobuf(bytes).await?;
                    self.clipboard_wire.clear_offer();
                }
                if label == "LOCAL_CLIPBOARD_PASTE" {
                    // Use the direct text path for local paste; it avoids a
                    // race between asynchronous host clipboard update and Ctrl+V.
                    self.send_control(
                        json!({"action":"text_input","content":text})
                            .to_string()
                            .into_bytes(),
                    )
                    .await?;
                }
            }
            return Ok(());
        }
        if label == "STREAMER_DATA_CHANNEL" {
            if std::env::var_os("UU_PERF_LOG").is_some() {
                crate::perf::trace("streamer_event", crate::perf::streamer_event(data));
            }
            return Ok(());
        }
        if self.metrics.decoded.load(Ordering::Relaxed) == 0 {
            self.trace_startup_message("receive", label, data);
        }
        let decoded = pb::Message::decode(data);
        if std::env::var_os("UU_PROTOCOL_TRACE").is_some() {
            let kind = decoded
                .as_ref()
                .ok()
                .and_then(|m| m.which.as_ref())
                .map(std::mem::discriminant);
            eprintln!(
                "dc label={label} bytes={} kind={kind:?} seq={:?}",
                data.len(),
                decoded.as_ref().map(|m| m.seq)
            );
            if let Ok(pb::Message {
                which: Some(pb::message::Which::RpcRequest(r)),
                ..
            }) = &decoded
                && let Some(pb::rpc_request::Which::FileTransferFtpRequest(f)) = &r.which
            {
                eprintln!(
                    "ftp request header={:?} kind={:?}",
                    r.request_header,
                    f.which.as_ref().map(std::mem::discriminant)
                );
            }
        }
        let Ok(message) = decoded else {
            return Ok(());
        };
        match message.which {
            Some(pb::message::Which::PortMappingFrame(frame)) => self.ports.receive(frame),
            Some(pb::message::Which::CodecNegotiation(capabilities)) => {
                let data = json!({"encoders":capabilities.encoder_cap_list.iter().take(32).map(|cap| json!({
                    "capture_mode":cap.capture_mode,"fps":cap.fps,"codec_type":cap.corec_type,
                    "width":cap.resolution_width,"height":cap.resolution_height
                })).collect::<Vec<_>>()});
                crate::perf::trace("remote_encoder_capabilities", data.clone());
                self.output.native("remote_encoders", data);
            }
            Some(pb::message::Which::SystemMetrics(metrics)) => {
                if let Some(general) = metrics.general {
                    self.touch_supported = Some(general.support_touch);
                    self.output.native("remote_general", json!({"support_touch":general.support_touch,"system_sound_support":general.system_sound_support,"win_os_ver":general.win_os_ver}));
                }
            }
            Some(pb::message::Which::ReportQosStats(stats)) => {
                crate::perf::trace(
                    "remote_qos",
                    json!({
                        "probe_bps":stats.probe_bps,"quality":stats.video_quality,
                        "fast_bitrate":stats.fast_bitrate,"general_bitrate":stats.general_bitrate,
                        "hd_bitrate":stats.hd_bitrate,"bluray_bitrate":stats.bluray_bitrate
                    }),
                );
            }
            Some(pb::message::Which::CaptureSettingSync(settings)) => {
                self.output.native(
                    "remote_capture",
                    json!({
                        "fps_enum":settings.fps,"quality":settings.frame_quality,
                        "max_bitrate":settings.max_custom_bitrate,
                        "width":settings.resolution_width,"height":settings.resolution_height,
                        "chroma_format":settings.chroma_format,
                        "resolution_type":settings.resolution_type,
                        "pixel_width":settings.resolution_pixel_width,
                        "pixel_height":settings.resolution_pixel_height,"dpi":settings.dpi_scale
                    }),
                );
            }
            Some(pb::message::Which::SystemStateChange(state)) => {
                if let Some(pb::system_state_change::Which::RemoteAssistFileTransferState(
                    permission,
                )) = state.which
                    && self.assistance
                {
                    self.files_allowed = permission.enabled;
                    if !permission.enabled {
                        self.files = Default::default();
                        for message in self.drag_files.cancel(&self.output) {
                            self.send_file(message).await?;
                        }
                    }
                    self.output
                        .native("file_permission", json!({"enabled":permission.enabled}));
                }
            }
            Some(pb::message::Which::SimpleAction(action)) if action.action == 0 => {
                self.send_handshake(
                    pb::Message {
                        seq: message.seq,
                        timestamp: message.timestamp,
                        which: Some(pb::message::Which::SimpleAction(pb::SimpleAction {
                            action: 1,
                            params: Some(pb::simple_action::Params::FeatureFlag(wire::features())),
                            ..Default::default()
                        })),
                    }
                    .encode_to_vec(),
                )
                .await?
            }
            Some(pb::message::Which::SimpleAction(action)) if action.action == 1 => {
                if let Some(pb::simple_action::Params::FeatureFlag(flags)) = action.params {
                    self.output.native("remote_features", json!({"capture_setting":flags.ff_capture_setting,"simple_action":flags.ff_simple_action,"system_metrics":flags.ff_system_metrics,"clipboard":flags.ff_clipboard,"file_transfer":flags.ff_file_transfer_ftp,"file_transfer_v2":flags.ff_file_transfer_ftp2,"virtual_mouse":flags.ff_virtual_mouse_device}));
                }
                if !self.protocol_ready {
                    self.protocol_ready = true;
                    self.phase("protobuf_ready");
                    self.announce_download_path().await?;
                    self.send_protobuf(wire::action(22)).await?;
                    // Official checkSystemMetric requests GENERAL and SETTING explicitly.
                    self.send_protobuf(wire::wrap(pb::message::Which::SystemMetrics(
                        pb::SystemMetrics {
                            metrics: vec![1, 2],
                            ..Default::default()
                        },
                    )))
                    .await?;
                    self.start_selected_video_track().await?;
                    self.bootstrap_headless().await?;
                    self.restore_screen_preference().await?;
                }
            }
            Some(pb::message::Which::Screens(screens)) => {
                if self.output.media.geometry().screen_id != screens.current_screen_id {
                    self.release_inputs().await;
                }
                self.output.media.screen(screens.current_screen_id);
                crate::perf::trace(
                    "screen_parameters",
                    crate::perf::screen_parameters(&screens),
                );
                self.selected_video_track = wire::selected_video_track(&screens);
                crate::perf::trace(
                    "video_track_available",
                    json!({
                        "selected_available":self.selected_video_track.is_some(),
                        "protocol_ready":self.protocol_ready
                    }),
                );
                self.start_selected_video_track().await?;
                self.super_screen.screens(&screens, &self.output);
                self.virtual_displays.screens(&screens, &self.output);
                self.bootstrap_headless().await?;
                self.restore_screen_preference().await?;
                let rows: Vec<Value> = screens
                    .screens
                    .into_iter()
                    .map(|s| {
                        json!({
                            "id":s.id,"name":s.display_name,"primary":s.is_primary_screen,
                            "screen_type":s.screen_type,"fps":s.fps,"dpi":s.dpi_scale.as_ref().map(|d|d.current_dpi),"dpis":s.dpi_scale.as_ref().map(|d|&d.dpis),"dpr":s.dpr,"video_track_index":s.video_track_index,
                            "width":s.current_resolution.as_ref().map(|r|r.width),
                            "height":s.current_resolution.as_ref().map(|r|r.height),
                            "resolutions":s.resolutions.iter().map(|r|json!({"width":r.width,"height":r.height})).collect::<Vec<_>>()
                        })
                    })
                    .collect();
                self.output.native(
                    "screens",
                    json!({"screens":rows,"current":screens.current_screen_id}),
                );
            }
            Some(pb::message::Which::ClipboardChange(text)) => {
                for packet in self.cancel_clipboard_files() {
                    self.send_file(packet).await?;
                }
                if let Some(offer) = &self.file_offer {
                    offer.remote_replaced();
                }
                self.file_offer = None;
                self.clipboard_wire.clear_offer();
                self.clipboard_text(Some(text.content));
            }
            Some(pb::message::Which::RpcRequest(request)) => {
                let id = request
                    .request_header
                    .as_ref()
                    .map(|h| h.request_id)
                    .unwrap_or(0);
                match request.which {
                    Some(pb::rpc_request::Which::TextChangeRequest(text)) => {
                        for packet in self.cancel_clipboard_files() {
                            self.send_file(packet).await?;
                        }
                        if let Some(offer) = &self.file_offer {
                            offer.remote_replaced();
                        }
                        self.file_offer = None;
                        self.clipboard_wire.clear_offer();
                        self.clipboard_text(Some(text.data));
                        self.send_protobuf(wire::response(
                            id,
                            pb::rpc_response::Which::TextChangeResponse(
                                pb::ClipboardTextChangeResponse { err: 1 },
                            ),
                        ))
                        .await?;
                    }
                    Some(pb::rpc_request::Which::FileTransferFtpRequest(ftp)) => {
                        if !self.files_allowed || !self.local_files_allowed {
                            return Ok(());
                        }
                        for bytes in self.file_request(id, ftp) {
                            self.send_file(bytes).await?;
                        }
                    }
                    Some(pb::rpc_request::Which::ClipRequest(request)) => {
                        let sync_files = self.clipboard_enabled
                            && self.files_allowed
                            && self.local_files_allowed
                            && matches!(&request.which,Some(pb::clipboard_request::Which::FormatList(f)) if f.has_action==0 && f.formats.iter().any(|f|f.name.eq_ignore_ascii_case("FileGroupDescriptorW")));
                        crate::perf::trace(
                            "clipboard_request",
                            super::clipboard_wire::request_summary(&request),
                        );
                        if matches!(
                            request.which,
                            Some(pb::clipboard_request::Which::FormatList(_))
                        ) {
                            for packet in self.cancel_clipboard_files() {
                                self.send_file(packet).await?;
                            }
                            if let Some(offer) = &self.file_offer {
                                offer.remote_replaced();
                            }
                            self.file_offer = None;
                        }
                        if let Some(offer) = &self.file_offer {
                            match offer.request(id, request.clone()) {
                                Ok(true) => return Ok(()),
                                Ok(false) => {}
                                Err(e) => {
                                    self.output.native(
                                        "clipboard_error",
                                        json!({"message":e.to_string()}),
                                    );
                                    return Ok(());
                                }
                            }
                        }
                        if super::drag_files::is_drag_request(&request) {
                            for message in self.drag_files.request(
                                id,
                                request,
                                self.drag_receive && self.files_allowed && self.local_files_allowed,
                                &self.download_dir,
                                &self.output,
                            ) {
                                self.send_file(message).await?;
                            }
                            self.receive_clipboard_files();
                            return Ok(());
                        }
                        match self.clipboard_wire.request(
                            id,
                            request,
                            self.clipboard_enabled && !sync_files,
                        ) {
                            Ok((messages, text)) => {
                                for message in messages {
                                    self.send_file(message).await?;
                                }
                                self.clipboard_text(text);
                                self.clipboard_image();
                            }
                            Err(e) => self
                                .output
                                .native("clipboard_error", json!({"message":e.to_string()})),
                        }
                        if sync_files {
                            match self.begin_clipboard_files() {
                                Ok(packets) => {
                                    for packet in packets {
                                        self.send_file(packet).await?;
                                    }
                                }
                                Err(e) => self
                                    .output
                                    .native("clipboard_error", json!({"message":e.to_string()})),
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(pb::message::Which::RpcResponse(response)) => {
                if let Some(pb::rpc_response::Which::TerminalCheckEnvResp(result)) = &response.which
                    && self.terminal_pending.as_ref().map(|p| p.0)
                        == response.response_header.as_ref().map(|h| h.request_id)
                    && let Some((_, _, command)) = self.terminal_pending.take()
                {
                    self.output
                        .native("terminal_environment", json!({"code":result.code}));
                    if matches!(result.code, 0 | 200) {
                        self.send_terminal(&command).await?;
                    } else {
                        self.output.native("terminal_status",json!({"kind":8,"result":{"code":result.code,"message":result.message}}));
                    }
                    return Ok(());
                }
                if let Some(pb::rpc_response::Which::FileTransferFtpResponse(ftp)) = &response.which
                    && let Some(pb::file_transfer_ftp_response::Which::FileDirectory(directory)) =
                        &ftp.which
                    && let Some(header) = &response.response_header
                    && let Some(mut result) = self
                        .lookup_probe
                        .as_ref()
                        .and_then(|probe| probe.reply(header.request_id, Instant::now()))
                {
                    result["file_error"] = json!(directory.file_error);
                    self.lookup_probe = None;
                    self.output.native("lookup_probe", result);
                    return Ok(());
                }
                if let Some(pb::rpc_response::Which::SendVideoTrackRsp(result)) = &response.which
                    && self.video_track_request.is_some()
                    && response.response_header.as_ref().map(|h| h.request_id)
                        == self.video_track_request
                {
                    self.output
                        .native("video_track_result", json!({"error":result.error_code}));
                }
                if let Some(pb::rpc_response::Which::ClipResponse(clip)) = &response.which {
                    crate::perf::trace(
                        "clipboard_response",
                        super::clipboard_wire::response_summary(clip),
                    );
                    let id = response
                        .response_header
                        .as_ref()
                        .map_or(0, |h| h.request_id);
                    if let Some(offer) = &self.file_offer {
                        offer.response(id, clip.clone());
                    }
                    for message in self.drag_files.response(id, clip.clone(), &self.output) {
                        self.send_file(message).await?;
                    }
                    self.receive_clipboard_files();
                    match self.clipboard_wire.response(clip.clone()) {
                        Ok(text) => self.clipboard_text(text),
                        Err(e) => self
                            .output
                            .native("clipboard_error", json!({"message":e.to_string()})),
                    }
                    self.clipboard_image();
                    for message in self.clipboard_wire.take_outgoing() {
                        self.send_file(message).await?;
                    }
                    if self.clipboard_offer == Some(id)
                        && matches!(
                            clip.which,
                            Some(pb::clipboard_response::Which::FormatListResponse(_))
                        )
                    {
                        self.clipboard_offer = None;
                        if self.clipboard_paste_offer.take() == Some(id)
                            && self.clipboard_enabled
                            && !self.read_only
                        {
                            self.paste_keys().await?;
                        }
                        self.output.native("clipboard_image_offered",json!({"error":1,"format":"image","request_id":id,"state":"offered","content_transfer":"on_remote_request"}));
                    }
                }
                if let Some(pb::rpc_response::Which::FileTransferFtpResponse(ftp)) = &response.which
                {
                    for message in self.file_response(
                        response
                            .response_header
                            .as_ref()
                            .map(|h| h.request_id)
                            .unwrap_or(0),
                        ftp.clone(),
                    ) {
                        self.send_file(message).await?;
                    }
                }
                if let Some(pb::rpc_response::Which::TextChangeResponse(text)) = &response.which {
                    self.output
                        .native("clipboard_sent", json!({"error":text.err}));
                }
                let rpc_id = response
                    .response_header
                    .as_ref()
                    .map_or(0, |h| h.request_id);
                match &response.which {
                    Some(pb::rpc_response::Which::VirtualAudioDriverPolicyRsp(r)) => {
                        if let Some((request, enabled, _)) = self.microphone_pending
                            && request == rpc_id
                        {
                            self.microphone_pending = None;
                            if let Some(microphone) = &self.microphone {
                                microphone
                                    .enabled
                                    .store(enabled && r.error_code == 0, Ordering::Relaxed);
                            }
                            self.output.native("microphone_policy", json!({"request_id":rpc_id,"enabled":enabled,"accepted":r.error_code==0,"error_code":r.error_code}));
                            if r.error_code == 0 {
                                self.send_protobuf(wire::action(if enabled { 23 } else { 24 }))
                                    .await?;
                            }
                        }
                    }
                    Some(pb::rpc_response::Which::CreateVirtualDisplayRsp(r)) => self
                        .virtual_displays
                        .response(rpc_id, r.error_code, &self.output),
                    Some(pb::rpc_response::Which::RemoveVirtualDisplayRsp(r)) => self
                        .virtual_displays
                        .response(rpc_id, r.error_code, &self.output),
                    Some(pb::rpc_response::Which::EnterSuperScreenRep(r)) => self
                        .super_screen
                        .response(rpc_id, true, r.error_code, &self.output),
                    Some(pb::rpc_response::Which::QuitSuperScreen(r)) => self
                        .super_screen
                        .response(rpc_id, false, r.error_code, &self.output),
                    _ => {}
                }
                if let Some(pb::rpc_response::Which::CaptureSetting(setting)) = response.which {
                    let errors: Vec<Value> = setting
                        .errors
                        .into_iter()
                        .map(|e| json!({"code":e.error_code,"message":e.error_message,"detail":e.error_detail}))
                        .collect();
                    self.output.native(
                        "capture_result",
                        json!({
                            "request_id":response.response_header.as_ref().map(|h|h.request_id),
                            "errors":errors
                        }),
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
    async fn paste_keys(&mut self) -> Result<()> {
        self.release_inputs().await;
        for (action, key) in [
            ("kbd_press", 17),
            ("kbd_press", 86),
            ("kbd_release", 86),
            ("kbd_release", 17),
        ] {
            self.send_control(json!({"action":action,"key":key}).to_string().into_bytes())
                .await?;
        }
        Ok(())
    }
    async fn bootstrap_headless(&mut self) -> Result<()> {
        if let Some(packet) = self.virtual_displays.headless_bootstrap(
            self.video_visible && self.metrics.decoded.load(Ordering::Relaxed) == 0,
            self.protocol_ready,
            &self.output,
        )? {
            self.send_protobuf(packet).await?;
        }
        Ok(())
    }
    fn cancel_clipboard_files(&mut self) -> Vec<Vec<u8>> {
        if let Some((id, _)) = self.clipboard_files_pending.take()
            && self.drag_files.active(id)
        {
            return self.drag_files.cancel(&self.output);
        }
        vec![]
    }
    fn begin_clipboard_files(&mut self) -> Result<Vec<Vec<u8>>> {
        const CAPACITY: u64 = 1024 * 1024 * 1024;
        ensure!(
            self.clipboard_cache_count < 32 && self.clipboard_cache_bytes < CAPACITY,
            "本次连接的剪贴板缓存已满，请使用文件窗口传输"
        );
        let storage = Arc::new(
            tempfile::Builder::new()
                .prefix("uux-clipboard-")
                .tempdir()?,
        );
        let id = 0x5000_0000 | ((uuid::Uuid::new_v4().as_u128() as u32) & 0x0fff_ffff);
        let packets = self.drag_files.begin_limited(
            storage.path(),
            Some(id),
            false,
            Some(CAPACITY - self.clipboard_cache_bytes),
            &self.output,
        )?;
        self.clipboard_files_pending = Some((id, storage));
        Ok(packets)
    }
    fn receive_clipboard_files(&mut self) {
        if let Some((id, paths, bytes)) = self.drag_files.take_completed()
            && self
                .clipboard_files_pending
                .as_ref()
                .is_some_and(|p| p.0 == id)
            && let Some((_, storage)) = self.clipboard_files_pending.take()
        {
            self.clipboard_cache_count += 1;
            self.clipboard_cache_bytes += bytes;
            if self.clipboard_enabled
                && let Some(clipboard) = &self.clipboard
            {
                clipboard.remote_files(paths, storage);
            }
        }
        if self
            .clipboard_files_pending
            .as_ref()
            .is_some_and(|p| !self.drag_files.active(p.0))
        {
            self.clipboard_files_pending = None;
        }
    }
    fn clipboard_text(&self, text: Option<String>) {
        if let Some(text) = text {
            if self.clipboard_observe {
                self.output.native("clipboard_value",json!({"text":text,"format":"text","source":"remote","observed_at_ms":crate::service::now_ms()}));
            }
            if let Some(clipboard) = &self.clipboard {
                self.output
                    .native("clipboard_received", json!({"bytes":text.len()}));
                clipboard.remote(text);
            }
        }
    }
    fn clipboard_image(&mut self) {
        if let Some(png) = self.clipboard_wire.image.take() {
            if self.clipboard_observe {
                self.output.native("clipboard_value",json!({"format":"image","mime_type":"image/png","data_base64":base64::engine::general_purpose::STANDARD.encode(&png),"source":"remote","observed_at_ms":crate::service::now_ms()}));
            }
            if let Some(clipboard) = &self.clipboard {
                clipboard.remote_image(png);
            }
        }
        if let Some(error) = self.clipboard_wire.error.take() {
            self.output
                .native("clipboard_error", json!({"message":error}));
        }
    }
    fn file_request(&mut self, id: i64, ftp: pb::FileTransferFtpRequest) -> Vec<Vec<u8>> {
        match self.files.request(id, ftp, &self.output) {
            Ok(mut v) => {
                v.extend(self.files.tick(&self.output));
                v
            }
            Err(e) => {
                self.output.event(Event::Error {
                    scope: "files".into(),
                    message: e.to_string(),
                    generation: Some(self.generation),
                    login_id: None,
                });
                vec![]
            }
        }
    }
    fn file_response(&mut self, id: i64, ftp: pb::FileTransferFtpResponse) -> Vec<Vec<u8>> {
        match self.files.response(id, ftp, &self.output) {
            Ok(mut v) => {
                v.extend(self.files.tick(&self.output));
                v
            }
            Err(e) => {
                self.output.event(Event::Error {
                    scope: "files".into(),
                    message: e.to_string(),
                    generation: Some(self.generation),
                    login_id: None,
                });
                vec![]
            }
        }
    }
    async fn connected_loop(&mut self, commands: &mut mpsc::Receiver<Value>) -> Result<()> {
        let mut signals = self.signals.take().unwrap();
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        let mut recovery_timer = tokio::time::interval(Duration::from_millis(50));
        recovery_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        timer.tick().await; // Consume the immediate tick before starting the measurement window.
        let mut last_frames = self.metrics.decoded.load(Ordering::Relaxed);
        let mut last_samples = self.metrics.samples.load(Ordering::Relaxed);
        let mut last_bytes = self.metrics.rtp_bytes.load(Ordering::Relaxed);
        let mut measured_at = Instant::now();
        loop {
            tokio::select! {
                _=self.cancel.cancelled()=>return Ok(()),
                _=recovery_timer.tick(), if self.startup_recovery.needs_poll()
                    && (self.video_visible && self.protocol_ready || self.startup_recovery.in_flight())
                    =>self.recover_video_startup().await?,
                event=signals.recv()=>self.signal_event(event.context("信令结束")??).await?,
                data=self.incoming.recv()=>if let Some((label,data))=data {self.data(&label,&data).await?;},
                command=commands.recv()=>{
                    let Some(command)=command else {return Ok(());};
                    if command["op"] == "expect_video" {
                        self.video_visible=command["enabled"].as_bool().unwrap_or(true);
                        set_video_wait(&mut self.video_deadline,command["enabled"].as_bool().unwrap_or(true),
                            self.first_frame_announced,Instant::now());
                        self.begin_deferred_capture().await?;
                        self.bootstrap_headless().await?;
                        self.restore_screen_preference().await?;
                        continue;
                    }
                    let client_request=command.get("client_request").cloned();
                    let microphone_command=command["op"]=="microphone";
                    let result=self.command(command).await;
                    if let Some(request)=client_request {
                        self.output.native("command_result",json!({"client_request":request,"ok":result.is_ok(),"error":result.as_ref().err().map(|e|e.to_string())}));
                    }
                    if let Err(e)=result {
                        if microphone_command {self.output.native("microphone_policy",json!({"accepted":false,"error":e.to_string()}));}
                        self.output.event(Event::Error{scope:"operation".into(),message:e.to_string(),generation:Some(self.generation),login_id:None});
                    }
                },
                Some(packet)=self.ports.outgoing.recv()=>if !packet.cancel.is_cancelled() {
                    let id = packet.frame.rule_id;
                    if let Err(error) = self.send_file(wire::wrap(pb::message::Which::PortMappingFrame(packet.frame))).await {
                        for frame in self.ports.disable(id).await {
                            let _ = self.send_file(wire::wrap(pb::message::Which::PortMappingFrame(frame))).await;
                        }
                        self.output.native("port_status",json!({"id":id,"enabled":false,"status":"failed","message":error.to_string()}));
                    }
                },
                _=timer.tick()=>{
                    self.observe_first_frame();
                    ensure!(self.video_deadline.is_none_or(|deadline| Instant::now()<deadline),"等待远端画面超时");
                    if self.terminal_pending.as_ref().is_some_and(|p| p.1.elapsed() >= Duration::from_secs(3)) {
                        let (_,_,command) = self.terminal_pending.take().unwrap();
                        self.send_terminal(&command).await?;
                    }
                    if self.lookup_probe.as_ref().is_some_and(|probe| probe.expired(Instant::now())) {
                        let probe = self.lookup_probe.take().unwrap();
                        self.output.native("lookup_probe", probe.timeout());
                    }
                    for message in self.files.tick(&self.output){self.send_file(message).await?;}
                    self.clipboard_wire.tick();
                    self.clipboard_image();
                    for message in self.drag_files.tick(&self.output){self.send_file(message).await?;}
                    self.receive_clipboard_files();
                    self.super_screen.tick(&self.output);
                    self.virtual_displays.tick(&self.output);
                    self.begin_deferred_capture().await?;
                    let frames=self.metrics.decoded.load(Ordering::Relaxed);
                    let samples=self.metrics.samples.load(Ordering::Relaxed);
                    let bytes=self.metrics.rtp_bytes.load(Ordering::Relaxed);
                    let interval = measured_at.elapsed().as_secs_f64().max(0.001);
                    measured_at = Instant::now();
                    let buffered = if let Some(channel) = self.channels.get(CONTROL) { channel.buffered_amount().await } else { 0 };
                    let text_buffered = if let Some(channel) = self.channels.get(TEXT) { channel.buffered_amount().await } else { 0 };
                    let video_size = self.metrics.video_size.load(Ordering::Relaxed);
                    if let Some(audio)=&self.audio {self.output.native("audio_status",audio.status.lock().unwrap().clone());}
                    if let Some(microphone)=&self.microphone {self.output.native("microphone_status",microphone.status.lock().unwrap().clone());}
                    if self.microphone_pending.is_some_and(|(_,_,start)|start.elapsed()>Duration::from_secs(8)) {
                        let (request,enabled,_) = self.microphone_pending.take().unwrap();
                        self.output.native("microphone_policy",json!({"request_id":request,"enabled":enabled,"accepted":false,"error":"远端虚拟声卡策略确认超时"}));
                    }
                    self.output.native("decoder_status",self.output.media.decoder.lock().unwrap().clone());
                    self.output.native("stats",json!({
                        "generation":self.generation,"fps":((frames-last_frames) as f64/interval).round() as u64,
                        "source_fps":((samples-last_samples) as f64/interval).round() as u64,
                        "interval_s":interval,
                        "mbps":(bytes-last_bytes) as f64*8.0/1_000_000.0/interval,
                        "frames":frames,"packets":self.metrics.rtp_packets.load(Ordering::Relaxed),
                        "samples":self.metrics.samples.load(Ordering::Relaxed),
                        "video_width":video_size >> 32,"video_height":video_size & 0xffff_ffff,
                        "lost_frames":self.metrics.losses.load(Ordering::Relaxed),
                        "rtx_recovered":self.recovered.load(Ordering::Relaxed),
                        "decode_ms":self.metrics.decode_us.load(Ordering::Relaxed) as f64/1000.0,
                        "decode_errors":self.metrics.decode_errors.load(Ordering::Relaxed),
                        "queue_drops":self.metrics.queue_drops.load(Ordering::Relaxed),
                        "audio_packets":self.metrics.audio_packets.load(Ordering::Relaxed),
                        "audio_errors":self.metrics.audio_errors.load(Ordering::Relaxed),
                        "frame_age_ms":self.metrics.last_frame.lock().unwrap().map(|t|t.elapsed().as_millis()),
                        "max_gap_ms":self.metrics.max_gap_ms.load(Ordering::Relaxed),
                        "late_packets":self.metrics.late_packets.load(Ordering::Relaxed),
                        "nack_packets":self.metrics.nacks.load(Ordering::Relaxed),
                        "pli_requests":self.metrics.pli.load(Ordering::Relaxed),
                        "skipped_until_idr":self.metrics.skipped_until_idr.load(Ordering::Relaxed),
                        "decoder_queue":self.metrics.queue_time.take(),
                        "h264_decode":self.metrics.decode_time.take(),
                        "rgba_convert":self.metrics.convert_time.take(),
                        "channel_send":self.metrics.send_time.take(),
                        "frame_interval":self.metrics.frame_interval.take(),
                        "rtp_frame_interval":self.metrics.rtp_frame_interval.take(),
                        "control_buffered_bytes":buffered,
                        "text_buffered_bytes":text_buffered,
                        "rtcp_packets":self.metrics.rtcp_packets.load(Ordering::Relaxed),
                        "elapsed":self.started.elapsed().as_secs()
                    }));
                    last_frames=frames;last_bytes=bytes;last_samples=samples;
                    if let Some(pc)=&self.pc {ensure!(!matches!(pc.connection_state(),RTCPeerConnectionState::Failed|RTCPeerConnectionState::Closed),"远端连接已关闭");}
                },
            }
        }
    }
    async fn command(&mut self, command: Value) -> Result<()> {
        if let Some(binding) = command.get("geometry") {
            let geometry: crate::media_bus::Geometry = serde_json::from_value(binding.clone())?;
            if geometry != self.output.media.geometry() {
                self.release_inputs().await;
                bail!("屏幕或画面版本已变化，旧坐标已拒绝");
            }
        }
        let op = command["op"].as_str().unwrap_or("");
        if self.read_only
            && matches!(
                op,
                "down"
                    | "up"
                    | "move"
                    | "relative"
                    | "button_down"
                    | "button_up"
                    | "click"
                    | "scroll"
                    | "text"
                    | "touch"
                    | "paste_local"
            )
        {
            ensure!(
                command.get("client_request").is_none(),
                "当前会话为仅观看模式"
            );
            return Ok(());
        }
        let data = match op {
            "lookup_probe" => {
                ensure!(
                    std::env::var_os("UU_PERF_LOG").is_some(),
                    "传输测量需开启性能诊断"
                );
                ensure!(
                    self.files_allowed && self.local_files_allowed,
                    "当前会话未允许文件访问"
                );
                if let Some(probe) = &self.lookup_probe {
                    ensure!(probe.expired(Instant::now()), "上一条测量仍在等待确认");
                    self.output.native("lookup_probe", probe.timeout());
                }
                let id = command["id"].as_u64().context("测量编号无效")?;
                let (sequence, bytes) = wire::lookup_probe();
                self.lookup_probe = Some(crate::perf::RoundTripProbe::new(
                    sequence,
                    id,
                    Instant::now(),
                ));
                if let Err(error) = self.send_control(bytes).await {
                    self.lookup_probe = None;
                    return Err(error);
                }
                return Ok(());
            }
            "keep_awake" => {
                self.keep_awake(command["enabled"] == true).await;
                return Ok(());
            }
            "local_security" => {
                self.lock_on_disconnect = command["lock_on_disconnect"] == true;
                self.local_files_allowed = command["allow_files"] == true;
                self.read_only = command["read_only"] == true;
                if self.read_only {
                    self.release_inputs().await;
                }
                if !self.local_files_allowed {
                    self.file_offer = None;
                    self.files.close(&self.output);
                    self.files = Default::default();
                    for message in self.drag_files.cancel(&self.output) {
                        self.send_file(message).await?;
                    }
                }
                return Ok(());
            }
            "touch" => {
                ensure!(
                    self.touch_supported != Some(false),
                    "远端报告不支持触摸输入"
                );
                let event_type = match command["phase"].as_str() {
                    Some("start") => 1,
                    Some("move") => 2,
                    Some("end") => 3,
                    Some("cancel") => 4,
                    _ => bail!("触摸阶段无效"),
                };
                let points = command["points"].as_array().context("需要触摸点数组")?;
                ensure!(
                    !points.is_empty() && points.len() <= 10,
                    "一次触摸须为 1–10 个触点"
                );
                let mut ids = BTreeSet::new();
                let mut touch_points = Vec::new();
                for point in points {
                    let id = u32::try_from(point["id"].as_u64().context("触点编号无效")?)?;
                    ensure!(ids.insert(id), "触点编号重复");
                    let x = point["x"].as_f64().context("触点 x 无效")?;
                    let y = point["y"].as_f64().context("触点 y 无效")?;
                    let pressure = point["pressure"].as_f64().unwrap_or(0.5);
                    ensure!(
                        (0.0..=1.0).contains(&x)
                            && (0.0..=1.0).contains(&y)
                            && (0.0..=1.0).contains(&pressure),
                        "触点坐标或压力超出范围"
                    );
                    ensure!(
                        event_type == 4
                            || (event_type == 1 && !self.touches.contains(&id))
                            || (event_type != 1 && self.touches.contains(&id)),
                        "触点生命周期无效"
                    );
                    touch_points.push(pb::TouchEventPoint {
                        id,
                        x: x as f32,
                        y: y as f32,
                        pressure: pressure as f32,
                        ..Default::default()
                    });
                }
                self.send_protobuf(wire::wrap(pb::message::Which::InputEvent(pb::InputEvent {
                    which: Some(pb::input_event::Which::TouchEvent(pb::TouchEvent {
                        event_type,
                        touch_points,
                    })),
                })))
                .await?;
                for id in ids {
                    if event_type == 1 {
                        self.touches.insert(id);
                    } else if event_type >= 3 {
                        self.touches.remove(&id);
                    }
                }
                return Ok(());
            }
            "down" | "up" => {
                let key = command["key"]
                    .as_u64()
                    .filter(|k| (1..=255).contains(k))
                    .context("键码无效")? as u16;
                if op == "down" {
                    self.keys.insert(key);
                } else {
                    self.keys.remove(&key);
                }
                json!({"action":if op=="down" {"kbd_press"} else {"kbd_release"},"key":key})
            }
            "move" => {
                json!({"action":"mouse_move_absolute","abs_x":command["x"],"abs_y":command["y"]})
            }
            "relative" => {
                json!({"action":"mouse_move_relative","delta_x":command["dx"],"delta_y":command["dy"]})
            }
            "button_down" | "button_up" | "click" => {
                let button = command["button"].as_u64().unwrap_or(1) as u8;
                ensure!([1, 2, 16, 32, 64].contains(&button), "鼠标键无效");
                if op == "button_down" {
                    self.buttons.insert(button);
                } else {
                    self.buttons.remove(&button);
                }
                json!({"action":match op {"button_down"=>"mouse_press","button_up"=>"mouse_release",_=>"mouse_click"},"button":button})
            }
            "scroll" => {
                json!({"action":"mouse_scroll","delta_x":command["dx"],"delta_y":command["dy"]})
            }
            "text" => {
                let text = command["text"].as_str().context("文字无效")?;
                ensure!(text.len() <= 65536, "单次文字过长");
                json!({"action":"text_input","content":text})
            }
            "clipboard_get" => {
                if command["scope"] == "local" {
                    self.clipboard
                        .as_ref()
                        .context("本地剪贴板不可用")?
                        .local(command);
                    return Ok(());
                }
                if command["format"] == "files" {
                    if self.file_offer.as_ref().is_some_and(|f| f.finished()) {
                        self.file_offer = None;
                    }
                    ensure!(
                        self.files_allowed && self.local_files_allowed,
                        "当前会话未允许文件传输"
                    );
                    ensure!(
                        self.file_offer.is_none(),
                        "本机正在提供文件剪贴板，请先完成粘贴再读取远端文件"
                    );
                    let directory = std::path::PathBuf::from(
                        command["destination"]
                            .as_str()
                            .context("缺少接收目录 destination")?,
                    );
                    let id = command["queued_id"]
                        .as_i64()
                        .filter(|n| *n < 0 && *n > -(0x4000_0000i64))
                        .context("文件任务编号无效")?;
                    for packet in self.drag_files.begin(
                        &directory,
                        Some((-id) as u32),
                        false,
                        &self.output,
                    )? {
                        self.send_file(packet).await?;
                    }
                    return Ok(());
                }
                self.clipboard_observe = true;
                let request = if command["format"] == "image" {
                    self.clipboard_wire.validate_remote_image_read()?;
                    self.clipboard_wire.ask_image()
                } else {
                    self.clipboard_wire.ask_text()
                };
                return self.send_file(request).await;
            }
            "clipboard_local" => {
                for packet in self.cancel_clipboard_files() {
                    self.send_file(packet).await?;
                }
                self.clipboard
                    .as_ref()
                    .context("本地剪贴板不可用")?
                    .local(command);
                return Ok(());
            }
            "clipboard_files" => {
                ensure!(
                    self.files_allowed && self.local_files_allowed,
                    "当前会话未允许文件传输"
                );
                if command["drop"].is_object() {
                    ensure!(
                        !self.read_only && command["geometry"].is_object(),
                        "拖放需要有效画面和控制权限"
                    );
                    ensure!(
                        command["drop"]["screen_id"] == self.output.media.geometry().screen_id,
                        "拖放屏幕已失效"
                    );
                }
                for packet in self.cancel_clipboard_files() {
                    self.send_file(packet).await?;
                }
                self.file_offer = None;
                self.clipboard_wire.clear_offer();
                self.file_offer = Some(super::clipboard_files::FileOffer::start(
                    &command,
                    self.output.clone(),
                    self.incoming_tx.clone(),
                )?);
                return Ok(());
            }
            "clipboard_image" => {
                for packet in self.cancel_clipboard_files() {
                    self.send_file(packet).await?;
                }
                self.file_offer = None;
                let data = command["data_base64"]
                    .as_str()
                    .context("缺少 PNG data_base64")?;
                ensure!(data.len() <= 2 * 1024 * 1024 * 4 / 3 + 4, "PNG 超过 2 MiB");
                let (id, bytes) = self
                    .clipboard_wire
                    .offer_image(&base64::engine::general_purpose::STANDARD.decode(data)?)?;
                self.send_file(bytes).await?;
                self.clipboard_offer = Some(id);
                self.output.native(
                    "clipboard_image_submitted",
                    json!({"request_id":id,"client_request":command["client_request"]}),
                );
                return Ok(());
            }
            "clipboard" => {
                for packet in self.cancel_clipboard_files() {
                    self.send_file(packet).await?;
                }
                self.file_offer = None;
                let text = command["text"].as_str().context("剪贴板文本无效")?;
                ensure!(text.len() <= 65536, "单次同步的剪贴板文字超过 64 KiB");
                self.clipboard_wire.clear_offer();
                let (_, bytes) = wire::rpc(pb::rpc_request::Which::TextChangeRequest(
                    pb::ClipboardTextChangeRequest {
                        format_id: 13,
                        data: text.into(),
                    },
                ));
                return self.send_protobuf(bytes).await;
            }
            "read_dir" | "mkdir" | "rename" | "remove_file" | "remove_dir" | "upload"
            | "download" | "transfer_pause" | "transfer_cancel" | "folder_cancel"
            | "transfer_batch" | "transfers_pause" | "transfers_cancel" => {
                ensure!(
                    self.files_allowed && self.local_files_allowed,
                    "当前会话未允许文件传输"
                );
                if let Some(offer) = &self.file_offer {
                    offer.command(&command)?;
                }
                for message in self.drag_files.command(&command, &self.output)? {
                    self.send_file(message).await?;
                }
                if command["id"].as_i64().is_some_and(|id| id < 0) {
                    return Ok(());
                }
                for message in self.files.command(&command, &self.output)? {
                    self.send_file(message).await?;
                }
                return Ok(());
            }
            "release" => {
                if let Some(offer) = &mut self.file_offer
                    && offer.input_pending
                {
                    offer.reject("输入控制权已释放，请重新拖放或粘贴");
                    offer.input_pending = false;
                }
                self.clipboard_paste_offer = None;
                self.release_inputs().await;
                return Ok(());
            }
            "clipboard_enabled" => {
                self.clipboard_enabled = command["enabled"].as_bool().unwrap_or(true);
                if !self.clipboard_enabled {
                    for packet in self.cancel_clipboard_files() {
                        self.send_file(packet).await?;
                    }
                }
                if let Some(clipboard) = &self.clipboard {
                    clipboard.enable(self.clipboard_enabled);
                }
                return Ok(());
            }
            "drag_receive" => {
                self.drag_receive = command["enabled"] == true;
                if let Some(path) = command["directory"].as_str() {
                    self.download_dir = path.into();
                }
                self.announce_download_path().await?;
                if !self.drag_receive {
                    for message in self.drag_files.cancel(&self.output) {
                        self.send_file(message).await?;
                    }
                }
                return Ok(());
            }
            "paste_local" => {
                if let Some(clipboard) = &self.clipboard {
                    clipboard.paste();
                }
                return Ok(());
            }
            "audio" => {
                let enabled = command["enabled"].as_bool().unwrap_or(true);
                if let Some(audio) = &self.audio {
                    if let Some(device) = command.get("device_id") {
                        *audio.device.lock().unwrap() = if device.is_null() || device == "default" {
                            None
                        } else {
                            Some(device.as_str().context("device_id 无效")?.to_owned())
                        };
                    }
                    audio.enabled.store(enabled, Ordering::Relaxed);
                }
                self.send_protobuf(wire::action(if enabled || self.audio_subscription {
                    25
                } else {
                    26
                }))
                .await?;
                return Ok(());
            }
            "microphone" => {
                let enabled = command["enabled"].as_bool().context("缺少 enabled")?;
                ensure!(
                    !enabled
                        || self
                            .microphone_transceiver
                            .as_ref()
                            .is_some_and(|t| matches!(
                                t.current_direction(),
                                RTCRtpTransceiverDirection::Sendonly
                                    | RTCRtpTransceiverDirection::Sendrecv
                            )),
                    "远端未协商接收麦克风的音频轨道"
                );
                ensure!(
                    self.microphone_pending.is_none() || !enabled,
                    "正在等待远端麦克风确认"
                );
                let microphone = self.microphone.as_ref().context("此会话未协商麦克风轨道")?;
                microphone.enabled.store(false, Ordering::Relaxed);
                if let Some(device) = command.get("device_id") {
                    *microphone.device.lock().unwrap() = if device.is_null() || device == "default"
                    {
                        None
                    } else {
                        Some(device.as_str().context("device_id 无效")?.to_owned())
                    };
                }
                // Official ControlPresenter sends policy=1 on open, 0 on close.
                let (request, bytes) =
                    wire::rpc(pb::rpc_request::Which::VirtualAudioDriverPolicyReq(
                        pb::VirtualAudioDriverPolicyRequest {
                            policy: i32::from(enabled),
                        },
                    ));
                self.send_protobuf(bytes).await?;
                self.microphone_pending = Some((request, enabled, Instant::now()));
                self.output.native("microphone_submitted",json!({"request_id":request,"enabled":enabled,"client_request":command["client_request"]}));
                return Ok(());
            }
            "audio_receive" => {
                self.audio_subscription = command["enabled"].as_bool().unwrap_or(false);
                let playback = self
                    .audio
                    .as_ref()
                    .is_some_and(|a| a.enabled.load(Ordering::Relaxed));
                self.send_protobuf(wire::action(if playback || self.audio_subscription {
                    25
                } else {
                    26
                }))
                .await?;
                return Ok(());
            }
            "virtual_display_create" | "virtual_display_remove" => {
                self.release_inputs().await;
                let message = self.virtual_displays.command(&command, &self.output)?;
                if let Err(error) = self.send_protobuf(message).await {
                    self.virtual_displays
                        .failed(&error.to_string(), &self.output);
                    return Err(error);
                }
                return Ok(());
            }
            "super_screen" => {
                self.finish_startup_recovery().await?;
                self.screen_preference = None;
                self.release_inputs().await;
                if let Some(bytes) = self.super_screen.command(&command, &self.output)? {
                    self.send_protobuf(bytes).await?;
                }
                return Ok(());
            }
            "decoder" => {
                let preference = command["preference"].as_str().context("缺少本地解码偏好")?;
                ensure!(
                    super::decoder::preferences().contains(&preference),
                    "本地解码偏好无效"
                );
                *self.output.media.decoder_preference.lock().unwrap() = preference.into();
                self.output
                    .media
                    .request_keyframe
                    .store(true, Ordering::Relaxed);
                return Ok(());
            }
            "capture" => {
                crate::capabilities::validate_capture(&self.remote_streamer, &command)?;
                let mut merged = self
                    .capture_requested
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                merged.extend(command.as_object().context("显示参数无效")?.clone());
                let command = Value::Object(merged);
                self.finish_startup_recovery().await?;
                let fps = command["fps"].as_i64().unwrap_or(60) as i32;
                ensure!([30, 60, 90, 144].contains(&fps), "帧率无效");
                let quality = command["quality"].as_i64().unwrap_or(2);
                ensure!((1..=6).contains(&quality), "画质无效");
                let width = command["width"].as_i64().unwrap_or(0);
                let height = command["height"].as_i64().unwrap_or(0);
                ensure!(
                    (width == 0 && height == 0)
                        || ((320..=3840).contains(&width) && (200..=2160).contains(&height)),
                    "分辨率超出支持范围"
                );
                let codec = match command["codec"].as_str().unwrap_or("h264") {
                    "h264" => 1,
                    "h265" => 2,
                    _ => bail!("编码格式无效"),
                };
                let chroma = if command["true_color"] == true { 3 } else { 1 };
                let dpi = command["dpi"].as_i64().unwrap_or(0);
                ensure!(dpi == 0 || (50..=500).contains(&dpi), "DPI 比例无效");
                let bitrate = command["bitrate"].as_i64().unwrap_or(0);
                ensure!((0..=40_000_000).contains(&bitrate), "码率无效");
                // Diagnostic comparison: the legacy resolution-only RPC leaves
                // codec, FPS and quality untouched. UI keeps the full request
                // until field tests establish compatibility with this host.
                let resolution_only = command["resolution_only"] == true;
                ensure!(
                    !resolution_only || (width > 0 && height > 0),
                    "单独分辨率请求需要明确尺寸"
                );
                let which = if resolution_only {
                    pb::rpc_request::Which::ResolutionSetting(pb::ResolutionSettingRequest {
                        resolution_width: width as i32,
                        resolution_height: height as i32,
                    })
                } else {
                    pb::rpc_request::Which::CaptureSetting(pb::CaptureSettingRequest {
                        fps: wire::fps_enum(fps),
                        fps_count: fps,
                        frame_quality: quality as i32,
                        cursor_capture: true,
                        screen_id: command["screen_id"].as_i64().unwrap_or(0) as i32,
                        chorma_format: chroma,
                        dpi_scale: dpi as i32,
                        enable_hdr: command["hdr"] == true,
                        resolution_type: if width > 0 { 4 } else { 3 },
                        resolution_width: width as i32,
                        resolution_height: height as i32,
                        codec_type: codec,
                        max_custom_bitrate: bitrate as i32,
                        ..Default::default()
                    })
                };
                let (request_id, bytes) = wire::rpc(which);
                crate::perf::trace(
                    "capture_request",
                    json!({
                        "request_id":request_id,"fps":fps,"quality":quality,"width":width,"height":height,
                        "bitrate":bitrate,"resolution_type":if width > 0 {4} else {3},"resolution_only":resolution_only,
                        "channel":self.protobuf_channel()
                    }),
                );
                let started = Instant::now();
                let result = self.send_protobuf(bytes).await;
                if result.is_ok() {
                    self.capture_requested = command.clone();
                    self.output.native("display_requested", command.clone());
                    self.output.native(
                        "capture_submitted",
                        json!({
                            "request_id":request_id,"ui_request":command["ui_request"]
                        }),
                    );
                }
                crate::perf::trace(
                    "capture_submitted",
                    json!({
                        "request_id":request_id,"submitted":result.is_ok(),
                        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0
                    }),
                );
                return result;
            }
            "action" => {
                let action = command["action"].as_i64().context("操作无效")? as i32;
                ensure!(
                    [2, 3, 7, 8, 9, 10, 12, 13].contains(&action)
                        || (action == 4 && std::env::var_os("UU_PERF_LOG").is_some()),
                    "操作不支持"
                );
                if matches!(action, 7 | 8) {
                    self.startup_recovery.disable();
                }
                let result = self.send_protobuf(wire::action(action)).await;
                if let Some(request_id) = command["request_id"].as_str() {
                    self.output
                        .native("action_sent", json!({"request_id":request_id,"ok":result.is_ok(),"message":result.as_ref().err().map(|e|e.to_string())}));
                }
                return result;
            }
            "terminal" => {
                ensure!(
                    !self.read_only || command["action"] == "close",
                    "当前为仅查看模式"
                );
                if command["action"] == "open" {
                    ensure!(self.terminal_pending.is_none(), "正在打开终端");
                    let (id, bytes) = wire::rpc(pb::rpc_request::Which::TerminalCheckEnvReq(
                        pb::CheckEnvReq {},
                    ));
                    self.send_channel("BINARY_DATA_CHANNEL", false, bytes)
                        .await?;
                    self.terminal_pending = Some((id, Instant::now(), command));
                } else if command["action"] == "close" {
                    self.terminal_pending = None;
                    if let Some(bytes) = self.terminal.close() {
                        self.send_channel("BINARY_DATA_CHANNEL", false, bytes)
                            .await?;
                    }
                } else if command["action"] == "resize"
                    && let Some((_, _, pending)) = &mut self.terminal_pending
                {
                    pending["cols"] = command["cols"].clone();
                    pending["rows"] = command["rows"].clone();
                } else if self.terminal_pending.is_none() {
                    self.send_terminal(&command).await?;
                }
                return Ok(());
            }
            "port_mapping" => {
                let id = command["id"].as_u64().context("映射编号无效")?;
                let enabled = command["enabled"] == true;
                let result: Result<()> = async {
                    for frame in self.ports.disable(id).await {
                        self.send_file(wire::wrap(pb::message::Which::PortMappingFrame(frame)))
                            .await?;
                    }
                    if enabled {
                        ensure!(!self.read_only, "当前为仅查看模式");
                        let config: super::ports::RuleConfig =
                            serde_json::from_value(command["rule"].clone())?;
                        ensure!(config.id == id, "映射编号不一致");
                        self.ports.enable(config, self.output.clone()).await?;
                    }
                    Ok(())
                }
                .await;
                self.output.native("port_status",json!({"id":id,"scope":"listener","listening":enabled && result.is_ok(),"last_target_state":"not_attempted","enabled":enabled && result.is_ok(),"status":if result.is_err(){"failed"}else if enabled{"listening"}else{"disabled"},
                    "message":result.err().map_or(if enabled{"已启用"}else{"未启用"}.into(),|e|e.to_string())}));
                return Ok(());
            }
            _ => return Ok(()),
        };
        self.send_control(serde_json::to_vec(&data)?).await
    }
    async fn send_terminal(&mut self, command: &Value) -> Result<()> {
        let result: Result<()> = async {
            let bytes = self.terminal.command(command)?;
            self.send_channel("BINARY_DATA_CHANNEL", false, bytes).await
        }
        .await;
        if let Err(error) = result {
            self.terminal.close();
            self.output.native(
                "terminal_status",
                json!({"kind":8,"result":{"message":error.to_string()}}),
            );
        }
        Ok(())
    }
    async fn release_inputs(&mut self) {
        if !self.touches.is_empty() {
            let touch_points = std::mem::take(&mut self.touches)
                .into_iter()
                .map(|id| pb::TouchEventPoint {
                    id,
                    ..Default::default()
                })
                .collect();
            let _ = self
                .send_protobuf(wire::wrap(pb::message::Which::InputEvent(pb::InputEvent {
                    which: Some(pb::input_event::Which::TouchEvent(pb::TouchEvent {
                        event_type: 4,
                        touch_points,
                    })),
                })))
                .await;
        }
        for key in std::mem::take(&mut self.keys) {
            let _ = self
                .send_control(
                    json!({"action":"kbd_release","key":key})
                        .to_string()
                        .into_bytes(),
                )
                .await;
        }
        for button in std::mem::take(&mut self.buttons) {
            let _ = self
                .send_control(
                    json!({"action":"mouse_release","button":button})
                        .to_string()
                        .into_bytes(),
                )
                .await;
        }
    }
    async fn keep_awake(&mut self, enabled: bool) {
        if enabled && self.inhibitor.is_none() {
            match crate::platform::Inhibitor::acquire().await {
                Ok(inhibitor) => self.inhibitor = Some(inhibitor),
                Err(e) => self
                    .output
                    .native("settings_notice", json!({"message":e.to_string()})),
            }
        } else if !enabled && let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.release().await;
        }
    }
    async fn close(&mut self) {
        self.file_offer = None;
        let started = Instant::now();
        let drag_cancel = self.drag_files.cancel(&self.output);
        if let Some(microphone) = &self.microphone {
            microphone.enabled.store(false, Ordering::Relaxed);
        }
        self.phase("releasing");
        self.files.close(&self.output);
        // Never let a blocked channel prevent the signaling release.
        let super_exit = self.super_screen.close_request();
        let terminal_close = self.terminal.close();
        let port_close = self.ports.close().await;
        let _ = tokio::time::timeout(Duration::from_millis(750), async {
            self.release_inputs().await;
            if self.microphone.is_some() {
                let (_, bytes) = wire::rpc(pb::rpc_request::Which::VirtualAudioDriverPolicyReq(
                    pb::VirtualAudioDriverPolicyRequest { policy: 0 },
                ));
                let _ = self.send_protobuf(bytes).await;
            }
            for frame in port_close {
                let _ = self
                    .send_file(wire::wrap(pb::message::Which::PortMappingFrame(frame)))
                    .await;
            }
            if let Some(bytes) = terminal_close {
                let _ = self.send_channel("BINARY_DATA_CHANNEL", false, bytes).await;
            }
            if let Some(message) = super_exit {
                let _ = self.send_protobuf(message).await;
            }
            for message in drag_cancel {
                let _ = self.send_file(message).await;
            }
            if self.lock_on_disconnect {
                let _ = self.send_protobuf(wire::action(10)).await;
            }
            let _ = self.send_protobuf(wire::action(21)).await;
            for label in [CONTROL, TEXT, "BINARY_DATA_CHANNEL"] {
                if let Some(channel) = self.channels.get(label) {
                    while channel.buffered_amount().await > 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        })
        .await;
        let released = if let Some(signal) = &self.signal {
            signal
                .close(if self.ice.is_empty() {
                    None
                } else {
                    Some(&self.ice)
                })
                .await
        } else {
            true
        };
        self.cancel.cancel();
        if let Some(pc) = &self.pc {
            let _ = tokio::time::timeout(Duration::from_secs(2), pc.close()).await;
        }
        self.channels.clear();
        self.audio = None;
        self.microphone = None;
        self.microphone_transceiver = None;
        self.microphone_pending = None;
        self.clipboard = None;
        if let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.release().await;
        }
        self.output.native("released",json!({"generation":self.generation,"release_sent":released,
            "signal_closed":self.signal.as_ref().is_none_or(|s|s.is_closed()),"elapsed_ms":started.elapsed().as_millis()}));
    }
}
