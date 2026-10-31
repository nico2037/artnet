use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use glow::HasContext;

pub const GOBO_SIZE: usize = 256;

/// PNG decoded to 8-bit samples (1-4 channels).
struct Image {
    width: usize,
    height: usize,
    channels: usize,
    data: Vec<u8>,
}

fn decode_png(reader: impl Read) -> Result<Image> {
    let mut decoder = png::Decoder::new(reader);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut data = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut data)?;
    Ok(Image { width: info.width as usize, height: info.height as usize, channels: info.color_type.samples(), data })
}

/// ITU-R 601 luma, like Pillow's "L".
fn luma(p: &[u8], channels: usize) -> f32 {
    if channels >= 3 { 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32 } else { p[0] as f32 }
}

/// `value` per source pixel, bilinearly resampled to GOBO_SIZE².
fn resample(img: &Image, value: impl Fn(&[u8]) -> f32) -> Vec<u8> {
    let (w, h, ch) = (img.width, img.height, img.channels);
    let at = |x: usize, y: usize| value(&img.data[(y * w + x) * ch..]);
    let mut out = vec![0u8; GOBO_SIZE * GOBO_SIZE];
    for y in 0..GOBO_SIZE {
        for x in 0..GOBO_SIZE {
            let sx = ((x as f32 + 0.5) * w as f32 / GOBO_SIZE as f32 - 0.5).clamp(0.0, (w - 1) as f32);
            let sy = ((y as f32 + 0.5) * h as f32 / GOBO_SIZE as f32 - 0.5).clamp(0.0, (h - 1) as f32);
            let (x0, y0) = (sx as usize, sy as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
            let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
            out[y * GOBO_SIZE + x] = (top * (1.0 - fy) + bottom * fy).round() as u8;
        }
    }
    out
}

/// Grayscale resampled to GOBO_SIZE².
fn load_gray(path: &Path) -> Result<Vec<u8>> {
    let img = decode_png(File::open(path)?)?;
    let ch = img.channels;
    Ok(resample(&img, |p| luma(p, ch)))
}

/// GDTF wheel image as transmission (alpha × brightness; transparent = gobo holder) at GOBO_SIZE².
pub fn gobo_transmission(png: &[u8]) -> Result<Vec<u8>> {
    let img = decode_png(png)?;
    let ch = img.channels;
    let has_alpha = ch == 2 || ch == 4;
    Ok(resample(&img, |p| luma(p, ch) * if has_alpha { p[ch - 1] as f32 / 255.0 } else { 1.0 }))
}

/// All *.png in `dir` (sorted) as one R8 texture array with mipmaps; returns (texture, gobo count).
pub(crate) unsafe fn load_gobos(gl: &glow::Context, dir: &Path) -> Result<(glow::Texture, usize)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let mut data = Vec::new();
    for f in &files {
        data.extend(load_gray(f).with_context(|| format!("loading gobo {}", f.display()))?);
    }
    let layers = files.len().max(1);
    if files.is_empty() {
        data = vec![255; GOBO_SIZE * GOBO_SIZE];
    }

    let tex = gl.create_texture().map_err(|e| anyhow!(e))?;
    gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(tex));
    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
    gl.tex_image_3d(
        glow::TEXTURE_2D_ARRAY, 0, glow::R8 as i32, GOBO_SIZE as i32, GOBO_SIZE as i32, layers as i32, 0,
        glow::RED, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(&data)),
    );
    gl.generate_mipmap(glow::TEXTURE_2D_ARRAY);
    gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    Ok((tex, files.len()))
}

/// Tileable smooth 3D noise: random values blurred with a periodic Gaussian (same character as the
/// Python version, not bit-identical).
pub(crate) unsafe fn noise_texture(gl: &glow::Context) -> Result<glow::Texture> {
    const N: usize = 32;
    const SIGMA: f32 = 1.2;
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut v: Vec<f32> = (0..N * N * N)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / (1u64 << 24) as f32
        })
        .collect();

    let radius = (3.0 * SIGMA).ceil() as isize;
    let kernel: Vec<f32> = (-radius..=radius).map(|i| (-0.5 * (i as f32 / SIGMA).powi(2)).exp()).collect();
    let sum: f32 = kernel.iter().sum();
    let idx = |x: usize, y: usize, z: usize| (z * N + y) * N + x;
    for axis in 0..3 {
        let src = v.clone();
        for z in 0..N {
            for y in 0..N {
                for x in 0..N {
                    let mut acc = 0.0;
                    for (k, w) in kernel.iter().enumerate() {
                        let o = (k as isize - radius).rem_euclid(N as isize) as usize;
                        let (sx, sy, sz) = match axis {
                            0 => ((x + o) % N, y, z),
                            1 => (x, (y + o) % N, z),
                            _ => (x, y, (z + o) % N),
                        };
                        acc += w * src[idx(sx, sy, sz)];
                    }
                    v[idx(x, y, z)] = acc / sum;
                }
            }
        }
    }
    let (lo, hi) = v.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    let bytes: Vec<u8> = v.iter().map(|x| ((x - lo) / (hi - lo) * 255.0) as u8).collect();

    let tex = gl.create_texture().map_err(|e| anyhow!(e))?;
    gl.bind_texture(glow::TEXTURE_3D, Some(tex));
    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
    gl.tex_image_3d(
        glow::TEXTURE_3D, 0, glow::R8 as i32, N as i32, N as i32, N as i32, 0, glow::RED, glow::UNSIGNED_BYTE,
        glow::PixelUnpackData::Slice(Some(&bytes)),
    );
    for (param, value) in [
        (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
        (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
        (glow::TEXTURE_WRAP_S, glow::REPEAT),
        (glow::TEXTURE_WRAP_T, glow::REPEAT),
        (glow::TEXTURE_WRAP_R, glow::REPEAT),
    ] {
        gl.tex_parameter_i32(glow::TEXTURE_3D, param, value as i32);
    }
    Ok(tex)
}

pub fn save_png(path: &Path, (w, h): (u32, u32), rgb: &[u8]) -> Result<()> {
    let mut encoder = png::Encoder::new(File::create(path)?, w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgb)?;
    Ok(())
}
