//! Shared, bounded elementary streams. Never perform encoding or consumer I/O here.
use bytes::Bytes;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_AGE: Duration = Duration::from_secs(3);
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Geometry {
    pub screen_id: i32,
    pub version: u64,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone)]
pub struct Sample {
    pub sequence: u64,
    pub track: &'static str,
    pub codec: String,
    pub geometry: Geometry,
    pub timestamp_us: i64,
    pub timestamp_basis: &'static str,
    pub rtp_timestamp: u32,
    pub ssrc: u32,
    pub keyframe: bool,
    pub duration_us: Option<u64>,
    pub data: Bytes,
    pub received: Instant,
}
impl Sample {
    pub fn metadata(&self) -> Value {
        json!({"sequence":self.sequence,"track":self.track,"codec":self.codec,"format":match self.codec.as_str(){"rgba"=>"rgba8_packed","pcm_f32le"=>"pcm_f32le_interleaved",_=>if self.track=="video" {"annex_b_access_unit"} else {"opus_packet"}},"sample_rate":if self.track=="audio"{Some(48000)}else{None},"channels":if self.track=="audio"{Some(2)}else{None},"screen_id":self.geometry.screen_id,"version":self.geometry.version,"width":self.geometry.width,"height":self.geometry.height,"timestamp_us":self.timestamp_us,"timestamp_basis":self.timestamp_basis,"rtp_timestamp":self.rtp_timestamp,"ssrc":self.ssrc,"keyframe":self.keyframe,"duration_us":self.duration_us,"size":self.data.len()})
    }
}
struct Clock {
    rtp: u32,
    base: i64,
    sender: bool,
}
struct State {
    origin: Instant,
    wall_us: i64,
    sequence: u64,
    geometry: Geometry,
    codec: String,
    parameters: BTreeMap<u8, Bytes>,
    video: VecDeque<Arc<Sample>>,
    audio: VecDeque<Arc<Sample>>,
    video_bytes: usize,
    audio_bytes: usize,
    clocks: HashMap<u32, Clock>,
    loss: u64,
    waiting_keyframe: bool,
    video_dropped: u64,
    audio_dropped: u64,
    video_ssrc: Option<u32>,
}
pub struct MediaBus {
    pub decoder: Mutex<Value>,
    pub decoder_preference: Mutex<String>,
    state: Mutex<State>,
    pub request_keyframe: AtomicBool,
    pub clock_id: String,
}
impl Default for MediaBus {
    fn default() -> Self {
        Self {
            decoder: Mutex::new(json!({"state":"awaiting_media"})),
            decoder_preference: Mutex::new("auto".into()),
            state: Mutex::new(State {
                origin: Instant::now(),
                wall_us: (crate::service::now_ms() * 1000) as i64,
                sequence: 0,
                geometry: Geometry {
                    screen_id: -1,
                    version: 1,
                    width: 0,
                    height: 0,
                },
                codec: "h264".into(),
                parameters: BTreeMap::new(),
                video: VecDeque::new(),
                audio: VecDeque::new(),
                video_bytes: 0,
                audio_bytes: 0,
                clocks: HashMap::new(),
                loss: 0,
                waiting_keyframe: true,
                video_dropped: 0,
                audio_dropped: 0,
                video_ssrc: None,
            }),
            request_keyframe: AtomicBool::new(false),
            clock_id: crate::service::uid(),
        }
    }
}
impl State {
    fn invalidate(&mut self) {
        self.geometry.version += 1;
        self.video_dropped = self.sequence;
        self.video.clear();
        self.video_bytes = 0;
        self.waiting_keyframe = true;
    }
    fn timestamp(&mut self, ssrc: u32, rtp: u32, rate: u32) -> (i64, &'static str) {
        let arrival = self.wall_us + self.origin.elapsed().as_micros() as i64;
        let clock = self.clocks.entry(ssrc).or_insert(Clock {
            rtp,
            base: arrival,
            sender: false,
        });
        let delta = rtp.wrapping_sub(clock.rtp) as i32 as i64;
        (
            clock.base + delta * 1_000_000 / i64::from(rate),
            if clock.sender {
                "sender_ntp"
            } else {
                "receiver_estimated"
            },
        )
    }
    fn trim(queue: &mut VecDeque<Arc<Sample>>, bytes: &mut usize, max: usize, dropped: &mut u64) {
        while *bytes > max
            || queue
                .front()
                .is_some_and(|s| s.received.elapsed() > MAX_AGE)
        {
            if let Some(s) = queue.pop_front() {
                *bytes = bytes.saturating_sub(s.data.len());
                *dropped = s.sequence;
            } else {
                break;
            }
        }
    }
}
impl MediaBus {
    pub fn start_cursor(&self, video: bool) -> u64 {
        let s = self.state.lock().unwrap();
        if video {
            s.video
                .iter()
                .rev()
                .find(|v| v.keyframe)
                .map_or(s.sequence, |v| v.sequence.saturating_sub(1))
        } else {
            s.sequence
        }
    }
    pub fn geometry(&self) -> Geometry {
        self.state.lock().unwrap().geometry
    }
    pub fn screen(&self, id: i32) {
        let mut s = self.state.lock().unwrap();
        if s.geometry.screen_id != id {
            s.geometry.screen_id = id;
            s.invalidate();
            self.request_keyframe.store(true, Ordering::Relaxed);
        }
    }
    pub fn dimensions(&self, width: u32, height: u32, version: u64) -> Option<Geometry> {
        let mut s = self.state.lock().unwrap();
        if s.geometry.version != version {
            return None;
        }
        if s.geometry.width != width || s.geometry.height != height {
            s.invalidate();
            self.request_keyframe.store(true, Ordering::Relaxed);
            s.geometry.width = width;
            s.geometry.height = height;
        }
        Some(s.geometry)
    }
    pub fn sender_report(&self, ssrc: u32, rtp: u32, ntp: u64) {
        let seconds = (ntp >> 32) as i64 - 2_208_988_800;
        if seconds <= 0 {
            return;
        }
        let base = seconds * 1_000_000 + (((ntp & 0xffff_ffff) as u128 * 1_000_000) >> 32) as i64;
        let mut s = self.state.lock().unwrap();
        if s.clocks.get(&ssrc).is_some_and(|c| !c.sender) {
            s.invalidate();
            s.audio_dropped = s.sequence;
            s.audio.clear();
            s.audio_bytes = 0;
            self.request_keyframe.store(true, Ordering::Relaxed);
        }
        s.clocks.insert(
            ssrc,
            Clock {
                rtp,
                base,
                sender: true,
            },
        );
    }
    pub fn video(
        &self,
        data: Bytes,
        timestamp: u32,
        ssrc: u32,
        loss: u64,
        codec: &str,
    ) -> Geometry {
        let mut s = self.state.lock().unwrap();
        if s.video_ssrc.replace(ssrc).is_some_and(|old| old != ssrc) {
            s.parameters.clear();
            s.invalidate();
            self.request_keyframe.store(true, Ordering::Relaxed);
        }
        if s.codec != codec {
            s.codec = codec.into();
            s.parameters.clear();
            s.invalidate();
        }
        if s.loss != loss {
            s.loss = loss;
            s.invalidate();
            self.request_keyframe.store(true, Ordering::Relaxed);
        }
        let mut key = false;
        let mut changed = false;
        for nal in annex_b_units(&data) {
            if nal.is_empty() {
                continue;
            }
            let kind = if codec == "h265" {
                (nal[0] >> 1) & 63
            } else {
                nal[0] & 31
            };
            key |= if codec == "h265" {
                (16..=21).contains(&kind)
            } else {
                kind == 5
            };
            if if codec == "h265" {
                (32..=34).contains(&kind)
            } else {
                [7, 8].contains(&kind)
            } {
                if s.parameters
                    .get(&kind)
                    .is_some_and(|old| old.as_ref() != nal)
                {
                    changed = true;
                }
                s.parameters.insert(kind, Bytes::copy_from_slice(nal));
            }
        }
        if changed {
            s.invalidate();
            self.request_keyframe.store(true, Ordering::Relaxed);
        }
        let configured = if codec == "h265" {
            [32, 33, 34].iter().all(|k| s.parameters.contains_key(k))
        } else {
            [7, 8].iter().all(|k| s.parameters.contains_key(k))
        };
        if s.waiting_keyframe && (!key || !configured) {
            return s.geometry;
        }
        let data = if key && configured {
            let mut bytes = Vec::new();
            for nal in s.parameters.values() {
                bytes.extend_from_slice(&[0, 0, 0, 1]);
                bytes.extend_from_slice(nal);
            }
            bytes.extend_from_slice(&data);
            Bytes::from(bytes)
        } else {
            data
        };
        if data.len() > MAX_BYTES {
            s.invalidate();
            self.request_keyframe.store(true, Ordering::Relaxed);
            return s.geometry;
        }
        s.waiting_keyframe = false;
        let (timestamp_us, basis) = s.timestamp(ssrc, timestamp, 90000);
        s.sequence += 1;
        let sample = Arc::new(Sample {
            sequence: s.sequence,
            track: "video",
            codec: codec.into(),
            geometry: s.geometry,
            timestamp_us,
            timestamp_basis: basis,
            rtp_timestamp: timestamp,
            ssrc,
            keyframe: key,
            duration_us: None,
            data,
            received: Instant::now(),
        });
        s.video_bytes += sample.data.len();
        s.video.push_back(sample);
        let State {
            video,
            video_bytes,
            video_dropped,
            ..
        } = &mut *s;
        State::trim(video, video_bytes, MAX_BYTES, video_dropped);
        s.geometry
    }
    pub fn audio(&self, data: Bytes, timestamp: u32, ssrc: u32) {
        if data.len() > 65536 {
            return;
        }
        let duration_us = opus::packet::get_nb_samples(&data, 48000)
            .ok()
            .map(|n| n as u64 * 1_000_000 / 48000);
        let mut s = self.state.lock().unwrap();
        let (timestamp_us, basis) = s.timestamp(ssrc, timestamp, 48000);
        s.sequence += 1;
        let sample = Arc::new(Sample {
            sequence: s.sequence,
            track: "audio",
            codec: "opus".into(),
            geometry: s.geometry,
            timestamp_us,
            timestamp_basis: basis,
            rtp_timestamp: timestamp,
            ssrc,
            keyframe: true,
            duration_us,
            data,
            received: Instant::now(),
        });
        s.audio_bytes += sample.data.len();
        s.audio.push_back(sample);
        let State {
            audio,
            audio_bytes,
            audio_dropped,
            ..
        } = &mut *s;
        State::trim(audio, audio_bytes, 1024 * 1024, audio_dropped);
    }
    pub fn format(&self) -> Value {
        let s = self.state.lock().unwrap();
        json!({"clock_id":self.clock_id,"time_base":{"numerator":1,"denominator":1_000_000},"timestamp_epoch":"unix","timestamp_note":"sender_ntp timestamps share the sender clock; receiver_estimated timestamps use per-track arrival anchors, not exact A/V synchronization","geometry":s.geometry,"video":{"codec":s.codec,"format":"annex_b_access_unit","parameter_sets":"prepended to every keyframe"},"audio":{"codec":"opus","format":"opus_packet","sample_rate":48000,"channels":2},"buffer":{"video_bytes":MAX_BYTES,"audio_bytes":1024*1024,"max_age_ms":3000}})
    }
    pub fn read(
        &self,
        cursor: u64,
        video: bool,
        audio: bool,
        need_keyframe: bool,
        max_bytes: usize,
        max_duration_ms: u64,
    ) -> Batch {
        let mut s = self.state.lock().unwrap();
        {
            let State {
                video,
                video_bytes,
                audio,
                audio_bytes,
                video_dropped,
                audio_dropped,
                ..
            } = &mut *s;
            State::trim(video, video_bytes, MAX_BYTES, video_dropped);
            State::trim(audio, audio_bytes, 1024 * 1024, audio_dropped);
        }
        let gap = cursor > 0
            && ((video && cursor < s.video_dropped) || (audio && cursor < s.audio_dropped));
        let need = video && (need_keyframe || gap);
        let first_video = if need {
            s.video
                .iter()
                .find(|v| v.sequence > cursor && v.keyframe)
                .map(|v| v.sequence)
        } else {
            Some(0)
        };
        let mut samples: Vec<_> = s
            .video
            .iter()
            .filter(|v| {
                video && v.sequence > cursor && first_video.is_some_and(|n| v.sequence >= n)
            })
            .chain(s.audio.iter().filter(|v| audio && v.sequence > cursor))
            .cloned()
            .collect();
        samples.sort_by_key(|s| s.sequence);
        let mut chosen = Vec::new();
        let mut bytes = 0;
        let mut required = None;
        let first = samples.first().map(|s| s.received);
        for v in samples {
            if first.is_some_and(|first| {
                v.received.duration_since(first).as_millis() > u128::from(max_duration_ms)
            }) {
                break;
            }
            if bytes + v.data.len() > max_bytes {
                if chosen.is_empty() {
                    required = Some(v.data.len());
                }
                break;
            }
            bytes += v.data.len();
            chosen.push(v);
        }
        let waiting = need && !chosen.iter().any(|s| s.track == "video" && s.keyframe);
        if waiting {
            self.request_keyframe.store(true, Ordering::Relaxed);
        }
        let next = chosen.last().map_or(cursor, |s| s.sequence);
        Batch {
            samples: chosen,
            next_cursor: next,
            gap,
            waiting_keyframe: waiting,
            required_bytes: required,
            geometry: s.geometry,
        }
    }
}
pub struct Batch {
    pub samples: Vec<Arc<Sample>>,
    pub next_cursor: u64,
    pub gap: bool,
    pub waiting_keyframe: bool,
    pub required_bytes: Option<usize>,
    pub geometry: Geometry,
}
pub fn annex_b_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i..].starts_with(&[0, 0, 0, 1]) {
            starts.push((i, i + 4));
            i += 4;
        } else if data[i..].starts_with(&[0, 0, 1]) {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, (_, start))| &data[*start..starts.get(i + 1).map_or(data.len(), |(end, _)| *end)])
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> Bytes {
        Bytes::from_static(&[
            0, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 0, 1, 0x68, 9, 0, 0, 0, 1, 0x65, 2,
        ])
    }
    #[test]
    fn starts_on_keyframe_and_boundaries_are_versioned() {
        let b = MediaBus::default();
        b.screen(3);
        b.video(Bytes::from_static(&[0, 0, 1, 0x41, 2]), 1, 1, 0, "h264");
        assert!(b.read(0, true, false, true, 1024, 100).samples.is_empty());
        b.video(key(), 2, 1, 0, "h264");
        let read = b.read(0, true, false, true, 1024, 100);
        assert!(read.samples[0].keyframe);
        let old = read.geometry;
        b.screen(4);
        assert!(b.geometry().version > old.version);
        assert!(b.read(0, true, false, true, 1024, 100).samples.is_empty());
    }
    #[test]
    fn rtp_wrap_and_sender_reports_share_clock() {
        let b = MediaBus::default();
        let ntp = ((2_208_988_800u64 + 1000) << 32) + (1 << 31);
        b.sender_report(10, u32::MAX - 89, ntp);
        b.sender_report(20, 0, ntp);
        let mut s = b.state.lock().unwrap();
        assert_eq!(s.timestamp(10, 0, 90000), (1_000_501_000, "sender_ntp"));
        assert_eq!(s.timestamp(20, 48, 48000), (1_000_501_000, "sender_ntp"));
    }
    #[test]
    fn bounded_reads_never_split_access_units() {
        let b = MediaBus::default();
        b.video(key(), 0, 1, 0, "h264");
        let small = b.read(0, true, false, true, 4, 100);
        assert!(small.samples.is_empty());
        assert!(small.required_bytes.unwrap() > 4);
        let enough = b.read(0, true, false, true, 1024, 100);
        assert_eq!(enough.samples.len(), 1);
    }
    #[test]
    fn interleaved_track_sequences_do_not_create_false_gaps() {
        let b = MediaBus::default();
        b.video(key(), 0, 1, 0, "h264");
        let first = b.read(0, true, false, true, 1024, 100);
        b.audio(Bytes::from_static(&[0xf8, 0xff, 0xfe]), 0, 2);
        b.audio(Bytes::from_static(&[0xf8, 0xff, 0xfe]), 960, 2);
        b.video(Bytes::from_static(&[0, 0, 1, 0x41, 3]), 3000, 1, 0, "h264");
        let read = b.read(first.next_cursor, true, false, false, 1024, 100);
        assert!(!read.gap);
        assert_eq!(read.samples.len(), 1);
        assert!(!read.waiting_keyframe);
    }
    #[test]
    fn slow_consumer_recovers_without_flushing_fast_consumer() {
        let b = MediaBus::default();
        b.video(key(), 0, 1, 0, "h264");
        let slow = b.read(0, true, false, true, 1024, 100).next_cursor;
        b.video(Bytes::from_static(&[0, 0, 1, 0x41, 3]), 3000, 1, 0, "h264");
        {
            let mut s = b.state.lock().unwrap();
            s.video_dropped = s.sequence;
            s.video.clear();
            s.video_bytes = 0;
        }
        b.video(key(), 6000, 1, 0, "h264");
        let version = b.geometry().version;
        let read = b.read(slow, true, false, false, 1024, 100);
        assert!(read.gap);
        assert!(read.samples[0].keyframe);
        assert_eq!(b.geometry().version, version);
        assert!(!b.read(read.next_cursor, true, false, false, 1024, 100).gap);
    }
}
