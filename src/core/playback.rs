//! Lazy native playback, with bounded latency and output-device recovery.
use super::media::Metrics;
use anyhow::{Context, Result};
use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub struct Audio {
    pub packets: mpsc::SyncSender<Bytes>,
    pub enabled: Arc<AtomicBool>,
    pub device: Arc<Mutex<Option<String>>>,
    pub status: Arc<Mutex<Value>>,
}
struct Output {
    _stream: Option<cpal::Stream>,
    native: Option<super::pulse_audio::NativeStream>,
    id: String,
    rate: usize,
    channels: usize,
    queue: Arc<Mutex<VecDeque<f32>>>,
    failed: Arc<AtomicBool>,
}
fn device(id: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match id {
        Some(id) => host
            .output_devices()?
            .find(|d| d.id().is_ok_and(|n| n.to_string() == id))
            .context("所选输出设备当前不可用"),
        None => host.default_output_device().context("没有默认音频输出设备"),
    }
}
fn open(device: cpal::Device) -> Result<Output> {
    let id = device.id()?.to_string();
    let supported = device.default_output_config()?;
    let config: cpal::StreamConfig = supported.clone().into();
    let queue = Arc::new(Mutex::new(VecDeque::<f32>::new()));
    let failed = Arc::new(AtomicBool::new(false));
    macro_rules! stream {
        ($t:ty) => {{
            let queue = queue.clone();
            let failed = failed.clone();
            device.build_output_stream(
                &config,
                move |data: &mut [$t], _: &cpal::OutputCallbackInfo| {
                    let mut buffer = queue.try_lock().ok();
                    for sample in data {
                        *sample = cpal::Sample::from_sample(
                            buffer.as_mut().and_then(|q| q.pop_front()).unwrap_or(0.0),
                        );
                    }
                },
                move |_| {
                    failed.store(true, Ordering::Relaxed);
                },
                None,
            )?
        }};
    }
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => stream!(f32),
        cpal::SampleFormat::I16 => stream!(i16),
        cpal::SampleFormat::U16 => stream!(u16),
        other => anyhow::bail!("输出设备格式暂未实现：{other:?}"),
    };
    stream.play()?;
    Ok(Output {
        _stream: Some(stream),
        native: None,
        id,
        rate: config.sample_rate as usize,
        channels: config.channels as usize,
        queue,
        failed,
    })
}
fn open_pulse(device: Option<&str>) -> Result<Output> {
    let stream = super::pulse_audio::NativeStream::open(false, device)?;
    Ok(Output {
        id: stream.id(),
        native: Some(stream),
        _stream: None,
        rate: 48000,
        channels: 2,
        queue: Arc::new(Mutex::new(VecDeque::new())),
        failed: Arc::new(AtomicBool::new(false)),
    })
}
impl Audio {
    pub fn start(metrics: Arc<Metrics>) -> Self {
        let (packets, rx) = mpsc::sync_channel::<Bytes>(8);
        let enabled = Arc::new(AtomicBool::new(false));
        let selected = Arc::new(Mutex::new(None::<String>));
        let status = Arc::new(Mutex::new(json!({"state":"disabled","enabled":false})));
        let (active, selection, report) = (enabled.clone(), selected.clone(), status.clone());
        std::thread::spawn(move || {
            let mut output: Option<Output> = None;
            let mut decoder = opus::Decoder::new(48000, opus::Channels::Stereo).ok();
            let mut pcm = vec![0.0; 5760 * 2];
            let mut last_selection = None;
            let mut last_probe = Instant::now() - Duration::from_secs(3);
            loop {
                let packet = match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(p) => Some(p),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(_) => break,
                };
                if packet.is_some() {
                    metrics.audio_packets.fetch_add(1, Ordering::Relaxed);
                }
                let requested = selection.lock().unwrap().clone();
                if !active.load(Ordering::Relaxed) {
                    output = None;
                    *report.lock().unwrap() =
                        json!({"state":"disabled","enabled":false,"requested_device":requested});
                    continue;
                }
                if last_selection != requested
                    || output
                        .as_ref()
                        .is_some_and(|o| o.failed.load(Ordering::Relaxed))
                {
                    output = None;
                    last_selection = requested.clone();
                    last_probe = Instant::now() - Duration::from_secs(3);
                }
                if last_probe.elapsed() >= Duration::from_secs(1) {
                    last_probe = Instant::now();
                    if let Some(device) = super::pulse_audio::selection(requested.as_deref()) {
                        if output.is_none() {
                            match open_pulse(device) {
                                Ok(o) => output = Some(o),
                                Err(e) => {
                                    *report.lock().unwrap() = json!({"state":"unavailable","enabled":true,"requested_device":requested,"error":e.to_string()})
                                }
                            }
                        }
                    } else {
                        match device(requested.as_deref()) {
                            Ok(d) => {
                                let changed = d.id().ok().is_none_or(|id| {
                                    output.as_ref().is_none_or(|o| o.id != id.to_string())
                                });
                                if changed {
                                    output = None;
                                    match open(d) {
                                        Ok(o) => output = Some(o),
                                        Err(e) => {
                                            *report.lock().unwrap() = json!({"state":"unavailable","enabled":true,"requested_device":requested,"error":e.to_string()})
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                output = None;
                                *report.lock().unwrap() = json!({"state":"unavailable","enabled":true,"requested_device":requested,"error":e.to_string()});
                            }
                        }
                    }
                }
                let Some(out) = &mut output else {
                    continue;
                };
                if let Some(native) = &mut out.native {
                    if let Err(error) = native.playback(&mut out.queue.lock().unwrap()) {
                        out.failed.store(true, Ordering::Relaxed);
                        *report.lock().unwrap() = json!({"state":"unavailable","enabled":true,"requested_device":requested,"error":error.to_string()});
                        continue;
                    }
                    out.id = native.id();
                }
                *report.lock().unwrap() = json!({"state":"playing","enabled":true,"requested_device":requested,"actual_device":out.id,"sample_rate":out.rate,"channels":out.channels,"buffer_limit_ms":120});
                let Some(bytes) = packet else {
                    continue;
                };
                let frames = match decoder
                    .as_mut()
                    .and_then(|d| d.decode_float(&bytes, &mut pcm, false).ok())
                    .filter(|n| *n > 0)
                {
                    Some(n) => n,
                    None => {
                        metrics.audio_errors.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
                let mut queue = out.queue.lock().unwrap();
                for n in 0..frames * out.rate / 48000 {
                    let position = n as f32 * 48000.0 / out.rate as f32;
                    let a = (position as usize).min(frames - 1);
                    let b = (a + 1).min(frames - 1);
                    let f = position - a as f32;
                    for channel in 0..out.channels {
                        let c = channel.min(1);
                        queue.push_back(pcm[a * 2 + c] * (1.0 - f) + pcm[b * 2 + c] * f);
                    }
                }
                let extra = queue
                    .len()
                    .saturating_sub(out.rate * out.channels * 120 / 1000);
                queue.drain(..extra);
            }
        });
        Self {
            packets,
            enabled,
            device: selected,
            status,
        }
    }
}
