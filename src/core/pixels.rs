use anyhow::{Context as _, Result, ensure};
use ffmpeg_next as ffmpeg;

/// Keep the scaler's aligned destination across frames. The returned packed
/// pixels belong to the consumer, so the next decode cannot overwrite them.
pub struct RgbaConverter {
    shape: Option<(ffmpeg::format::Pixel, u32, u32)>,
    scaler: Option<ffmpeg::software::scaling::Context>,
    output: ffmpeg::frame::Video,
    tone_map: Option<(String, ffmpeg::filter::Graph)>,
}
// SwsContext owns its allocations and is not thread-affine. It is never shared:
// conversion requires &mut self, and MCP readers serialize access with a mutex.
// FFmpeg's frame and filter graph wrappers are already Send. Do not implement Sync.
unsafe impl Send for RgbaConverter {}

impl Default for RgbaConverter {
    fn default() -> Self {
        Self {
            shape: None,
            scaler: None,
            output: ffmpeg::frame::Video::empty(),
            tone_map: None,
        }
    }
}

impl RgbaConverter {
    pub fn hdr_supported() -> bool {
        ["buffer", "buffersink", "zscale", "tonemap", "format"]
            .iter()
            .all(|f| ffmpeg::filter::find(f).is_some())
    }
    fn hdr_to_sdr(&mut self, input: &ffmpeg::frame::Video) -> Result<Vec<u8>> {
        ensure!(Self::hdr_supported(), "FFmpeg 缺少 HDR 到 SDR 色调映射滤镜");
        let max_nits = input
            .side_data(ffmpeg::util::frame::side_data::Type::ContentLightLevel)
            .and_then(|data| {
                data.data()
                    .get(..4)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_ne_bytes)
            })
            .filter(|n| (100..=10000).contains(n))
            .unwrap_or(1000);
        let peak = f64::from(max_nits) / 100.0;
        let shape = format!(
            "{:?}:{}:{}:{:?}:{:?}:{:?}:{:?}:{peak}",
            input.format(),
            input.width(),
            input.height(),
            input.color_space(),
            input.color_range(),
            input.color_primaries(),
            input.color_transfer_characteristic()
        );
        if self.tone_map.as_ref().is_none_or(|(old, _)| old != &shape) {
            let mut graph = ffmpeg::filter::Graph::new();
            // Keep per-session CPU usage bounded rather than FFmpeg's host-wide default.
            unsafe {
                (*graph.as_mut_ptr()).nb_threads = 2;
            }
            let format: ffmpeg::ffi::AVPixelFormat = input.format().into();
            let space: ffmpeg::ffi::AVColorSpace = input.color_space().into();
            let range: ffmpeg::ffi::AVColorRange = input.color_range().into();
            let args = format!(
                "video_size={}x{}:pix_fmt={}:time_base=1/1000:pixel_aspect=1/1:colorspace={}:range={}",
                input.width(),
                input.height(),
                format as i32,
                space as i32,
                range as i32
            );
            graph.add(
                &ffmpeg::filter::find("buffer").context("buffer filter missing")?,
                "in",
                &args,
            )?;
            graph.add(
                &ffmpeg::filter::find("buffersink").context("buffersink filter missing")?,
                "out",
                "",
            )?;
            graph.output("in",0)?.input("out",0)?.parse(&format!("zscale=transfer=linear:npl=100,format=gbrpf32le,zscale=primaries=bt709,tonemap=tonemap=mobius:desat=2:peak={peak},zscale=transfer=iec61966-2-1:matrix=gbr:range=full,format=rgba"))?;
            graph.validate()?;
            self.tone_map = Some((shape, graph));
        }
        let graph = &mut self.tone_map.as_mut().unwrap().1;
        // The safe wrapper's add() consumes/reset its nominally borrowed frame.
        // KEEP_REF explicitly leaves the decoder's frame and metadata intact.
        let result = unsafe {
            ffmpeg::ffi::av_buffersrc_add_frame_flags(
                graph.get("in").unwrap().as_mut_ptr(),
                input.as_ptr() as *mut _,
                ffmpeg::ffi::AV_BUFFERSRC_FLAG_KEEP_REF as i32,
            )
        };
        ensure!(
            result >= 0,
            "HDR 色调映射输入失败：{}",
            ffmpeg::Error::from(result)
        );
        let mut output = ffmpeg::frame::Video::empty();
        graph.get("out").unwrap().sink().frame(&mut output)?;
        Ok(packed(&output))
    }
    pub fn convert(&mut self, input: &ffmpeg::frame::Video) -> Result<Vec<u8>> {
        let (width, height) = (input.width(), input.height());
        ensure!(
            width > 0 && height > 0 && width <= 8192 && height <= 4320,
            "视频尺寸无效"
        );
        if matches!(
            input.color_transfer_characteristic(),
            ffmpeg::color::TransferCharacteristic::SMPTE2084
                | ffmpeg::color::TransferCharacteristic::ARIB_STD_B67
        ) {
            return self.hdr_to_sdr(input);
        }
        self.tone_map = None;
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
        let scaler = self.scaler.as_mut().unwrap();
        let matrix = match input.color_space() {
            ffmpeg::color::Space::BT709 => ffmpeg::ffi::SWS_CS_ITU709,
            ffmpeg::color::Space::BT2020NCL | ffmpeg::color::Space::BT2020CL => {
                ffmpeg::ffi::SWS_CS_BT2020
            }
            ffmpeg::color::Space::FCC => ffmpeg::ffi::SWS_CS_FCC,
            ffmpeg::color::Space::SMPTE240M => ffmpeg::ffi::SWS_CS_SMPTE240M,
            _ => ffmpeg::ffi::SWS_CS_DEFAULT,
        };
        unsafe {
            let coefficients = ffmpeg::ffi::sws_getCoefficients(matrix);
            let result = ffmpeg::ffi::sws_setColorspaceDetails(
                scaler.as_mut_ptr(),
                coefficients,
                i32::from(input.color_range() == ffmpeg::color::Range::JPEG),
                coefficients,
                1,
                0,
                1 << 16,
                1 << 16,
            );
            ensure!(result >= 0, "无法设置画面的颜色范围和矩阵");
        }
        scaler.run(input, &mut self.output)?;
        Ok(packed(&self.output))
    }
}
fn packed(output: &ffmpeg::frame::Video) -> Vec<u8> {
    let (width, height) = (output.width(), output.height());
    let row = width as usize * 4;
    let mut rgba = Vec::with_capacity(row * height as usize);
    let stride = output.stride(0);
    let data = output.data(0);
    if stride == row {
        rgba.extend_from_slice(&data[..row * height as usize]);
    } else {
        for y in 0..height as usize {
            rgba.extend_from_slice(&data[y * stride..y * stride + row]);
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hdr_is_tone_mapped_and_metadata_changes_rebuild_the_graph() {
        ffmpeg::init().unwrap();
        if !RgbaConverter::hdr_supported() {
            return;
        }
        let mut input = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV444P10LE, 16, 16);
        input.set_color_space(ffmpeg::color::Space::BT2020NCL);
        input.set_color_primaries(ffmpeg::color::Primaries::BT2020);
        input.set_color_transfer_characteristic(ffmpeg::color::TransferCharacteristic::SMPTE2084);
        input.set_color_range(ffmpeg::color::Range::MPEG);
        for plane in 1..3 {
            for pair in input.data_mut(plane).as_chunks_mut::<2>().0 {
                pair.copy_from_slice(&512u16.to_le_bytes());
            }
        }
        let mut converter = RgbaConverter::default();
        let mut brightness = Vec::new();
        for luma in [64u16, 509, 721, 940] {
            for pair in input.data_mut(0).as_chunks_mut::<2>().0 {
                pair.copy_from_slice(&luma.to_le_bytes());
            }
            let rgba = converter.convert(&input).unwrap();
            assert_eq!(rgba.len(), 16 * 16 * 4);
            assert!(rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            brightness.push(rgba[0]);
        }
        assert_eq!(brightness[0], 0);
        assert!(
            brightness[1] > 100
                && brightness[1] < brightness[2]
                && brightness.windows(2).all(|w| w[0] <= w[1]),
            "{brightness:?}"
        );
        input
            .set_color_transfer_characteristic(ffmpeg::color::TransferCharacteristic::ARIB_STD_B67);
        assert_eq!(converter.convert(&input).unwrap().len(), 16 * 16 * 4);
    }

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
