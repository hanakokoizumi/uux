use anyhow::{Result, ensure};
use ffmpeg_next as ffmpeg;

/// Keep the scaler's aligned destination across frames. The returned packed
/// pixels belong to the consumer, so the next decode cannot overwrite them.
pub struct RgbaConverter {
    shape: Option<(ffmpeg::format::Pixel, u32, u32)>,
    scaler: Option<ffmpeg::software::scaling::Context>,
    output: ffmpeg::frame::Video,
}

impl Default for RgbaConverter {
    fn default() -> Self {
        Self {
            shape: None,
            scaler: None,
            output: ffmpeg::frame::Video::empty(),
        }
    }
}

impl RgbaConverter {
    pub fn convert(&mut self, input: &ffmpeg::frame::Video) -> Result<Vec<u8>> {
        let (width, height) = (input.width(), input.height());
        ensure!(
            width > 0 && height > 0 && width <= 8192 && height <= 4320,
            "视频尺寸无效"
        );
        let shape = (input.format(), width, height);
        if self.shape != Some(shape) {
            let scaler = ffmpeg::software::scaling::Context::get(
                input.format(),
                width,
                height,
                ffmpeg::format::Pixel::RGBA,
                width,
                height,
                ffmpeg::software::scaling::Flags::FAST_BILINEAR,
            )?;
            self.scaler = Some(scaler);
            self.output = ffmpeg::frame::Video::empty();
            self.shape = Some(shape);
        }
        self.scaler.as_mut().unwrap().run(input, &mut self.output)?;
        let row = width as usize * 4;
        let mut rgba = Vec::with_capacity(row * height as usize);
        let stride = self.output.stride(0);
        let data = self.output.data(0);
        if stride == row {
            rgba.extend_from_slice(&data[..row * height as usize]);
        } else {
            for y in 0..height as usize {
                rgba.extend_from_slice(&data[y * stride..y * stride + row]);
            }
        }
        Ok(rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuse_preserves_owned_pixels_across_frame_and_format_changes() {
        ffmpeg::init().unwrap();
        let mut converter = RgbaConverter::default();
        let mut retained = Vec::new();
        // Odd widths exercise FFmpeg's padded lines. Return to an earlier
        // shape as well as changing source format without changing dimensions.
        for (width, height, format, seed) in [
            (17, 13, ffmpeg::format::Pixel::YUV420P, 32),
            (17, 13, ffmpeg::format::Pixel::YUV420P, 224),
            (17, 13, ffmpeg::format::Pixel::YUV444P, 96),
            (32, 18, ffmpeg::format::Pixel::YUV420P, 160),
            (17, 13, ffmpeg::format::Pixel::YUV420P, 64),
        ] {
            let mut input = ffmpeg::frame::Video::new(format, width, height);
            for plane in 0..3 {
                input
                    .data_mut(plane)
                    .fill(if plane == 0 { seed } else { 128 });
            }
            let actual = converter.convert(&input).unwrap();
            let mut scaler = ffmpeg::software::scaling::Context::get(
                format,
                width,
                height,
                ffmpeg::format::Pixel::RGBA,
                width,
                height,
                ffmpeg::software::scaling::Flags::FAST_BILINEAR,
            )
            .unwrap();
            let mut reference = ffmpeg::frame::Video::empty();
            scaler.run(&input, &mut reference).unwrap();
            let expected: Vec<u8> = (0..height as usize)
                .flat_map(|y| {
                    let start = y * reference.stride(0);
                    reference.data(0)[start..start + width as usize * 4]
                        .iter()
                        .copied()
                })
                .collect();
            assert_eq!(actual, expected);
            retained.push((actual, expected));
        }
        // The GUI may still own earlier frames while the next one is decoded.
        for (actual, expected) in retained {
            assert_eq!(actual, expected);
        }
    }
}
