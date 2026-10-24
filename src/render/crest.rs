//! Team crests: ESPN's 500 px PNG (cached by the football worker) scaled once with WIC to the
//! exact physical size it is drawn at, so it stays sharp (no bitmap stretching at draw time).

use std::path::Path;
use std::sync::Arc;

use windows::core::HSTRING;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeHighQualityCubic, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

/// A crest (square) or a cover (any shape) as premultiplied BGRA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crest {
    pub px: Arc<Vec<u8>>,
    pub size: (u32, u32),
}

/// The picture at `path` scaled to cover `w`×`h` px and cut to it around its centre: box art in
/// the pill's circle or the card's box; `None` if the file is missing or unreadable.
pub fn load_cover(path: &Path, w: u32, h: u32) -> Option<Crest> {
    if w == 0 || h == 0 || !path.exists() {
        return None;
    }
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let decoder = wic.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut iw, mut ih) = (0u32, 0u32);
        frame.GetSize(&mut iw, &mut ih).ok()?;
        let fill = (w as f32 / iw.max(1) as f32).max(h as f32 / ih.max(1) as f32);
        let (sw, sh) = (((iw as f32 * fill).ceil() as u32).max(w), ((ih as f32 * fill).ceil() as u32).max(h));
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, sw, sh, WICBitmapInterpolationModeHighQualityCubic).ok()?;
        let converter = wic.CreateFormatConverter().ok()?;
        converter.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let cut = windows::Win32::Graphics::Imaging::WICRect { X: ((sw - w) / 2) as i32, Y: ((sh - h) / 2) as i32, Width: w as i32, Height: h as i32 };
        let mut px = vec![0u8; (w * h * 4) as usize];
        converter.CopyPixels(&cut, w * 4, &mut px).ok()?;
        Some(Crest { px: Arc::new(px), size: (w, h) })
    }
}

/// The PNG at `path` scaled (high-quality cubic) to fit `side`×`side` px, centred; `None` if
/// the file is missing or unreadable.
pub fn load_crest(path: &Path, side: u32) -> Option<Crest> {
    if side == 0 || !path.exists() {
        return None;
    }
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let decoder = wic.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        let fit = side as f32 / w.max(h).max(1) as f32;
        let (sw, sh) = (((w as f32 * fit).round() as u32).clamp(1, side), ((h as f32 * fit).round() as u32).clamp(1, side));
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, sw, sh, WICBitmapInterpolationModeHighQualityCubic).ok()?;
        let converter = wic.CreateFormatConverter().ok()?;
        converter.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut scaled = vec![0u8; (sw * sh * 4) as usize];
        converter.CopyPixels(std::ptr::null(), sw * 4, &mut scaled).ok()?;
        // Centre a non-square crest in the square.
        let mut px = vec![0u8; (side * side * 4) as usize];
        let (ox, oy) = ((side - sw) / 2, (side - sh) / 2);
        for y in 0..sh {
            let from = (y * sw * 4) as usize;
            let to = (((y + oy) * side + ox) * 4) as usize;
            px[to..to + (sw * 4) as usize].copy_from_slice(&scaled[from..from + (sw * 4) as usize]);
        }
        Some(Crest { px: Arc::new(px), size: (side, side) })
    }
}

/// Writes a `w`×`h` BGRA PNG to `path`, `pixel(x, y)` for each pixel (tests only).
#[cfg(test)]
pub(crate) fn write_test_png(path: &Path, w: u32, h: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) {
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&pixel(x, y));
            }
        }
        let stream = wic.CreateStream().unwrap();
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), windows::Win32::Foundation::GENERIC_WRITE.0).unwrap();
        let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).unwrap();
        encoder.Initialize(&stream, WICBitmapEncoderNoCache).unwrap();
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut()).unwrap();
        let frame = frame.unwrap();
        frame.Initialize(None).unwrap();
        frame.SetSize(w, h).unwrap();
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format).unwrap();
        frame.WritePixels(h, w * 4, &px).unwrap();
        frame.Commit().unwrap();
        encoder.Commit().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 500×500 PNG: transparent, with an opaque red disc in the middle (like a crest).
    fn write_png(path: &std::path::Path) {
        write_test_png(path, 500, 500, |x, y| {
            let (dx, dy) = (x as i32 - 250, y as i32 - 250);
            if dx * dx + dy * dy < 200 * 200 { [0, 0, 255, 255] } else { [0; 4] }
        });
    }

    #[test]
    fn crest_scaled_to_exact_pixels_keeps_alpha() {
        let dir = std::env::temp_dir().join(format!("nowbar-crest-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("436.png");
        write_png(&path);
        let crest = load_crest(&path, 25).expect("decoded");
        assert_eq!(crest.size, (25, 25));
        assert_eq!(crest.px.len(), 25 * 25 * 4);
        let at = |x: usize, y: usize| &crest.px[(y * 25 + x) * 4..(y * 25 + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "corner stays transparent");
        let centre = at(12, 12);
        assert!(centre[3] == 255 && centre[2] > 240 && centre[0] < 15, "centre is opaque red: {centre:?}");
        assert!(load_crest(&dir.join("missing.png"), 25).is_none());
    }
}
