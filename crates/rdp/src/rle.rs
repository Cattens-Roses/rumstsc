//! MS-RDPBCGR 3.1.9 interleaved bitmap RLE, independent of an RDP library.
use crate::{Result, invalid, wire::Reader};

/// Return tightly packed pixels in RDP bottom-up scanline order.
pub fn decode(data: &[u8], width: usize, height: usize, bytes: usize) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width > 4096 || height > 2160 || !(1..=3).contains(&bytes) {
        return Err(invalid("RLE dimensions/pixel size"));
    }
    let count = width * height;
    let mut pixels: Vec<u32> = Vec::with_capacity(count);
    let mut r = Reader::new(data);
    let white = (1u32 << (bytes * 8)) - 1;
    let mut foreground = white;
    let mut insert = false;
    let mut first = true;
    while r.remaining() != 0 {
        if first && pixels.len() >= width {
            first = false;
            insert = false;
        }
        let header = r.u8()?;
        let code = if header < 0xc0 {
            header >> 5
        } else if header < 0xf0 {
            header >> 4
        } else {
            header
        };
        let mask = matches!(code, 2 | 0xd);
        let mut n = match code {
            0..=4 => {
                let n = (header & 31) as usize;
                if n == 0 {
                    r.u8()? as usize + if mask { 1 } else { 32 }
                } else if mask {
                    n * 8
                } else {
                    n
                }
            }
            0xc..=0xe => {
                let n = (header & 15) as usize;
                if n == 0 {
                    r.u8()? as usize + if mask { 1 } else { 16 }
                } else if mask {
                    n * 8
                } else {
                    n
                }
            }
            0xf0..=0xf4 | 0xf6..=0xf8 => r.u16()? as usize,
            0xf9 | 0xfa => 8,
            0xfd | 0xfe => 1,
            _ => return Err(invalid("unknown RLE opcode")),
        };
        if n == 0 {
            return Err(invalid("zero RLE run"));
        }
        let output = if matches!(code, 0xe | 0xf8) { n * 2 } else { n };
        if output > count - pixels.len() {
            return Err(invalid("RLE output exceeds bitmap"));
        }
        let background = |p: &Vec<u32>| {
            if p.len() >= width {
                p[p.len() - width]
            } else {
                0
            }
        };
        match code {
            0 | 0xf0 => {
                if insert {
                    let value = background(&pixels) ^ foreground;
                    pixels.push(value);
                    n -= 1;
                }
                for _ in 0..n {
                    let value = background(&pixels);
                    pixels.push(value);
                }
                insert = true;
                continue;
            }
            1 | 0xc | 0xf1 | 0xf6 => {
                if matches!(code, 0xc | 0xf6) {
                    foreground = read_pixel(&mut r, bytes)?;
                }
                for _ in 0..n {
                    let value = background(&pixels) ^ foreground;
                    pixels.push(value);
                }
            }
            2 | 0xd | 0xf2 | 0xf7 | 0xf9 | 0xfa => {
                if matches!(code, 0xd | 0xf7) {
                    foreground = read_pixel(&mut r, bytes)?;
                }
                let mut bits = 0;
                for i in 0..n {
                    if i % 8 == 0 {
                        bits = match code {
                            0xf9 => 3,
                            0xfa => 5,
                            _ => r.u8()?,
                        };
                    }
                    let value = background(&pixels) ^ if bits & 1 != 0 { foreground } else { 0 };
                    pixels.push(value);
                    bits >>= 1;
                }
            }
            3 | 0xf3 => {
                let value = read_pixel(&mut r, bytes)?;
                pixels.resize(pixels.len() + n, value);
            }
            4 | 0xf4 => {
                for _ in 0..n {
                    pixels.push(read_pixel(&mut r, bytes)?);
                }
            }
            0xe | 0xf8 => {
                let a = read_pixel(&mut r, bytes)?;
                let b = read_pixel(&mut r, bytes)?;
                for _ in 0..n {
                    pixels.extend([a, b]);
                }
            }
            0xfd => pixels.push(white),
            0xfe => pixels.push(0),
            _ => unreachable!(),
        }
        insert = false;
    }
    if pixels.len() != count {
        return Err(invalid("incomplete RLE bitmap"));
    }
    Ok(pixels
        .into_iter()
        .flat_map(|v| v.to_le_bytes().into_iter().take(bytes))
        .collect())
}
fn read_pixel(r: &mut Reader<'_>, bytes: usize) -> Result<u32> {
    let mut out = [0; 4];
    out[..bytes].copy_from_slice(r.take(bytes)?);
    Ok(u32::from_le_bytes(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_regular_and_lite_orders() {
        assert_eq!(
            decode(&[0x63, 1, 2, 3], 3, 1, 3).unwrap(),
            [1, 2, 3].repeat(3)
        );
        assert_eq!(decode(&[0x82, 1, 2, 3, 4], 2, 1, 2).unwrap(), [1, 2, 3, 4]);
        assert_eq!(decode(&[0xe2, 1, 2], 4, 1, 1).unwrap(), [1, 2, 1, 2]);
        assert_eq!(decode(&[0xc2, 7, 2], 2, 2, 1).unwrap(), [7, 7, 7, 7]);
        assert_eq!(
            decode(&[0x41, 0x55], 8, 1, 1).unwrap(),
            [255, 0, 255, 0, 255, 0, 255, 0]
        );
        assert_eq!(
            decode(&[0xd1, 7, 0x81], 8, 1, 1).unwrap(),
            [7, 0, 0, 0, 0, 0, 0, 7]
        );
    }
    #[test]
    fn background_insertion_and_first_row_reset() {
        assert_eq!(decode(&[2, 2], 4, 1, 1).unwrap(), [0, 0, 255, 0]);
        assert_eq!(decode(&[2, 2], 2, 2, 1).unwrap(), [0; 4]);
        assert_eq!(decode(&[0x62, 5, 0x22], 2, 2, 1).unwrap(), [5, 5, 250, 250]);
    }
    #[test]
    fn mega_and_special() {
        assert_eq!(decode(&[0xf3, 2, 0, 9], 2, 1, 1).unwrap(), [9, 9]);
        assert_eq!(decode(&[0xf6, 2, 0, 7], 2, 1, 1).unwrap(), [7, 7]);
        assert_eq!(
            decode(&[0xf9], 8, 1, 1).unwrap(),
            [255, 255, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            decode(&[0xfa], 8, 1, 1).unwrap(),
            [255, 0, 255, 0, 0, 0, 0, 0]
        );
        assert_eq!(decode(&[0xfd, 0xfe], 2, 1, 1).unwrap(), [255, 0]);
    }
    #[test]
    fn malformed_is_bounded() {
        for p in [&[0xf0][..], &[0x61], &[0xff], &[0x22], &[0xf0, 0, 0], &[]] {
            assert!(decode(p, 1, 1, 3).is_err());
        }
        assert!(decode(&[0x61, 1, 2, 3, 0xfe], 1, 1, 3).is_err());
    }
}
