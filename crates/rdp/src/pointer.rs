//! Slow-path pointer updates (MS-RDPBCGR 2.2.9.1.1.4).
use crate::{Result, invalid, wire::Reader};
use std::sync::Arc;

pub const CACHE_SIZE: usize = 25;

#[derive(Clone, Debug, Default)]
pub enum Pointer {
    #[default]
    Default,
    Hidden,
    Image(Arc<PointerImage>),
}

#[derive(Debug)]
pub struct PointerImage {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    /// Top-down BGRA, ready for presentation. Inverted monochrome pixels use
    /// opaque white on platforms whose cursor API cannot express screen XOR.
    pub bgra: Vec<u8>,
    /// Top-down, one byte per pixel; retains the original screen-XOR operation.
    pub invert: Vec<bool>,
}

pub(crate) struct Decoder {
    cache: Vec<Option<Arc<PointerImage>>>,
}
impl Default for Decoder {
    fn default() -> Self {
        Self {
            cache: vec![None; CACHE_SIZE],
        }
    }
}
impl Decoder {
    pub fn update(&mut self, bytes: &[u8]) -> Result<Option<Pointer>> {
        let mut r = Reader::new(bytes);
        let kind = r.u16()?;
        r.u16()?; // pad2Octets
        let pointer = match kind {
            1 => match r.u32()? {
                0 => Pointer::Hidden,
                0x7f00 => Pointer::Default,
                _ => return Err(invalid("system pointer type")),
            },
            3 => {
                r.take(4)?;
                return Ok(None);
            } // server pointer position
            6 | 8 => {
                let bpp = if kind == 8 { r.u16()? } else { 24 };
                let index = r.u16()? as usize;
                if index >= CACHE_SIZE {
                    return Err(invalid("pointer cache index"));
                }
                let hot_x = r.u16()?;
                let hot_y = r.u16()?;
                let width = r.u16()?;
                let height = r.u16()?;
                let and_len = r.u16()? as usize;
                let xor_len = r.u16()? as usize;
                // No large-pointer capability is advertised.
                if width == 0
                    || height == 0
                    || width > 32
                    || height > 32
                    || hot_x >= width
                    || hot_y >= height
                    || ![1, 15, 16, 24, 32].contains(&bpp)
                {
                    return Err(invalid("pointer dimensions, hotspot or depth"));
                }
                let w = width as usize;
                let h = height as usize;
                let storage_bpp = if bpp == 15 { 16 } else { bpp as usize };
                let xor_stride = (w * storage_bpp).div_ceil(16) * 2;
                let and_stride = w.div_ceil(16) * 2;
                if xor_len != xor_stride * h || and_len != and_stride * h {
                    return Err(invalid("pointer mask length"));
                }
                let xor = r.take(xor_len)?;
                let and = r.take(and_len)?;
                let mut bgra = vec![0; w * h * 4];
                let mut invert = vec![false; w * h];
                let alpha = bpp == 32 && xor.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
                for y in 0..h {
                    for x in 0..w {
                        let row = (h - 1 - y) * xor_stride;
                        let mut color = match bpp {
                            1 => {
                                let v = if xor[row + x / 8] & (0x80 >> (x % 8)) != 0 {
                                    255
                                } else {
                                    0
                                };
                                [v, v, v, 255]
                            }
                            15 | 16 => {
                                let pos = row + x * 2;
                                let v = u16::from_le_bytes([xor[pos], xor[pos + 1]]) as u32;
                                let (g, red) = if bpp == 16 {
                                    (((v >> 5) & 63) * 255 / 63, ((v >> 11) & 31) * 255 / 31)
                                } else {
                                    (((v >> 5) & 31) * 255 / 31, ((v >> 10) & 31) * 255 / 31)
                                };
                                [((v & 31) * 255 / 31) as u8, g as u8, red as u8, 255]
                            }
                            _ => {
                                let pos = row + x * (bpp as usize / 8);
                                [
                                    xor[pos],
                                    xor[pos + 1],
                                    xor[pos + 2],
                                    if alpha { xor[pos + 3] } else { 255 },
                                ]
                            }
                        };
                        if !alpha && and[(h - 1 - y) * and_stride + x / 8] & (0x80 >> (x % 8)) != 0
                        {
                            if color[..3] == [0, 0, 0] {
                                color[3] = 0;
                            } else {
                                invert[y * w + x] = true;
                            }
                        }
                        bgra[(y * w + x) * 4..(y * w + x + 1) * 4].copy_from_slice(&color);
                    }
                }
                let image = Arc::new(PointerImage {
                    width,
                    height,
                    hot_x,
                    hot_y,
                    bgra,
                    invert,
                });
                self.cache[index] = Some(image.clone());
                Pointer::Image(image)
            }
            7 => {
                let index = r.u16()? as usize;
                Pointer::Image(
                    self.cache
                        .get(index)
                        .and_then(Clone::clone)
                        .ok_or_else(|| invalid("uncached pointer"))?,
                )
            }
            _ => return Err(invalid("pointer update type")),
        };
        Ok(Some(pointer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn color() -> Vec<u8> {
        // Two rows, two pixels; XOR and AND scanlines are padded to 16 bits.
        let words: [u16; 9] = [6, 0, 2, 1, 0, 2, 2, 4, 12];
        let mut b: Vec<_> = words.into_iter().flat_map(u16::to_le_bytes).collect();
        b.extend([255, 255, 255, 0, 0, 0, 1, 2, 3, 4, 5, 6]);
        b.extend([0x80, 0, 0x40, 0]);
        b
    }
    #[test]
    fn bottom_up_masks_hotspot_and_cached_shape() {
        let mut decoder = Decoder::default();
        let Pointer::Image(image) = decoder.update(&color()).unwrap().unwrap() else {
            panic!()
        };
        assert_eq!((image.hot_x, image.hot_y), (1, 0));
        assert_eq!(&image.bgra[..4], &[1, 2, 3, 255]);
        assert_eq!(&image.bgra[8..12], &[255, 255, 255, 255]);
        assert!(image.invert[2]);
        let Pointer::Image(cached) = decoder.update(&[7, 0, 0, 0, 2, 0]).unwrap().unwrap() else {
            panic!()
        };
        assert!(Arc::ptr_eq(&image, &cached));
        assert!(matches!(
            decoder.update(&[1, 0, 0, 0, 0, 0, 0, 0]).unwrap(),
            Some(Pointer::Hidden)
        ));
        assert!(matches!(
            decoder.update(&[1, 0, 0, 0, 0, 0x7f, 0, 0]).unwrap(),
            Some(Pointer::Default)
        ));
    }
    #[test]
    fn rejects_invalid_and_truncated_updates() {
        for n in 0..color().len() {
            assert!(Decoder::default().update(&color()[..n]).is_err());
        }
        assert!(Decoder::default().update(&[7, 0, 0, 0, 24, 0]).is_err());
        for (offset, value) in [(4, 25), (6, 2), (10, 33), (14, 0)] {
            let mut b = color();
            b[offset] = value;
            assert!(Decoder::default().update(&b).is_err());
        }
    }
    #[test]
    fn new_pointer_alpha_and_monochrome_padding() {
        let mut packet: Vec<u8> = [8u16, 0, 32, 0, 0, 0, 1, 1, 2, 4]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        packet.extend([20, 40, 60, 128, 0x80, 0]);
        let Pointer::Image(image) = Decoder::default().update(&packet).unwrap().unwrap() else {
            panic!()
        };
        // An alpha cursor uses its alpha channel even if the AND mask is set.
        assert_eq!(image.bgra, [20, 40, 60, 128]);
        assert!(!image.invert[0]);
        let mut packet: Vec<u8> = [8u16, 0, 1, 0, 0, 0, 3, 1, 2, 2]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        packet.extend([0xa0, 0, 0x60, 0]);
        let Pointer::Image(image) = Decoder::default().update(&packet).unwrap().unwrap() else {
            panic!()
        };
        assert_eq!(
            image.bgra,
            [255, 255, 255, 255, 0, 0, 0, 0, 255, 255, 255, 255]
        );
        assert_eq!(image.invert, [false, false, true]);
    }
}
