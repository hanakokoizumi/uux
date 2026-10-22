//! Native FFmpeg decoding. Hardware contexts belong to each session/decoder.
use anyhow::{Context, Result, ensure};
use ffmpeg_next::{self as ffmpeg, ffi};
use serde_json::{Value, json};
use std::{ffi::CStr, ptr};

pub fn preferences() -> [&'static str; 4] {
    ["auto", "software", "vaapi", "cuda"]
}
fn codec(name: &str) -> Result<ffmpeg::Codec> {
    ffmpeg::decoder::find(if name == "h265" {
        ffmpeg::codec::Id::HEVC
    } else {
        ffmpeg::codec::Id::H264
    })
    .context("FFmpeg 缺少所选解码器")
}
/// Receive/decode profiles, independent of the local monitor's HDR capability.
/// Every advertised combination has a software fallback and an encoded fixture.
pub fn profiles() -> Vec<Value> {
    if ffmpeg::init().is_err() {
        return vec![];
    }
    [(1, "h264"), (2, "h265")]
        .into_iter()
        .filter(|(_, name)| codec(name).is_ok())
        .flat_map(|(codec, _)| {
            [1, 3].into_iter().flat_map(move |chroma| {
                [8, 10].into_iter().map(move |depth| {
                    json!({"video_codec":codec,"width":3840,"height":2160,
                        "chroma_sampling":chroma,"bit_depth":depth,"codec_impl":37})
                })
            })
        })
        .collect()
}
pub fn capabilities() -> Value {
    let _ = ffmpeg::init();
    let codecs = ["h264", "h265"].into_iter().map(|name| {
        let mut hardware = Vec::new();
        if let Ok(codec) = codec(name) {
            // FFmpeg owns these immutable codec descriptors for process lifetime.
            unsafe { for index in 0..64 {
                let config = ffi::avcodec_get_hw_config(codec.as_ptr(), index);
                if config.is_null() { break; }
                if (*config).methods & 1 == 0 { continue; }
                let label = ffi::av_hwdevice_get_type_name((*config).device_type);
                if !label.is_null() { hardware.push(CStr::from_ptr(label).to_string_lossy().into_owned()); }
            } }
        }
        json!({"codec":name,"library_hardware_backends":hardware,"device_usable":"verified_when_opened"})
    }).collect::<Vec<_>>();
    json!({"preferences":preferences(),"codecs":codecs,"fallback":"software_on_device_or_decode_failure"})
}
unsafe extern "C" fn hardware_format(
    ctx: *mut ffi::AVCodecContext,
    formats: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // Each callback uses its context's device, never process-global GPU state.
    unsafe {
        if ctx.is_null() || formats.is_null() || (*ctx).hw_device_ctx.is_null() {
            return ffi::AVPixelFormat::AV_PIX_FMT_NONE;
        }
        let device = (*(*ctx).hw_device_ctx)
            .data
            .cast::<ffi::AVHWDeviceContext>();
        for index in 0..64 {
            let config = ffi::avcodec_get_hw_config((*ctx).codec, index);
            if config.is_null() {
                break;
            }
            if (*config).methods & 1 != 0 && (*config).device_type == (*device).type_ {
                for offset in 0..64 {
                    let format = *formats.add(offset);
                    if format == ffi::AVPixelFormat::AV_PIX_FMT_NONE {
                        break;
                    }
                    if format == (*config).pix_fmt {
                        return format;
                    }
                }
            }
        }
        ffi::AVPixelFormat::AV_PIX_FMT_NONE
    }
}

pub struct Decoder {
    pub video: ffmpeg::decoder::Video,
    pub backend: &'static str,
    pub fallback: Option<String>,
}
impl Decoder {
    pub fn open(name: &str, preference: &str) -> Result<Self> {
        ensure!(preferences().contains(&preference), "本地解码偏好无效");
        let mut failures = Vec::new();
        let choices: &[&'static str] = match preference {
            "auto" => &["vaapi", "cuda"],
            "vaapi" => &["vaapi"],
            "cuda" => &["cuda"],
            _ => &[],
        };
        for backend in choices {
            match Self::open_backend(name, backend) {
                Ok(mut result) => {
                    result.fallback = (!failures.is_empty()).then(|| failures.join("; "));
                    return Ok(result);
                }
                Err(error) => failures.push(format!("{backend}: {error}")),
            }
        }
        let mut result = Self::open_backend(name, "software")?;
        result.fallback = (!failures.is_empty()).then(|| failures.join("; "));
        Ok(result)
    }
    fn open_backend(name: &str, backend: &'static str) -> Result<Self> {
        let codec = codec(name)?;
        let mut context = ffmpeg::codec::Context::new_with_codec(codec);
        context.set_threading(ffmpeg::codec::threading::Config {
            kind: ffmpeg::codec::threading::Type::Slice,
            count: 2,
        });
        context.set_flags(ffmpeg::codec::Flags::LOW_DELAY);
        if backend != "software" {
            let kind = if backend == "vaapi" {
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI
            } else {
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA
            };
            unsafe {
                let mut supported = false;
                for index in 0..64 {
                    let config = ffi::avcodec_get_hw_config(codec.as_ptr(), index);
                    if config.is_null() {
                        break;
                    }
                    if (*config).device_type == kind && (*config).methods & 1 != 0 {
                        supported = true;
                        break;
                    }
                }
                ensure!(supported, "解码器未提供该硬件后端");
                let mut device = ptr::null_mut();
                let result =
                    ffi::av_hwdevice_ctx_create(&mut device, kind, ptr::null(), ptr::null_mut(), 0);
                if result < 0 {
                    if !device.is_null() {
                        ffi::av_buffer_unref(&mut device);
                    }
                    anyhow::bail!("{}", ffmpeg::Error::from(result));
                }
                // Context owns this reference; avcodec_free_context releases it.
                (*context.as_mut_ptr()).hw_device_ctx = device;
                (*context.as_mut_ptr()).get_format = Some(hardware_format);
            }
        }
        Ok(Self {
            video: context.decoder().video()?,
            backend,
            fallback: None,
        })
    }
    pub fn software_frame(&self, frame: &ffmpeg::frame::Video) -> Result<ffmpeg::frame::Video> {
        let mut output = ffmpeg::frame::Video::empty();
        unsafe {
            let result = ffi::av_hwframe_transfer_data(output.as_mut_ptr(), frame.as_ptr(), 0);
            ensure!(
                result >= 0,
                "硬件画面下载失败：{}",
                ffmpeg::Error::from(result)
            );
            let result = ffi::av_frame_copy_props(output.as_mut_ptr(), frame.as_ptr());
            ensure!(
                result >= 0,
                "画面元数据复制失败：{}",
                ffmpeg::Error::from(result)
            );
        }
        Ok(output)
    }
}
