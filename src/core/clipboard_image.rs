//! Standard Windows clipboard DIB pixels; no Windows API or DLL dependencies.
use anyhow::{Context, Result, ensure};
use image::{ImageEncoder, RgbaImage};
const MAX_PIXELS: u64 = 8_294_400;
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .context("DIB 头不完整")?
            .try_into()?,
    ))
}
pub fn from_dib(bytes: &[u8]) -> Result<RgbaImage> {
    let header = u32_at(bytes, 0)? as usize;
    ensure!(
        [40, 52, 56, 108, 124].contains(&header) && bytes.len() >= header,
        "DIB 头格式尚未实现"
    );
    let width = u32_at(bytes, 4)? as i32;
    let height = u32_at(bytes, 8)? as i32;
    ensure!(
        width > 0
            && height != 0
            && height != i32::MIN
            && width as u64 * height.unsigned_abs() as u64 <= MAX_PIXELS,
        "剪贴板图片尺寸超限"
    );
    let bits = u16::from_le_bytes(bytes[14..16].try_into()?);
    let compression = u32_at(bytes, 16)?;
    ensure!(
        u16::from_le_bytes(bytes[12..14].try_into()?) == 1
            && [24, 32].contains(&bits)
            && [0, 3].contains(&compression),
        "当前接收 24/32 位 RGB/位掩码 DIB"
    );
    let mut offset = header;
    let masks = if compression == 3 {
        ensure!(bits == 32, "16 位位掩码图片尚未实现");
        if header == 40 {
            offset += 12;
        }
        [
            u32_at(bytes, 40)?,
            u32_at(bytes, 44)?,
            u32_at(bytes, 48)?,
            if header >= 56 { u32_at(bytes, 52)? } else { 0 },
        ]
    } else {
        [0xff0000, 0xff00, 0xff, 0]
    };
    // Arbitrary ICC profiles require color management; don't reinterpret them as sRGB.
    if header == 124 {
        ensure!(
            u32_at(bytes, 112)? == 0 && u32_at(bytes, 116)? == 0,
            "带嵌入 ICC 配置的剪贴板图片尚未实现"
        );
    }
    ensure!(
        masks[..3] == [0xff0000, 0xff00, 0xff] && [0, 0xff000000].contains(&masks[3]),
        "图片颜色掩码尚未实现"
    );
    let palette = u32_at(bytes, 32)? as usize;
    ensure!(palette <= 256, "DIB 调色板数量无效");
    offset = offset.checked_add(palette * 4).context("图片偏移溢出")?;
    let width = width as u32;
    let h = height.unsigned_abs();
    let stride = (width as usize * bits as usize).div_ceil(32) * 4;
    ensure!(
        bytes.len() >= offset + stride * h as usize,
        "DIB 像素不完整"
    );
    let mut rgba = vec![0u8; width as usize * h as usize * 4];
    for y in 0..h as usize {
        let sy = if height < 0 { y } else { h as usize - 1 - y };
        for x in 0..width as usize {
            let p = offset + sy * stride + x * bits as usize / 8;
            let q = (y * width as usize + x) * 4;
            rgba[q..q + 4].copy_from_slice(&[
                bytes[p + 2],
                bytes[p + 1],
                bytes[p],
                if bits == 32 && masks[3] != 0 {
                    bytes[p + 3]
                } else {
                    255
                },
            ]);
        }
    }
    RgbaImage::from_raw(width, h, rgba).context("图片数据无效")
}
pub fn to_dib(image: &RgbaImage) -> Result<Vec<u8>> {
    let (width, height) = image.dimensions();
    ensure!(
        width > 0 && height > 0 && width as u64 * height as u64 <= MAX_PIXELS,
        "剪贴板图片尺寸超限"
    );
    let mut data = vec![0u8; 124];
    for (offset, value) in [
        (0, 124),
        (4, width),
        (8, (-(height as i32)) as u32),
        (16, 3),
        (20, width * height * 4),
        (40, 0xff0000),
        (44, 0xff00),
        (48, 0xff),
        (52, 0xff000000),
        (56, 0x73524742),
        (108, 4),
    ] {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    data[12..14].copy_from_slice(&1u16.to_le_bytes());
    data[14..16].copy_from_slice(&32u16.to_le_bytes());
    data.reserve(image.as_raw().len());
    for p in image.as_raw().as_chunks::<4>().0 {
        data.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
    }
    Ok(data)
}
pub fn png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    image::codecs::png::PngEncoder::new(&mut output).write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    ensure!(
        output.len() <= 2 * 1024 * 1024,
        "剪贴板 PNG 超过 2 MiB，请使用文件传输"
    );
    Ok(output)
}
pub fn from_png(data: &[u8]) -> Result<RgbaImage> {
    ensure!(data.len() <= 2 * 1024 * 1024, "剪贴板 PNG 超过 2 MiB");
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(data), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(3840);
    limits.max_image_height = Some(2160);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?.to_rgba8())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dib_preserves_rgba_order_alpha_and_orientation() {
        let image = RgbaImage::from_raw(
            2,
            2,
            vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 8, 16, 32, 64],
        )
        .unwrap();
        let dib = to_dib(&image).unwrap();
        assert_eq!(from_dib(&dib).unwrap(), image);
        assert_eq!(from_png(&png(&image).unwrap()).unwrap(), image);
        assert!(from_dib(&dib[..dib.len() - 1]).is_err());
        let mut bad = dib.clone();
        bad[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(from_dib(&bad).is_err());
    }
}
