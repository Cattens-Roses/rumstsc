//! RDP6 planar color/RLE, used by the graphics pipeline and 32-bit bitmaps.
use crate::{Result, bitmap::Frame, invalid, wire::Reader};
fn plane(r: &mut Reader<'_>, w: usize, h: usize, rle: bool) -> Result<Vec<u8>> {
    if !rle {
        return Ok(r.take(w * h)?.to_vec());
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let mut x = 0;
        let mut last = 0u8;
        while x < w {
            let control = r.u8()?;
            let mut raw = (control >> 4) as usize;
            let mut run = (control & 15) as usize;
            if run == 1 {
                run = raw + 16;
                raw = 0;
            } else if run == 2 {
                run = raw + 32;
                raw = 0;
            }
            if raw + run == 0 || raw + run > w - x {
                return Err(invalid("planar RLE row bounds"));
            }
            for _ in 0..raw {
                last = r.u8()?;
                let delta = if last & 1 != 0 {
                    0u8.wrapping_sub((last >> 1) + 1)
                } else {
                    last >> 1
                };
                out[y * w + x] = if y == 0 {
                    last
                } else {
                    out[(y - 1) * w + x].wrapping_add(delta)
                };
                x += 1;
            }
            for _ in 0..run {
                let delta = if last & 1 != 0 {
                    0u8.wrapping_sub((last >> 1) + 1)
                } else {
                    last >> 1
                };
                out[y * w + x] = if y == 0 {
                    last
                } else {
                    out[(y - 1) * w + x].wrapping_add(delta)
                };
                x += 1;
            }
        }
    }
    Ok(out)
}
pub(crate) fn decode(input: &[u8], width: u16, height: u16, flip: bool) -> Result<Frame> {
    let mut frame = Frame::new(width, height)?;
    let w = width as usize;
    let h = height as usize;
    let mut r = Reader::new(input);
    let format = r.u8()?;
    let loss = format & 7;
    let sub = format & 8 != 0;
    let rle = format & 16 != 0;
    if format & 0xc0 != 0 || sub && loss == 0 {
        return Err(invalid("planar format"));
    }
    let alpha = if format & 32 == 0 {
        plane(&mut r, w, h, rle)?
    } else {
        vec![255; w * h]
    };
    let first = plane(&mut r, w, h, rle)?;
    let cw = if sub { w.div_ceil(2) } else { w };
    let ch = if sub { h.div_ceil(2) } else { h };
    let second = plane(&mut r, cw, ch, rle)?;
    let third = plane(&mut r, cw, ch, rle)?;
    if !rle && r.remaining() == 1 {
        r.u8()?;
    }
    if r.remaining() != 0 {
        return Err(invalid("planar trailing data"));
    }
    for y in 0..h {
        for x in 0..w {
            let src = y * w + x;
            let dst = ((if flip { h - 1 - y } else { y }) * w + x) * 4;
            let p = &mut frame.bgra[dst..dst + 4];
            if loss == 0 {
                p.copy_from_slice(&[third[src], second[src], first[src], alpha[src]]);
            } else {
                let ci = if sub { (y / 2) * cw + x / 2 } else { src };
                let co = crate::nsc::chroma(second[ci], loss);
                let cg = crate::nsc::chroma(third[ci], loss);
                let l = first[src] as i32;
                p.copy_from_slice(&[
                    (l - (co >> 1) - (cg >> 1)).clamp(0, 255) as u8,
                    (l + (cg >> 1)).clamp(0, 255) as u8,
                    (l + (co >> 1) - (cg >> 1)).clamp(0, 255) as u8,
                    alpha[src],
                ]);
            }
        }
    }
    Ok(frame)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn microsoft_delta_plane_vector() {
        let encoded = [
            0x13, 0xff, 0x20, 0xfe, 0xfd, 0x60, 1, 0x7d, 0xf5, 0xc2, 0x9a, 0x38, 0x60, 1, 0x67,
            0x8b, 0xa3, 0x78, 0xaf,
        ];
        assert_eq!(
            plane(&mut Reader::new(&encoded), 6, 3, true).unwrap(),
            [
                255, 255, 255, 255, 254, 253, 254, 192, 132, 96, 75, 25, 253, 140, 62, 14, 135, 193
            ]
        );
    }
    #[test]
    fn raw_planes_flip_and_bounds() {
        let p = decode(&[0x20, 10, 20, 30, 40, 50, 60], 1, 2, true).unwrap();
        assert_eq!(p.bgra, [60, 40, 20, 255, 50, 30, 10, 255]);
        assert!(decode(&[0x30, 0xff], 2, 2, false).is_err());
    }
}
