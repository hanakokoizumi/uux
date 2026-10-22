//! Explicit, per-session native microphone capture. Disabled until host policy ACK.
use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

pub struct Microphone {
    pub enabled: Arc<AtomicBool>,
    pub device: Arc<Mutex<Option<String>>>,
    pub status: Arc<Mutex<Value>>,
    stop: Arc<AtomicBool>,
}
struct Input {
    _stream: Option<cpal::Stream>,
    native: Option<super::pulse_audio::NativeStream>,
    id: String,
    rate: usize,
    queue: Arc<Mutex<VecDeque<f32>>>,
    failed: Arc<AtomicBool>,
}
fn device(id: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match id {
        Some(id) => host
            .input_devices()?
            .find(|d| d.id().is_ok_and(|x| x.to_string() == id))
            .context("所选麦克风当前不可用"),
        None => host.default_input_device().context("没有默认麦克风"),
    }
}
fn open(device: cpal::Device) -> Result<Input> {
    let id = device.id()?.to_string();
    let supported = device.default_input_config()?;
    let config: cpal::StreamConfig = supported.clone().into();
    ensure!(
        config.channels > 0
            && config.channels <= 32
            && (8000..=384000).contains(&config.sample_rate),
        "麦克风格式无效"
    );
    let queue = Arc::new(Mutex::new(VecDeque::<f32>::new()));
    let failed = Arc::new(AtomicBool::new(false));
    let rate = config.sample_rate as usize;
    let channels = config.channels as usize;
    macro_rules! stream {
        ($t:ty) => {{
            let queue = queue.clone();
            let failed = failed.clone();
            device.build_input_stream(
                &config,
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut queue) = queue.try_lock() {
                        for frame in data.chunks_exact(channels) {
                            let sample = frame
                                .iter()
                                .map(|s| cpal::Sample::to_sample::<f32>(*s))
                                .sum::<f32>()
                                / channels as f32;
                            queue.push_back(sample);
                        }
                        let excess = queue.len().saturating_sub(rate / 10);
                        queue.drain(..excess);
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
        format => anyhow::bail!("麦克风格式暂未实现：{format:?}"),
    };
    stream.play()?;
    Ok(Input {
        _stream: Some(stream),
        native: None,
        id,
        rate,
        queue,
        failed,
    })
}
fn open_pulse(device: Option<&str>) -> Result<Input> {
    let stream = super::pulse_audio::NativeStream::open(true, device)?;
    Ok(Input {
        id: stream.id(),
        native: Some(stream),
        _stream: None,
        rate: 48000,
        queue: Arc::new(Mutex::new(VecDeque::new())),
        failed: Arc::new(AtomicBool::new(false)),
    })
}
impl Microphone {
    pub fn start(
        track: Arc<TrackLocalStaticSample>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        let enabled = Arc::new(AtomicBool::new(false));
        let device = Arc::new(Mutex::new(None::<String>));
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(json!({"state":"disabled","enabled":false})));
        let (packets, mut receiver) = tokio::sync::mpsc::channel::<Bytes>(4);
        let sent = Arc::new(AtomicU64::new(0));
        let errors = Arc::new(AtomicU64::new(0));
        let (count, failures, active) = (sent.clone(), errors.clone(), enabled.clone());
        tokio::spawn(async move {
            loop {
                tokio::select! { _=cancel.cancelled()=>break, packet=receiver.recv()=>{
                    let Some(data)=packet else {break};if !active.load(Ordering::Relaxed){continue;}
                    if track.write_sample(&webrtc::media::Sample{data,duration:Duration::from_millis(20),..Default::default()}).await.is_ok(){count.fetch_add(1,Ordering::Relaxed);}else{failures.fetch_add(1,Ordering::Relaxed);}
                } }
            }
        });
        let (active, selection, report, stopping) = (
            enabled.clone(),
            device.clone(),
            status.clone(),
            stop.clone(),
        );
        std::thread::spawn(move || {
            let mut source: Option<Input> = None;
            let mut encoder =
                opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Voip).ok();
            if let Some(e) = &mut encoder {
                let _ = e.set_bitrate(opus::Bitrate::Bits(64000));
            }
            let mut phase = 0.0;
            let mut selected = None;
            let mut probe = Instant::now() - Duration::from_secs(2);
            let mut encoded = [0u8; 4000];
            let mut pcm = [0f32; 960];
            while !stopping.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
                let requested = selection.lock().unwrap().clone();
                if !active.load(Ordering::Relaxed) {
                    source = None;
                    *report.lock().unwrap() =
                        json!({"state":"disabled","enabled":false,"requested_device":requested});
                    continue;
                }
                if selected != requested
                    || source
                        .as_ref()
                        .is_some_and(|s| s.failed.load(Ordering::Relaxed))
                {
                    source = None;
                    phase = 0.0;
                    selected = requested.clone();
                    probe = Instant::now() - Duration::from_secs(2);
                }
                if probe.elapsed() >= Duration::from_secs(1) {
                    probe = Instant::now();
                    if let Some(device) = super::pulse_audio::selection(requested.as_deref()) {
                        if source.is_none() {
                            match open_pulse(device) {
                                Ok(input) => source = Some(input),
                                Err(error) => {
                                    *report.lock().unwrap() = json!({"state":"unavailable","enabled":true,"requested_device":requested,"error":error.to_string()})
                                }
                            }
                        }
                    } else {
                        match device_fn(requested.as_deref()) {
                            Ok(d) => {
                                if d.id().ok().is_none_or(|id| {
                                    source.as_ref().is_none_or(|s| s.id != id.to_string())
                                }) {
                                    source = None;
                                    match open(d) {
                                        Ok(input) => source = Some(input),
                                        Err(error) => {
                                            *report.lock().unwrap() = json!({"state":"unavailable","enabled":true,"requested_device":requested,"error":error.to_string()})
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                source = None;
                                *report.lock().unwrap() = json!({"state":"unavailable","enabled":true,"requested_device":requested,"error":error.to_string()});
                            }
                        }
                    }
                }
                let Some(source) = &mut source else { continue };
                if let Some(native) = &mut source.native {
                    if let Err(error) = native.capture(&mut source.queue.lock().unwrap()) {
                        source.failed.store(true, Ordering::Relaxed);
                        *report.lock().unwrap() = json!({"state":"unavailable","enabled":true,"requested_device":requested,"error":error.to_string()});
                        continue;
                    }
                    source.id = native.id();
                }
                let Some(encoder) = &mut encoder else {
                    *report.lock().unwrap() =
                        json!({"state":"unavailable","enabled":true,"error":"Opus 编码器不可用"});
                    continue;
                };
                *report.lock().unwrap() = json!({"state":"capturing","enabled":true,"requested_device":requested,"actual_device":source.id,"capture_rate":source.rate,"sample_rate":48000,"channels":1,"codec":"opus","frame_duration_ms":20,"buffer_limit_ms":100,"submitted_packets":sent.load(Ordering::Relaxed),"send_errors":errors.load(Ordering::Relaxed),"remote_audio_observed":false});
                let mut queue = source.queue.lock().unwrap();
                if !resample(&mut queue, source.rate, &mut phase, &mut pcm) {
                    continue;
                }
                drop(queue);
                if let Ok(count) = encoder.encode_float(&pcm, &mut encoded) {
                    let _ = packets.try_send(Bytes::copy_from_slice(&encoded[..count]));
                }
            }
        });
        Self {
            enabled,
            device,
            status,
            stop,
        }
    }
}
// Avoid shadowing the configuration handle in the worker.
fn device_fn(id: Option<&str>) -> Result<cpal::Device> {
    device(id)
}
impl Drop for Microphone {
    fn drop(&mut self) {
        self.enabled.store(false, Ordering::Relaxed);
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn resample(queue: &mut VecDeque<f32>, rate: usize, phase: &mut f64, pcm: &mut [f32; 960]) -> bool {
    let step = rate as f64 / 48000.0;
    let end = *phase + 960.0 * step;
    let consumed = end.floor() as usize;
    if queue.len() < consumed + 2 {
        return false;
    }
    for (index, value) in pcm.iter_mut().enumerate() {
        let position = *phase + index as f64 * step;
        let a = position.floor() as usize;
        let fraction = (position - a as f64) as f32;
        *value = queue[a] * (1.0 - fraction) + queue[a + 1] * fraction;
    }
    queue.drain(..consumed);
    *phase = end - consumed as f64;
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn microphone_resampling_preserves_fractional_sample_positions() {
        for rate in [11025, 22050, 44100, 48000, 96000] {
            let mut queue = VecDeque::from(vec![0.25; rate + 4]);
            let mut phase = 0.0;
            let mut pcm = [0.0; 960];
            for _ in 0..50 {
                assert!(resample(&mut queue, rate, &mut phase, &mut pcm));
                assert!(pcm.iter().all(|v| (*v - 0.25).abs() < 1e-6));
            }
            assert!((queue.len() as isize - 4).abs() <= 1);
            assert!(phase < 1.0);
        }
    }
}
