//! Untile the pinned SDL CPU framebuffer into tightly packed RGBA bytes.
//!
//! Primary implementation matching crispy-doom.elf's PS5_DrawTileThread:
//! https://github.com/ps5-payload-dev/SDL/blob/8c56053f13ca13a0c050de613706ff69eb615836/src/video/ps5/SDL_ps5video.c
//! Its SDL_ps5tilemap.inc contains a 512x128 u16 permutation, also present at
//! ELF virtual address 0x534170. The scalar bit permutation below was compared
//! against every one of those 65,536 entries. This is not a general PS5 tiler.
//! SDL uses ABGR8888 integer pixels, i.e. RGBA bytes on the little-endian guest.
//! Bytes are preserved, with no invented alpha, color conversion, or scaling.
use crate::Result;

fn dimensions(width: u32, height: u32) -> Result<(usize, usize)> {
    // Each 128-wide strip occupies a contiguous 16384-pixel block. Whole strips
    // are required for the old tiler's 128*width row-band stride not to alias.
    // 1920 is deliberately accepted without rounding it up to a 512-wide tile.
    if width == 0 || width > 1920 || width % 128 != 0 || height == 0 || height > 1080 {
        return Err(
            "SDL tiled framebuffer requires width128..1920 in steps128 and height1..1080".into(),
        );
    }
    Ok((width as usize, height as usize))
}

fn tile_offset(x: usize, y: usize) -> usize {
    let bit = |value: usize, n: usize| (value >> n) & 1;
    bit(x, 0)
        | bit(x, 1) << 1
        | bit(y, 0) << 2
        | bit(y, 1) << 3
        | bit(y, 2) << 4
        | bit(x, 2) << 5
        | (bit(x, 3) ^ bit(y, 3)) << 6
        | (bit(x, 4) ^ bit(y, 4)) << 7
        | (bit(x, 6) ^ bit(y, 5)) << 8
        | (bit(x, 5) ^ bit(y, 6)) << 9
        | bit(y, 3) << 10
        | bit(x, 4) << 11
        | bit(y, 6) << 12
        | bit(x, 6) << 13
        | bit(x, 7) << 14
        | bit(x, 8) << 15
}
fn index(x: usize, y: usize, width: usize) -> usize {
    (x / 512) * 65536 + (y / 128) * (128 * width) + tile_offset(x % 512, y % 128)
}
fn required(width: usize, height: usize) -> usize {
    // Earlier complete bands/strips cannot exceed the final strip's address
    // block. Scan at most128*128 entries in its final (possibly partial) band.
    let x0 = ((width - 1) / 128) * 128;
    let y0 = ((height - 1) / 128) * 128;
    let mut maximum = 0;
    for y in y0..height {
        for x in x0..width {
            maximum = maximum.max(index(x, y, width));
        }
    }
    (maximum + 1) * 4
}

/// Minimum source length covering every visible pixel, including tiling gaps.
/// Width is the guest row-band stride; no implicit padded width is introduced.
pub fn required_bytes(width: u32, height: u32) -> Result<usize> {
    let (width, height) = dimensions(width, height)?;
    Ok(required(width, height))
}

/// Validate the complete source extent before allocating the output or reading
/// any source bytes. Caller owns/validates the guest mapping backing this slice.
pub fn untile_rgba(source: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let (width, height) = dimensions(width, height)?;
    if source.len() < required(width, height) {
        return Err("Truncated SDL tiled framebuffer".into());
    }
    let mut output = vec![0; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let from = index(x, y, width) * 4;
            let to = (y * width + x) * 4;
            output[to..to + 4].copy_from_slice(&source[from..from + 4]);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dimensions_and_complete_source_are_checked() {
        for (width, height) in [
            (0, 1),
            (1, 129),
            (127, 1),
            (129, 1),
            (1921, 1),
            (2048, 1),
            (128, 0),
            (128, 1081),
        ] {
            assert!(required_bytes(width, height).is_err());
            assert!(untile_rgba(&[], width, height).is_err());
        }
        for (width, height) in [(128, 1), (512, 128), (640, 129), (1920, 1080)] {
            let bytes = required_bytes(width, height).unwrap();
            assert!(bytes <= 1920 * 1152 * 4);
            assert!(untile_rgba(&vec![0; bytes - 1], width, height).is_err());
            assert_eq!(
                untile_rgba(&vec![0; bytes], width, height).unwrap().len(),
                width as usize * height as usize * 4
            );
        }
    }
    #[test]
    fn full_tile_is_a_permutation_with_known_table_offsets() {
        let mut seen = [false; 65536];
        for y in 0..128 {
            for x in 0..512 {
                let i = tile_offset(x, y);
                assert!(!seen[i]);
                seen[i] = true;
            }
        }
        assert!(seen.iter().all(|&v| v));
        for (x, y, expected) in [
            (0, 0, 0),
            (4, 0, 32),
            (16, 0, 2176),
            (32, 0, 512),
            (128, 0, 16384),
            (256, 0, 32768),
            (0, 1, 4),
            (0, 8, 1088),
            (0, 16, 128),
        ] {
            assert_eq!(tile_offset(x, y), expected);
        }
    }
    #[test]
    fn visible_pixels_never_alias_and_required_extent_is_exact() {
        for width in [128, 384, 512, 640, 1920] {
            let height = 1080;
            let bound = required(width, height) / 4;
            let mut seen = vec![false; bound];
            let mut max = 0;
            for y in 0..height {
                for x in 0..width {
                    let i = index(x, y, width);
                    assert!(!seen[i]);
                    seen[i] = true;
                    max = max.max(i);
                }
            }
            assert_eq!(max + 1, bound);
        }
    }
    #[test]
    fn rgba_bars_cross_tile_and_band_boundaries_without_conversion() {
        let (width, height) = (640usize, 257usize);
        let mut source = vec![0xa5; required(width, height)];
        let mut expected = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let pixel = [(x / 128) as u8, (y / 128) as u8, x as u8, y as u8];
                let offset = index(x, y, width) * 4;
                source[offset..offset + 4].copy_from_slice(&pixel);
                expected.extend(pixel);
            }
        }
        assert_eq!(
            untile_rgba(&source, width as u32, height as u32).unwrap(),
            expected
        );
    }
}
