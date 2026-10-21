use crate::backend::VideoFrame;
use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg_next as ffmpeg;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use webrtc::rtp::{codecs::h264::H264Packet, packet::Packet, packetizer::Depacketizer};

/// Desktop capture may stop sending while the screen is still. Waiting for
/// the *next* timestamp before displaying a complete frame adds arbitrary
/// latency. Complete contiguous access units are emitted on their RTP marker.
pub struct AccessUnit {
    pub data: Bytes,
    pub losses: u64,
    pub assembled_at: Instant,
    pub rtp_timestamp: u32,
    pub rtp_ssrc: u32,
}

/// H.264 uses a 90 kHz media clock. Compare only ordered, complete access
/// units; arrival/decoding times are measured separately. Ignore duplicate
/// timestamps and backward discontinuities, while accepting u32 wraparound.
pub fn rtp_frame_interval(previous: u32, current: u32) -> Option<Duration> {
    let ticks = current.wrapping_sub(previous);
    (ticks > 0 && ticks < (1 << 31)).then(|| Duration::from_secs_f64(ticks as f64 / 90_000.0))
}

pub struct H264Assembler {
    next: Option<u16>,
    pending: std::collections::HashMap<u16, Packet>,
    timestamp: Option<u32>,
    bytes: Vec<u8>,
    fragment_size: usize,
    depacketizer: H264Packet,
    progressed: Instant,
    last_packet_at: Option<Instant>,
    last_fresh_packet: Option<(Instant, u64)>,
    pub losses: u64,
    pub late_packets: u64,
}
impl Default for H264Assembler {
    fn default() -> Self {
        Self {
            next: None,
            pending: Default::default(),
            timestamp: None,
            bytes: vec![],
            fragment_size: 0,
            depacketizer: Default::default(),
            progressed: Instant::now(),
            last_packet_at: None,
            last_fresh_packet: None,
            losses: 0,
            late_packets: 0,
        }
    }
}
impl H264Assembler {
    pub fn push(&mut self, packet: Packet) -> Vec<AccessUnit> {
        let sequence = packet.header.sequence_number;
        if let Some(previous) = self.last_packet_at.replace(Instant::now())
            && previous.elapsed() >= Duration::from_millis(1500)
        {
            crate::perf::trace(
                "video_receive_gap",
                serde_json::json!({
                    "gap_ms":previous.elapsed().as_secs_f64()*1000.0,
                    "continues_sequence":self.next == Some(sequence),
                    "pending_packets":self.pending.len(),
                    "partial_frame":!self.bytes.is_empty() || self.fragment_size > 0
                }),
            );
        }
        let next = *self.next.get_or_insert(sequence);
        if sequence.wrapping_sub(next) >= 32768 {
            self.late_packets += 1;
            return vec![];
        }
        if let Some(gap) = self.fresh_packet_gap(&packet, Instant::now()) {
            crate::perf::trace("video_fresh_packet_gap", gap);
        }
        self.pending.insert(sequence, packet);
        self.drain()
    }
    /// RTX padding/replayed packets may keep arriving while capture is stalled.
    /// Measure new nonempty sequence data independently of raw packet activity.
    /// This is arrival progress, not proof of a complete or decodable frame.
    fn fresh_packet_gap(&mut self, packet: &Packet, now: Instant) -> Option<serde_json::Value> {
        let sequence = packet.header.sequence_number;
        if packet.payload.is_empty()
            || self.pending.contains_key(&sequence)
            || self
                .next
                .is_some_and(|next| sequence.wrapping_sub(next) >= 32768)
        {
            return None;
        }
        let (previous, late_before) = self.last_fresh_packet.replace((now, self.late_packets))?;
        let gap = now.saturating_duration_since(previous);
        (gap >= Duration::from_millis(1500)).then(|| {
            serde_json::json!({
                "gap_ms":gap.as_secs_f64()*1000.0,
                "late_packets_during_gap":self.late_packets.saturating_sub(late_before),
                "continues_sequence":self.next == Some(sequence),
                "pending_packets":self.pending.len(),
                "partial_frame":!self.bytes.is_empty() || self.fragment_size > 0
            })
        })
    }
    pub fn missing(&self) -> Vec<u16> {
        let Some(next) = self.next else { return vec![] };
        if self.progressed.elapsed().as_millis() < 15 {
            return vec![];
        }
        let distance = self
            .pending
            .keys()
            .map(|seq| seq.wrapping_sub(next))
            .max()
            .unwrap_or(0)
            .min(128);
        (0..distance)
            .map(|n| next.wrapping_add(n))
            .filter(|seq| !self.pending.contains_key(seq))
            .collect()
    }
    pub fn expire(&mut self) -> Vec<AccessUnit> {
        if !self.pending.is_empty()
            && (self.progressed.elapsed().as_millis() > 220 || self.pending.len() > 512)
        {
            let next = self.next.unwrap();
            self.next = self
                .pending
                .keys()
                .min_by_key(|seq| seq.wrapping_sub(next))
                .copied();
            self.bytes.clear();
            self.fragment_size = 0;
            self.timestamp = None;
            self.depacketizer = Default::default();
            self.losses += 1;
            return self.drain();
        }
        vec![]
    }
    fn drain(&mut self) -> Vec<AccessUnit> {
        let mut frames = vec![];
        while let Some(packet) = self.next.and_then(|n| self.pending.remove(&n)) {
            self.next = Some(packet.header.sequence_number.wrapping_add(1));
            self.progressed = Instant::now();
            if packet.payload.is_empty() {
                continue;
            }
            let timestamp = packet.header.timestamp;
            if self.timestamp != Some(timestamp) {
                if !self.bytes.is_empty() || self.fragment_size > 0 {
                    self.losses += 1;
                }
                self.bytes.clear();
                self.fragment_size = 0;
                self.depacketizer = Default::default();
                if !self.depacketizer.is_partition_head(&packet.payload) {
                    self.timestamp = None;
                    continue;
                }
                self.timestamp = Some(timestamp);
            }
            if !self.valid_payload(&packet.payload) {
                self.bytes.clear();
                self.fragment_size = 0;
                self.timestamp = None;
                self.depacketizer = Default::default();
                self.losses += 1;
                continue;
            }
            match self.depacketizer.depacketize(&packet.payload) {
                Ok(data) => self.bytes.extend_from_slice(&data),
                Err(_) => {
                    self.bytes.clear();
                    self.timestamp = None;
                    self.losses += 1;
                    continue;
                }
            }
            if self.bytes.len() > 16 * 1024 * 1024 {
                self.bytes.clear();
                self.timestamp = None;
                self.losses += 1;
                continue;
            }
            if packet.header.marker && self.fragment_size > 0 {
                self.bytes.clear();
                self.fragment_size = 0;
                self.timestamp = None;
                self.depacketizer = Default::default();
                self.losses += 1;
                continue;
            }
            if packet.header.marker && !self.bytes.is_empty() {
                frames.push(AccessUnit {
                    data: Bytes::from(std::mem::take(&mut self.bytes)),
                    losses: self.losses,
                    assembled_at: Instant::now(),
                    rtp_timestamp: timestamp,
                    rtp_ssrc: packet.header.ssrc,
                });
                self.timestamp = None;
            }
        }
        frames
    }
    fn valid_payload(&mut self, data: &[u8]) -> bool {
        if data.is_empty() {
            return false;
        }
        match data[0] & 31 {
            24 => {
                let mut offset = 1;
                while offset < data.len() {
                    if offset + 2 > data.len() {
                        return false;
                    }
                    let length = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
                    offset += 2;
                    if length == 0 || offset + length > data.len() {
                        return false;
                    }
                    offset += length;
                }
                true
            }
            28 => {
                if data.len() < 3 {
                    return false;
                }
                if data[1] & 0x80 != 0 {
                    self.fragment_size = 1;
                }
                if self.fragment_size == 0 {
                    return false;
                }
                self.fragment_size += data.len() - 2;
                let valid = self.fragment_size <= 16 * 1024 * 1024;
                if data[1] & 0x40 != 0 {
                    self.fragment_size = 0;
                }
                valid
            }
            _ => true,
        }
    }
}

#[derive(Default)]
pub struct Metrics {
    pub rtp_packets: AtomicU64,
    pub rtcp_packets: AtomicU64,
    pub rtp_bytes: AtomicU64,
    pub samples: AtomicU64,
    pub losses: AtomicU64,
    pub decoded: AtomicU64,
    /// Last decoded dimensions, packed together so a stats sample cannot mix sizes.
    pub video_size: AtomicU64,
    pub decode_errors: AtomicU64,
    pub queue_drops: AtomicU64,
    pub audio_packets: AtomicU64,
    pub audio_errors: AtomicU64,
    pub decode_us: AtomicU64,
    pub max_gap_ms: AtomicU64,
    pub last_frame: Mutex<Option<Instant>>,
    pub first_frame: Mutex<Option<Instant>>,
    pub queue_time: crate::perf::Timing,
    pub decode_time: crate::perf::Timing,
    pub convert_time: crate::perf::Timing,
    pub send_time: crate::perf::Timing,
    pub frame_interval: crate::perf::Timing,
    pub rtp_frame_interval: crate::perf::Timing,
    pub late_packets: AtomicU64,
    pub nacks: AtomicU64,
    pub pli: AtomicU64,
    pub skipped_until_idr: AtomicU64,
}

pub struct EncodedFrame {
    pub unit: AccessUnit,
    pub queue_loss: u64,
}

pub fn video_decoder(
    generation: u64,
    latest: Arc<Mutex<Option<VideoFrame>>>,
    metrics: Arc<Metrics>,
    ctx: eframe::egui::Context,
) -> mpsc::SyncSender<EncodedFrame> {
    // Retransmission can deliver several completed frames in a single burst.
    // Keep their reference chain intact; the renderer still consumes only the
    // latest decoded frame. An eight-frame queue caused needless IDR recovery.
    let (tx, rx) = mpsc::sync_channel::<EncodedFrame>(32);
    std::thread::spawn(move || {
        let result: Result<()> = (|| {
            ffmpeg::init()?;
            let codec =
                ffmpeg::decoder::find(ffmpeg::codec::Id::H264).context("FFmpeg 缺少 H264")?;
            let mut context = ffmpeg::codec::Context::new_with_codec(codec);
            context.set_threading(ffmpeg::codec::threading::Config {
                kind: ffmpeg::codec::threading::Type::Slice,
                count: 2,
            });
            context.set_flags(ffmpeg::codec::Flags::LOW_DELAY);
            let mut decoder = context.decoder().video()?;
            let mut converter = super::pixels::RgbaConverter::default();
            let mut stream_properties = None;
            let mut epoch = (0, 0);
            let mut waiting_keyframe = true;
            let mut sps = vec![];
            let mut pps = vec![];
            while let Ok(frame) = rx.recv() {
                metrics.queue_time.record(frame.unit.assembled_at.elapsed());
                let data = frame.unit.data;
                let started = Instant::now();
                // Loss belongs to a precise point in the queued reference chain.
                // A global counter may already include damage to *future* frames:
                // flushing early can then let corrupt post-loss frames decode unchecked.
                let current_epoch = (frame.unit.losses, frame.queue_loss);
                if epoch != current_epoch {
                    decoder.flush();
                    waiting_keyframe = true;
                    epoch = current_epoch;
                }
                let starts = data
                    .windows(4)
                    .enumerate()
                    .filter_map(|(i, w)| (w == [0, 0, 0, 1]).then_some(i))
                    .collect::<Vec<_>>();
                for (i, start) in starts.iter().enumerate() {
                    if *start + 4 >= data.len() {
                        continue;
                    }
                    let end = starts.get(i + 1).copied().unwrap_or(data.len());
                    match data[*start + 4] & 31 {
                        7 => {
                            if sps != data[*start..end] {
                                if let Some(parameters) =
                                    crate::perf::h264_sps(&data[*start + 4..end])
                                {
                                    crate::perf::trace("h264_sps", parameters);
                                }
                                sps = data[*start..end].to_vec();
                            }
                        }
                        8 => pps = data[*start..end].to_vec(),
                        _ => {}
                    }
                }
                let mut prefixed = None;
                if waiting_keyframe {
                    if sps.is_empty()
                        || pps.is_empty()
                        || !data
                            .windows(5)
                            .any(|w| w[..4] == [0, 0, 0, 1] && w[4] & 31 == 5)
                    {
                        metrics.skipped_until_idr.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let mut packet = sps.clone();
                    packet.extend_from_slice(&pps);
                    packet.extend_from_slice(&data);
                    prefixed = Some(packet);
                    waiting_keyframe = false;
                }
                if decoder
                    .send_packet(&ffmpeg::Packet::copy(prefixed.as_deref().unwrap_or(&data)))
                    .is_err()
                {
                    metrics.decode_errors.fetch_add(1, Ordering::Relaxed);
                    waiting_keyframe = true;
                    continue;
                }
                let mut decoded = ffmpeg::frame::Video::empty();
                while decoder.receive_frame(&mut decoded).is_ok() {
                    metrics.decode_time.record(started.elapsed());
                    let conversion_started = Instant::now();
                    let width = decoded.width();
                    let height = decoded.height();
                    ensure!(
                        width > 0 && height > 0 && width <= 8192 && height <= 4320,
                        "视频尺寸无效"
                    );
                    let rate = decoder.frame_rate().map(|rate| (rate.0, rate.1));
                    let properties = (width, height, rate, decoder.delay(), decoder.has_b_frames());
                    if stream_properties != Some(properties) {
                        stream_properties = Some(properties);
                        crate::perf::trace(
                            "video_stream_properties",
                            serde_json::json!({
                                "width":width,"height":height,"frame_rate":rate,
                                "decoder_delay_frames":properties.3,"has_b_frames":properties.4
                            }),
                        );
                    }
                    let rgba = converter.convert(&decoded)?;
                    metrics.video_size.store(
                        (u64::from(width) << 32) | u64::from(height),
                        Ordering::Relaxed,
                    );
                    metrics.convert_time.record(conversion_started.elapsed());
                    let sequence = metrics.decoded.fetch_add(1, Ordering::Relaxed) + 1;
                    if sequence == 1 {
                        *metrics.first_frame.lock().unwrap() = Some(Instant::now());
                        crate::perf::trace(
                            "media_start",
                            serde_json::json!({"stage":"video_decoded","generation":generation,
                                "width":width,"height":height}),
                        );
                    }
                    let now = Instant::now();
                    if let Some(previous) = metrics.last_frame.lock().unwrap().replace(now) {
                        metrics.frame_interval.record(now.duration_since(previous));
                        metrics.max_gap_ms.fetch_max(
                            now.duration_since(previous).as_millis() as u64,
                            Ordering::Relaxed,
                        );
                    }
                    metrics
                        .decode_us
                        .store(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                    *latest.lock().unwrap() = Some(VideoFrame {
                        generation,
                        sequence,
                        width: width as usize,
                        height: height as usize,
                        rgba,
                        assembled_at: frame.unit.assembled_at,
                        decoded_at: now,
                    });
                    crate::backend::repaint_ui(&ctx, true);
                }
            }
            Ok(())
        })();
        if result.is_err() {
            metrics.decode_errors.fetch_add(1, Ordering::Relaxed);
        }
    });
    tx
}

pub struct Audio {
    pub packets: mpsc::SyncSender<Bytes>,
    pub enabled: Arc<AtomicBool>,
}

impl Audio {
    pub fn start(metrics: Arc<Metrics>) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Bytes>(8);
        let enabled = Arc::new(AtomicBool::new(true));
        let active = enabled.clone();
        std::thread::spawn(move || {
            let result: Result<()> = (|| {
                let host = cpal::default_host();
                let device = host.default_output_device().context("没有默认音频设备")?;
                let supported = device.default_output_config()?;
                let config: cpal::StreamConfig = supported.clone().into();
                let channels = config.channels as usize;
                let rate = config.sample_rate as usize;
                let queue = Arc::new(Mutex::new(VecDeque::<f32>::new()));
                let audio_metrics = metrics.clone();
                let error = move |_: cpal::StreamError| {
                    audio_metrics.audio_errors.fetch_add(1, Ordering::Relaxed);
                };
                macro_rules! stream {
                    ($t:ty) => {{
                        let queue = queue.clone();
                        let active = active.clone();
                        device.build_output_stream(
                            &config,
                            move |data: &mut [$t], _: &cpal::OutputCallbackInfo| {
                                let mut queue = queue.lock().unwrap();
                                for sample in data {
                                    let value = queue.pop_front().unwrap_or(0.0);
                                    *sample = cpal::Sample::from_sample(
                                        if active.load(Ordering::Relaxed) {
                                            value
                                        } else {
                                            0.0
                                        },
                                    );
                                }
                            },
                            error,
                            None,
                        )?
                    }};
                }
                let stream = match supported.sample_format() {
                    cpal::SampleFormat::F32 => stream!(f32),
                    cpal::SampleFormat::I16 => stream!(i16),
                    cpal::SampleFormat::U16 => stream!(u16),
                    _ => anyhow::bail!("音频格式不支持"),
                };
                stream.play()?;
                let mut decoder = opus::Decoder::new(48000, opus::Channels::Stereo)?;
                let mut pcm = vec![0f32; 5760 * 2];
                while let Ok(bytes) = rx.recv() {
                    let frames = match decoder.decode_float(&bytes, &mut pcm, false) {
                        Ok(frames) if frames > 0 => frames,
                        _ => {
                            metrics.audio_errors.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    };
                    metrics.audio_packets.fetch_add(1, Ordering::Relaxed);
                    let mut queue = queue.lock().unwrap();
                    // Bound latency to 120 ms even if the output device stalls.
                    let max = rate * channels * 120 / 1000;
                    if queue.len() > max {
                        let drop = queue.len() - max;
                        queue.drain(..drop);
                    }
                    let output_frames = frames * rate / 48000;
                    for n in 0..output_frames {
                        let position = n as f32 * 48000.0 / rate as f32;
                        let a = (position as usize).min(frames - 1);
                        let b = (a + 1).min(frames - 1);
                        let fraction = position - a as f32;
                        for channel in 0..channels {
                            let c = channel.min(1);
                            queue.push_back(
                                pcm[a * 2 + c] * (1.0 - fraction) + pcm[b * 2 + c] * fraction,
                            );
                        }
                    }
                }
                drop(stream);
                Ok(())
            })();
            if result.is_err() {
                metrics.audio_errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("远端音频播放不可用，请检查默认输出设备");
            }
        });
        Self {
            packets: tx,
            enabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Packet {
        Packet {
            header: webrtc::rtp::header::Header {
                sequence_number: seq,
                timestamp: ts,
                marker,
                ..Default::default()
            },
            payload: Bytes::copy_from_slice(payload),
        }
    }
    #[test]
    fn marker_displays_frame_without_waiting_for_another_frame() {
        let mut a = H264Assembler::default();
        let mut input = packet(40, 100, true, &[0x65, 1, 2, 3]);
        input.header.ssrc = 1234;
        let frames = a.push(input);
        assert_eq!(frames[0].rtp_timestamp, 100);
        assert_eq!(frames[0].rtp_ssrc, 1234);
        assert_eq!(
            frames.into_iter().map(|f| f.data).collect::<Vec<_>>(),
            vec![Bytes::from_static(&[0, 0, 0, 1, 0x65, 1, 2, 3])]
        );
        assert!(a.expire().is_empty());
    }
    #[test]
    fn repeated_old_packets_do_not_hide_a_new_video_data_gap() {
        let mut a = H264Assembler::default();
        assert_eq!(a.push(packet(40, 100, true, &[0x65, 1, 2])).len(), 1);
        let before = Instant::now() - Duration::from_secs(22);
        a.last_fresh_packet = Some((before, 0));
        for _ in 0..1400 {
            assert!(a.push(packet(40, 100, true, &[0x65, 1, 2])).is_empty());
        }
        assert_eq!(a.last_fresh_packet, Some((before, 0)));
        let next = packet(41, 200, true, &[0x65, 3, 4]);
        let gap = a
            .fresh_packet_gap(&next, before + Duration::from_secs(22))
            .unwrap();
        assert_eq!(gap["gap_ms"], 22_000.0);
        assert_eq!(gap["late_packets_during_gap"], 1400);
        assert_eq!(gap["continues_sequence"], true);
        assert_eq!(a.push(next).len(), 1);
        assert_eq!(a.losses, 0);
    }
    #[test]
    fn pending_duplicates_and_padding_do_not_refresh_data_progress() {
        let mut a = H264Assembler::default();
        a.push(packet(65535, 100, true, &[0x65, 1, 1]));
        a.push(packet(1, 200, true, &[0x61, 3, 3]));
        let progress = a.last_fresh_packet;
        assert!(a.push(packet(1, 200, true, &[0x61, 3, 3])).is_empty());
        assert_eq!(a.last_fresh_packet, progress);
        assert!(
            a.fresh_packet_gap(&packet(0, 200, false, &[]), Instant::now())
                .is_none()
        );
        assert_eq!(a.last_fresh_packet, progress);
        assert_eq!(a.push(packet(0, 200, false, &[0x61, 2, 2])).len(), 1);
        assert_eq!(a.losses, 0);
    }
    #[test]
    fn media_clock_interval_handles_wrap_and_discontinuity() {
        assert_eq!(
            rtp_frame_interval(100, 4600),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            rtp_frame_interval(u32::MAX - 2249, 2250),
            Some(Duration::from_millis(50))
        );
        assert_eq!(rtp_frame_interval(100, 100), None);
        assert_eq!(rtp_frame_interval(4600, 100), None);
        // Static desktops may intentionally stop sending; retain that interval
        // instead of interpreting it as clock failure or inventing zero FPS.
        assert_eq!(
            rtp_frame_interval(100, 1_800_100),
            Some(Duration::from_secs(20))
        );
    }
    #[test]
    fn repair_fills_gap_and_fu_fragments_are_emitted_in_order() {
        let mut a = H264Assembler::default();
        assert!(a.push(packet(10, 100, false, &[0x7c, 0x85, 1])).is_empty());
        assert!(a.push(packet(12, 100, true, &[0x7c, 0x45, 3])).is_empty());
        a.progressed = Instant::now() - std::time::Duration::from_millis(30);
        assert_eq!(a.missing(), vec![11]);
        assert_eq!(
            a.push(packet(11, 100, false, &[0x7c, 0x05, 2]))
                .into_iter()
                .map(|f| f.data)
                .collect::<Vec<_>>(),
            vec![Bytes::from_static(&[0, 0, 0, 1, 0x65, 1, 2, 3])]
        );
        assert!(a.push(packet(11, 100, false, &[0x7c, 0x05, 2])).is_empty());
        assert_eq!(a.losses, 0);
    }
    #[test]
    fn sequence_wrap_and_loss_do_not_join_two_frames() {
        let mut a = H264Assembler::default();
        assert_eq!(a.push(packet(65535, 100, true, &[0x65, 1, 1])).len(), 1);
        assert_eq!(a.push(packet(0, 200, true, &[0x61, 2, 2])).len(), 1);
        assert!(a.push(packet(2, 300, true, &[0x65, 3, 3])).is_empty());
        a.progressed = Instant::now() - std::time::Duration::from_millis(230);
        let frames = a.expire();
        assert_eq!(
            frames[0].data,
            Bytes::from_static(&[0, 0, 0, 1, 0x65, 3, 3])
        );
        assert_eq!(frames[0].losses, 1);
        assert_eq!(a.losses, 1);
        assert!(a.missing().is_empty());
    }
    #[test]
    fn queued_frames_keep_their_own_loss_boundary_even_in_one_drain() {
        let mut a = H264Assembler::default();
        let first = a.push(packet(1, 100, true, &[0x65, 1, 1]));
        assert_eq!(first[0].losses, 0);
        // Buffer a valid frame, damaged frame, and recovery IDR behind one gap.
        assert!(a.push(packet(3, 200, true, &[0x61, 3, 3])).is_empty());
        assert!(a.push(packet(4, 300, true, &[24, 0, 5, 0])).is_empty());
        assert!(a.push(packet(5, 400, true, &[0x65, 5, 5])).is_empty());
        let burst = a.push(packet(2, 200, false, &[0x61, 2, 2]));
        assert_eq!(a.losses, 1);
        assert_eq!(burst.len(), 2);
        assert_eq!(
            burst[0].losses, 0,
            "future damage must not invalidate the older frame"
        );
        assert_eq!(
            burst[1].losses, 1,
            "reset immediately before the recovery IDR"
        );
        assert_eq!(burst[1].data, Bytes::from_static(&[0, 0, 0, 1, 0x65, 5, 5]));
        assert_eq!(
            first[0].losses, 0,
            "already queued IDR must retain its epoch"
        );
    }
    #[test]
    fn malformed_fu_never_reaches_decoder() {
        let mut a = H264Assembler::default();
        assert!(a.push(packet(1, 100, true, &[0x7c, 0x45, 1])).is_empty());
        assert!(
            a.push(packet(2, 200, true, &[24, 0, 1, 0x65, 0]))
                .is_empty()
        );
        assert_eq!(a.push(packet(3, 300, true, &[0x65, 1, 2])).len(), 1);
    }
}
