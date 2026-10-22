//! Optional per-reader decoding. Encoded subscriptions never instantiate these.
use crate::media_bus::Sample;
use anyhow::{Result, ensure};
use bytes::Bytes;
use std::{collections::BTreeMap, sync::Arc};
#[derive(Default)]
pub struct Decoder {
    video: Option<(String, ffmpeg_next::decoder::Video)>,
    audio: Option<opus::Decoder>,
    timestamps: BTreeMap<i64, Arc<Sample>>,
    pixels: crate::core::pixels::RgbaConverter,
}
impl Decoder {
    pub fn convert(
        &mut self,
        sample: Arc<Sample>,
        raw_video: bool,
        raw_audio: bool,
    ) -> Result<Option<Arc<Sample>>> {
        if sample.track == "audio" && raw_audio {
            if self.audio.is_none() {
                self.audio = Some(opus::Decoder::new(48000, opus::Channels::Stereo)?);
            }
            let mut pcm = vec![0f32; 5760 * 2];
            let frames =
                self.audio
                    .as_mut()
                    .unwrap()
                    .decode_float(&sample.data, &mut pcm, false)?;
            let mut output = (*sample).clone();
            output.codec = "pcm_f32le".into();
            output.duration_us = Some(frames as u64 * 1_000_000 / 48000);
            output.data = Bytes::from(
                pcm[..frames * 2]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<_>>(),
            );
            return Ok(Some(Arc::new(output)));
        }
        if sample.track == "video" && raw_video {
            if self
                .video
                .as_ref()
                .is_none_or(|(codec, _)| codec != &sample.codec)
            {
                let decoder = crate::core::decoder::Decoder::open(&sample.codec, "software")?;
                self.video = Some((sample.codec.clone(), decoder.video));
                self.timestamps.clear();
            }
            let (_, decoder) = self.video.as_mut().unwrap();
            let seq = sample.sequence as i64;
            self.timestamps.insert(seq, sample.clone());
            while self.timestamps.len() > 32 {
                self.timestamps.pop_first();
            }
            let mut packet = ffmpeg_next::Packet::copy(&sample.data);
            packet.set_pts(Some(seq));
            decoder.send_packet(&packet)?;
            let mut frame = ffmpeg_next::frame::Video::empty();
            let mut output = None;
            while decoder.receive_frame(&mut frame).is_ok() {
                ensure!(
                    frame.width() <= 3840 && frame.height() <= 2160,
                    "解码帧订阅最多支持 3840 × 2160"
                );
                let source = frame
                    .pts()
                    .and_then(|pts| self.timestamps.remove(&pts))
                    .unwrap_or_else(|| sample.clone());
                let mut item = (*source).clone();
                item.codec = "rgba".into();
                item.keyframe = true;
                item.geometry.width = frame.width();
                item.geometry.height = frame.height();
                item.data = Bytes::from(self.pixels.convert(&frame)?);
                output = Some(Arc::new(item));
            }
            return Ok(output);
        }
        Ok(Some(sample))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(codec: &str, data: &[u8]) -> Arc<Sample> {
        Arc::new(Sample {
            sequence: 1,
            track: if codec == "opus" { "audio" } else { "video" },
            codec: codec.into(),
            geometry: crate::media_bus::Geometry {
                screen_id: 9,
                version: 3,
                width: 32,
                height: 18,
            },
            timestamp_us: 123456,
            timestamp_basis: "sender_ntp",
            rtp_timestamp: 77,
            ssrc: 9,
            keyframe: true,
            duration_us: None,
            data: Bytes::copy_from_slice(data),
            received: std::time::Instant::now(),
        })
    }
    #[test]
    fn original_h264_and_hevc_decode_to_pixels_with_clock_and_screen_binding() {
        ffmpeg_next::init().unwrap();
        for (codec, data) in [
            (
                "h264",
                include_bytes!("../tests/fixtures/blue.h264").as_slice(),
            ),
            (
                "h265",
                include_bytes!("../tests/fixtures/blue.h265").as_slice(),
            ),
            (
                "h264",
                include_bytes!("../tests/fixtures/blue10.h264").as_slice(),
            ),
            (
                "h265",
                include_bytes!("../tests/fixtures/blue10.h265").as_slice(),
            ),
            (
                "h264",
                include_bytes!("../tests/fixtures/blue444.h264").as_slice(),
            ),
            (
                "h265",
                include_bytes!("../tests/fixtures/blue444.h265").as_slice(),
            ),
            (
                "h264",
                include_bytes!("../tests/fixtures/blue44410.h264").as_slice(),
            ),
            (
                "h265",
                include_bytes!("../tests/fixtures/blue44410.h265").as_slice(),
            ),
        ] {
            let output = Decoder::default()
                .convert(sample(codec, data), true, false)
                .unwrap()
                .unwrap();
            assert_eq!(output.data.len(), 32 * 18 * 4);
            assert_eq!(output.timestamp_us, 123456);
            assert_eq!(output.geometry.screen_id, 9);
            assert_eq!(output.geometry.version, 3);
            assert!(
                output
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[2] > 200 && p[0] < 15 && p[1] < 15 && p[3] == 255)
            );
        }
    }
    #[test]
    fn opus_pcm_keeps_duration_and_has_no_audio_device_dependency() {
        let mut encoder =
            opus::Encoder::new(48000, opus::Channels::Stereo, opus::Application::Audio).unwrap();
        let mut packet = [0u8; 4000];
        let count = encoder.encode_float(&[0.0; 1920], &mut packet).unwrap();
        let output = Decoder::default()
            .convert(sample("opus", &packet[..count]), false, true)
            .unwrap()
            .unwrap();
        assert_eq!(output.data.len(), 1920 * 4);
        assert_eq!(output.duration_us, Some(20_000));
        assert_eq!(output.codec, "pcm_f32le");
    }
}
