//! Getting pixels out of files, as few of them as the caller needs.

use crate::library::Kind;
use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::{DynamicImage, ImageDecoder, metadata::Orientation};
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};

/// Tightly packed 8-bit RGB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgb8 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgb8 {
    pub fn pixels(&self) -> impl Iterator<Item = [u8; 3]> + '_ {
        self.data.as_chunks::<3>().0.iter().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoded {
    /// Size of the original, before any scaling on the way in.
    pub width: u32,
    pub height: u32,
    /// More than one frame.
    pub animated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Image(String),
    #[error("video thumbnails need ffmpeg: {0}")]
    Video(String),
}

/// Decode `path` to at least `want_w` x `want_h` where the format allows it.
///
/// JPEG is decoded at a reduced scale inside the IDCT when the original is much
/// larger than what is wanted, which is most of the cost of thumbnailing a
/// folder of camera-sized wallpapers.
pub fn load(
    path: &Path,
    kind: Kind,
    want_w: u32,
    want_h: u32,
) -> Result<(Rgb8, Decoded), DecodeError> {
    match kind {
        Kind::Video => load_video_frame(path, want_w),
        _ if is_jpeg(path)? => load_jpeg(path, want_w, want_h),
        _ => load_generic(path, kind),
    }
}

fn is_jpeg(path: &Path) -> std::io::Result<bool> {
    let mut magic = [0u8; 3];
    let mut file = std::fs::File::open(path)?;
    Ok(file.read_exact(&mut magic).is_ok() && magic == [0xFF, 0xD8, 0xFF])
}

fn load_jpeg(path: &Path, want_w: u32, want_h: u32) -> Result<(Rgb8, Decoded), DecodeError> {
    let file = std::fs::File::open(path)?;
    let mut decoder = jpeg_decoder::Decoder::new(BufReader::new(file));
    decoder.read_info().map_err(|e| DecodeError::Image(e.to_string()))?;
    let info = decoder.info().ok_or_else(|| DecodeError::Image("no JPEG header".into()))?;
    let (orig_w, orig_h) = (info.width as u32, info.height as u32);
    let orientation = decoder.exif_data().and_then(exif_orientation);

    // Orientation swaps the axes for 90 degree turns, so ask for enough of
    // whichever axis will end up horizontal.
    let (want_w, want_h) = match orientation {
        Some(o) if swaps_axes(o) => (want_h, want_w),
        _ => (want_w, want_h),
    };
    let clamp = |v: u32| v.clamp(1, u16::MAX as u32) as u16;
    decoder.scale(clamp(want_w), clamp(want_h)).map_err(|e| DecodeError::Image(e.to_string()))?;
    let raw = decoder.decode().map_err(|e| DecodeError::Image(e.to_string()))?;
    let info = decoder.info().ok_or_else(|| DecodeError::Image("no JPEG header".into()))?;
    let (w, h) = (info.width as u32, info.height as u32);
    let data = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => raw,
        jpeg_decoder::PixelFormat::L8 => raw.iter().flat_map(|&v| [v, v, v]).collect(),
        jpeg_decoder::PixelFormat::L16 => {
            raw.as_chunks::<2>().0.iter().flat_map(|v| [v[0]; 3]).collect()
        }
        jpeg_decoder::PixelFormat::CMYK32 => raw
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let k = 255 - p[3] as u32;
                let f = |c: u8| ((255 - c as u32) * (255 - k) / 255) as u8;
                [f(p[0]), f(p[1]), f(p[2])]
            })
            .collect(),
    };
    let mut img = Rgb8 { width: w, height: h, data };
    let (mut ow, mut oh) = (orig_w, orig_h);
    if let Some(o) = orientation {
        img = orient(img, o);
        if swaps_axes(o) {
            (ow, oh) = (oh, ow);
        }
    }
    Ok((img, Decoded { width: ow, height: oh, animated: false }))
}

fn load_generic(path: &Path, kind: Kind) -> Result<(Rgb8, Decoded), DecodeError> {
    let image_err = |e: image::ImageError| DecodeError::Image(e.to_string());
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder().map_err(image_err)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(image_err)?;
    img.apply_orientation(orientation);
    let animated = kind == Kind::Animated && gif_has_frames(path);
    let rgb = img.into_rgb8();
    let (w, h) = rgb.dimensions();
    Ok((
        Rgb8 { width: w, height: h, data: rgb.into_raw() },
        Decoded { width: w, height: h, animated },
    ))
}

fn gif_has_frames(path: &Path) -> bool {
    use image::AnimationDecoder;
    let Ok(file) = std::fs::File::open(path) else { return false };
    let Ok(decoder) = image::codecs::gif::GifDecoder::new(BufReader::new(file)) else {
        return false;
    };
    decoder.into_frames().take(2).filter(Result::is_ok).count() > 1
}

/// One frame from the video, via ffmpeg, which is the only dependency that
/// reads every container people use for video wallpapers.
fn load_video_frame(path: &Path, want_w: u32) -> Result<(Rgb8, Decoded), DecodeError> {
    // A second in skips the black frame many loops start on. Short clips have
    // no second second, so fall back to the first frame.
    let frame =
        ffmpeg_frame(path, want_w, Some("1")).or_else(|_| ffmpeg_frame(path, want_w, None))?;
    let img = image::load_from_memory(&frame).map_err(|e| DecodeError::Video(e.to_string()))?;
    let rgb = img.into_rgb8();
    let (w, h) = rgb.dimensions();
    Ok((
        Rgb8 { width: w, height: h, data: rgb.into_raw() },
        Decoded { width: w, height: h, animated: true },
    ))
}

fn ffmpeg_frame(path: &Path, want_w: u32, seek: Option<&str>) -> Result<Vec<u8>, DecodeError> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin"]);
    if let Some(at) = seek {
        cmd.args(["-ss", at]);
    }
    let scale = format!("scale='min({want_w}*2,iw)':-2");
    cmd.arg("-i").arg(path).args([
        "-frames:v",
        "1",
        "-vf",
        &scale,
        "-f",
        "image2pipe",
        "-vcodec",
        "png",
        "-",
    ]);
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| DecodeError::Video(e.to_string()))?;
    if !out.status.success() || out.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(DecodeError::Video(stderr.lines().last().unwrap_or("no frame").to_owned()));
    }
    Ok(out.stdout)
}

/// Scale and centre-crop to exactly `w` x `h`, the way a wallpaper is shown.
pub fn cover(src: &Rgb8, w: u32, h: u32) -> Rgb8 {
    resize(src, w, h, true)
}

fn resize(src: &Rgb8, w: u32, h: u32, crop: bool) -> Rgb8 {
    let fallback = || Rgb8 { width: w, height: h, data: vec![0; (w * h * 3) as usize] };
    let Ok(src_ref) = ImageRef::new(src.width, src.height, &src.data, PixelType::U8x3) else {
        return fallback();
    };
    let mut dst = Image::new(w, h, PixelType::U8x3);
    let mut options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    if crop {
        options = options.fit_into_destination(Some((0.5, 0.5)));
    }
    match Resizer::new().resize(&src_ref, &mut dst, &options) {
        Ok(()) => Rgb8 { width: w, height: h, data: dst.into_vec() },
        Err(e) => {
            tracing::warn!("resize failed: {e}");
            fallback()
        }
    }
}

/// Scale to `w` x `h` ignoring aspect ratio. For tiny colour samples, where
/// keeping every part of the image matters more than its proportions.
pub fn squash(src: &Rgb8, w: u32, h: u32) -> Rgb8 {
    resize(src, w, h, false)
}

fn swaps_axes(o: Orientation) -> bool {
    matches!(
        o,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

fn orient(img: Rgb8, o: Orientation) -> Rgb8 {
    let Some(buf) = image::RgbImage::from_raw(img.width, img.height, img.data) else {
        return Rgb8 { width: 0, height: 0, data: Vec::new() };
    };
    let mut dynamic = DynamicImage::ImageRgb8(buf);
    dynamic.apply_orientation(o);
    let rgb = dynamic.into_rgb8();
    let (width, height) = rgb.dimensions();
    Rgb8 { width, height, data: rgb.into_raw() }
}

/// Read the orientation tag from a raw EXIF block (a TIFF structure).
fn exif_orientation(exif: &[u8]) -> Option<Orientation> {
    let exif = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    let le = match exif.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b = exif.get(at..at + 2)?;
        Some(if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = exif.get(at..at + 4)?;
        let b = [b[0], b[1], b[2], b[3]];
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let ifd = u32_at(4)? as usize;
    let entries = u16_at(ifd)? as usize;
    (0..entries.min(256))
        .find_map(|i| {
            let entry = ifd + 2 + i * 12;
            (u16_at(entry)? == 0x0112).then(|| u16_at(entry + 8)).flatten()
        })
        .and_then(|v| Orientation::from_exif(v as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> image::RgbImage {
        image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, 90])
        })
    }

    #[test]
    fn jpeg_is_decoded_at_reduced_scale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.jpg");
        gradient(2048, 1536).save(&path).unwrap();
        let (img, decoded) = load(&path, Kind::Still, 256, 144).unwrap();
        assert_eq!((decoded.width, decoded.height), (2048, 1536));
        // 1/8 scale is the largest reduction that still covers 256x144.
        assert_eq!((img.width, img.height), (256, 192));
    }

    #[test]
    fn png_goes_through_the_generic_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.png");
        gradient(64, 32).save(&path).unwrap();
        let (img, decoded) = load(&path, Kind::Still, 16, 9).unwrap();
        assert_eq!((img.width, img.height, decoded.animated), (64, 32, false));
    }

    #[test]
    fn cover_crops_to_the_exact_size() {
        let src = Rgb8 { width: 400, height: 100, data: vec![128; 400 * 100 * 3] };
        let out = cover(&src, 160, 90);
        assert_eq!((out.width, out.height, out.data.len()), (160, 90, 160 * 90 * 3));
        assert!(out.data.iter().all(|&v| (126..=130).contains(&v)));
    }

    #[test]
    fn reads_exif_orientation_both_endians() {
        // TIFF header, one IFD with one entry: tag 0x0112, type SHORT, count 1, value 6.
        let le = [b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0];
        assert_eq!(exif_orientation(&le), Some(Orientation::Rotate90));
        let be = [b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 3, 0, 0];
        assert_eq!(exif_orientation(&be), Some(Orientation::Rotate180));
        assert_eq!(exif_orientation(b"junk"), None);
    }
}
