//! Local synthetic-pixel comparison. No remote connection or captured content.
#[path = "../src/core/pixels.rs"]
mod pixels;

use anyhow::Result;
use ffmpeg_next as ffmpeg;
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};

fn main() -> Result<()> {
    ffmpeg::init()?;
    for (width, height) in [(1280, 720), (1920, 1080), (3840, 2160)] {
        let mut input = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P, width, height);
        for plane in 0..3 {
            for (i, sample) in input.data_mut(plane).iter_mut().enumerate() {
                *sample = 32 + (i % 192) as u8;
            }
        }
        let mut scaler = ffmpeg::software::scaling::Context::get(
            input.format(),
            width,
            height,
            ffmpeg::format::Pixel::RGBA,
            width,
            height,
            ffmpeg::software::scaling::Flags::FAST_BILINEAR,
        )?;
        let mut converter = pixels::RgbaConverter::default();
        let mut times: [Vec<f64>; 2] = Default::default();
        let mut expected = None;
        // Alternate which implementation goes first. Hashing is outside the
        // measured interval; verify every output, including the warm-up frames.
        for round in 0..120 {
            input.data_mut(0)[0] = 32 + round;
            for variant in [round as usize % 2, 1 - round as usize % 2] {
                let started = Instant::now();
                let output = if variant == 1 {
                    converter.convert(black_box(&input))?
                } else {
                    let mut rgb = ffmpeg::frame::Video::empty();
                    scaler.run(black_box(&input), &mut rgb)?;
                    let row = width as usize * 4;
                    let mut rgba = Vec::with_capacity(row * height as usize);
                    for y in 0..height as usize {
                        rgba.extend_from_slice(
                            &rgb.data(0)[y * rgb.stride(0)..y * rgb.stride(0) + row],
                        );
                    }
                    rgba
                };
                let elapsed = started.elapsed().as_secs_f64() * 1000.;
                if round >= 20 {
                    times[variant].push(elapsed);
                }
                let hash = Sha256::digest(black_box(&output));
                if let Some((previous_round, previous_hash)) = expected
                    && previous_round == round
                {
                    anyhow::ensure!(hash == previous_hash, "pixel mismatch");
                }
                expected = Some((round, hash));
            }
        }
        for (variant, samples) in times.iter_mut().enumerate() {
            samples.sort_by(f64::total_cmp);
            println!(
                "{}",
                serde_json::json!({
                    "width":width,"height":height,"reuse":variant == 1,"samples":samples.len(),
                    "median_ms":(samples[49]+samples[50])/2.,"p95_ms":samples[94],
                    "pixels_verified":true
                })
            );
        }
    }
    Ok(())
}
