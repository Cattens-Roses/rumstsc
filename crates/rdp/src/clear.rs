//! ClearCodec layers and bounded in-memory glyph/V-Bar dictionaries.
use crate::{Result, bitmap::Frame, invalid, wire::Reader};
use std::collections::BTreeMap;
type Pixel = [u8; 4];
#[derive(Default)]
pub(crate) struct Clear {
    sequence: u8,
    glyphs: BTreeMap<u16, Vec<u8>>,
    bars: BTreeMap<u16, Vec<Pixel>>,
    short: BTreeMap<u16, Vec<Pixel>>,
    bar_cursor: u16,
    short_cursor: u16,
}
fn pixel(r: &mut Reader<'_>) -> Result<Pixel> {
    let p = r.take(3)?;
    Ok([p[0], p[1], p[2], 255])
}
fn run(r: &mut Reader<'_>) -> Result<usize> {
    let n = r.u8()? as usize;
    if n != 255 {
        return Ok(n);
    }
    let n = r.u16()? as usize;
    if n != 65535 {
        return Ok(n);
    }
    Ok(r.u32()? as usize)
}
fn put(frame: &mut Frame, x: usize, y: usize, color: Pixel) {
    let i = (y * frame.width as usize + x) * 4;
    frame.bgra[i..i + 4].copy_from_slice(&color);
}
impl Clear {
    pub fn decode(&mut self, input: &[u8], w: u16, h: u16) -> Result<Frame> {
        let mut frame = Frame::new(w, h)?;
        let area = w as usize * h as usize;
        let mut r = Reader::new(input);
        let flags = r.u8()?;
        let sequence = r.u8()?;
        if flags & !7 != 0 || sequence != self.sequence || flags & 2 != 0 && flags & 1 == 0 {
            return Err(invalid("ClearCodec header/sequence"));
        }
        if flags & 4 != 0 {
            self.bar_cursor = 0;
            self.short_cursor = 0;
        }
        let glyph = if flags & 1 != 0 {
            let index = r.u16()?;
            if index >= 4000 || area > 1024 {
                return Err(invalid("ClearCodec glyph limits"));
            }
            Some(index)
        } else {
            None
        };
        if flags & 2 != 0 {
            let pixels = self
                .glyphs
                .get(&glyph.unwrap())
                .ok_or_else(|| invalid("ClearCodec missing glyph"))?;
            if pixels.len() != frame.bgra.len() || r.remaining() != 0 {
                return Err(invalid("ClearCodec glyph dimensions"));
            }
            frame.bgra.copy_from_slice(pixels);
            self.sequence = self.sequence.wrapping_add(1);
            return Ok(frame);
        }
        let residual = r.u32()? as usize;
        let bands = r.u32()? as usize;
        let subcodecs = r.u32()? as usize;
        let mut residual = Reader::new(r.take(residual)?);
        let mut cursor = 0;
        while residual.remaining() > 0 {
            let color = pixel(&mut residual)?;
            let count = run(&mut residual)?;
            if count == 0 || count > area - cursor {
                return Err(invalid("ClearCodec residual run"));
            }
            for p in frame.bgra[cursor * 4..(cursor + count) * 4]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
            {
                p.copy_from_slice(&color);
            }
            cursor += count;
        }
        let mut bands = Reader::new(r.take(bands)?);
        while bands.remaining() > 0 {
            let xs = bands.u16()? as usize;
            let xe = bands.u16()? as usize;
            let ys = bands.u16()? as usize;
            let ye = bands.u16()? as usize;
            let background = pixel(&mut bands)?;
            if xs > xe || ys > ye || xe >= w as usize || ye >= h as usize || ye - ys >= 52 {
                return Err(invalid("ClearCodec band bounds"));
            }
            let height = ye - ys + 1;
            for x in xs..=xe {
                let header = bands.u16()?;
                let bar = if header & 0x8000 != 0 {
                    self.bars
                        .get(&(header & 0x7fff))
                        .ok_or_else(|| invalid("ClearCodec missing V-Bar"))?
                        .clone()
                } else {
                    let (on, short) = if header & 0xc000 == 0x4000 {
                        let on = bands.u8()? as usize;
                        let short = self
                            .short
                            .get(&(header & 0x3fff))
                            .ok_or_else(|| invalid("ClearCodec missing short V-Bar"))?
                            .clone();
                        (on, short)
                    } else {
                        let on = (header & 255) as usize;
                        let off = (header >> 8) as usize;
                        if off < on || off > height {
                            return Err(invalid("ClearCodec short V-Bar bounds"));
                        }
                        let mut short = Vec::with_capacity(off - on);
                        for _ in on..off {
                            short.push(pixel(&mut bands)?);
                        }
                        self.short.insert(self.short_cursor, short.clone());
                        self.short_cursor = (self.short_cursor + 1) & 0x3fff;
                        (on, short)
                    };
                    if on > height || short.len() > height - on {
                        return Err(invalid("ClearCodec V-Bar bounds"));
                    }
                    let mut bar = vec![background; height];
                    bar[on..on + short.len()].copy_from_slice(&short);
                    self.bars.insert(self.bar_cursor, bar.clone());
                    self.bar_cursor = (self.bar_cursor + 1) & 0x7fff;
                    bar
                };
                if bar.len() != height {
                    return Err(invalid("ClearCodec cached V-Bar height"));
                }
                for (y, color) in bar.into_iter().enumerate() {
                    put(&mut frame, x, ys + y, color);
                }
            }
        }
        let mut sub = Reader::new(r.take(subcodecs)?);
        while sub.remaining() > 0 {
            let x = sub.u16()?;
            let y = sub.u16()?;
            let width = sub.u16()?;
            let height = sub.u16()?;
            let size = sub.u32()? as usize;
            let codec = sub.u8()?;
            if width == 0
                || height == 0
                || x as u32 + width as u32 > w as u32
                || y as u32 + height as u32 > h as u32
                || size > width as usize * height as usize * 3
            {
                return Err(invalid("ClearCodec subcodec bounds"));
            }
            let data = sub.take(size)?;
            let source = match codec {
                0 => {
                    if data.len() != width as usize * height as usize * 3 {
                        return Err(invalid("ClearCodec raw subcodec length"));
                    }
                    let mut source = Frame::new(width, height)?;
                    for (dst, src) in source
                        .bgra
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(data.as_chunks::<3>().0.iter())
                    {
                        dst.copy_from_slice(&[src[0], src[1], src[2], 255]);
                    }
                    source
                }
                1 => crate::nsc::decode(data, width, height)?,
                2 => rlex(data, width, height)?,
                _ => return Err(invalid("ClearCodec subcodec identifier")),
            };
            frame.memory_blt(
                &source,
                [x as i16, y as i16, width as i16, height as i16, 0, 0],
                0xcc,
                None,
            )?;
        }
        if r.remaining() != 0 {
            return Err(invalid("ClearCodec payload length"));
        }
        if let Some(index) = glyph {
            self.glyphs.insert(index, frame.bgra.clone());
        }
        self.sequence = self.sequence.wrapping_add(1);
        Ok(frame)
    }
}
fn rlex(input: &[u8], w: u16, h: u16) -> Result<Frame> {
    let mut r = Reader::new(input);
    let count = r.u8()? as usize;
    if count == 0 || count > 127 {
        return Err(invalid("ClearCodec palette count"));
    }
    let bits = (usize::BITS - (count - 1).leading_zeros()) as usize;
    let mask = (1 << bits) - 1;
    let mut palette = Vec::with_capacity(count);
    for _ in 0..count {
        palette.push(pixel(&mut r)?);
    }
    let mut frame = Frame::new(w, h)?;
    let mut position = 0;
    let area = w as usize * h as usize;
    while r.remaining() > 0 {
        let segment = r.u8()? as usize;
        let stop = segment & mask;
        let depth = segment >> bits;
        let length = run(&mut r)?;
        if stop >= count
            || depth > stop
            || length > area - position
            || depth + 1 > area - position - length
        {
            return Err(invalid("ClearCodec palette segment"));
        }
        let start = stop - depth;
        for _ in 0..length {
            frame.bgra[position * 4..position * 4 + 4].copy_from_slice(&palette[start]);
            position += 1;
        }
        for color in &palette[start..=stop] {
            frame.bgra[position * 4..position * 4 + 4].copy_from_slice(color);
            position += 1;
        }
    }
    if position != area {
        return Err(invalid("ClearCodec palette pixel count"));
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn composite(flags: u8, sequence: u8, residual: &[u8], bands: &[u8], sub: &[u8]) -> Vec<u8> {
        let mut p = vec![flags, sequence];
        if flags & 1 != 0 {
            p.extend([7, 0]);
        }
        for n in [residual.len(), bands.len(), sub.len()] {
            p.extend((n as u32).to_le_bytes());
        }
        p.extend(residual);
        p.extend(bands);
        p.extend(sub);
        p
    }
    #[test]
    fn residual_glyph_shape_reuse_and_sequence() {
        let mut c = Clear::default();
        let f = c
            .decode(&composite(1, 0, &[1, 2, 3, 4], &[], &[]), 2, 2)
            .unwrap();
        assert_eq!(f.bgra, [1, 2, 3, 255].repeat(4));
        let f = c.decode(&[3, 1, 7, 0], 4, 1).unwrap();
        assert_eq!(f.bgra, [1, 2, 3, 255].repeat(4));
        assert!(c.decode(&[3, 2, 7, 0], 3, 1).is_err());
        assert!(Clear::default().decode(&[3, 0, 7, 0], 4, 1).is_err());
        assert!(
            Clear::default()
                .decode(&composite(0, 0, &[1, 2, 3, 5], &[], &[]), 2, 2)
                .is_err()
        );
    }
    #[test]
    fn bars_short_hits_and_full_hits() {
        let mut bands = vec![];
        for n in [0u16, 2, 0, 1] {
            bands.extend(n.to_le_bytes());
        }
        bands.extend([9, 8, 7]);
        bands.extend([0, 2, 1, 2, 3, 4, 5, 6]); // raw short V-Bar, two pixels
        bands.extend([0, 0x40, 0]); // reuse short slot zero, y-on zero
        bands.extend([0, 0x80]); // reuse full V-Bar slot zero
        let f = Clear::default()
            .decode(&composite(4, 0, &[], &bands, &[]), 3, 2)
            .unwrap();
        assert_eq!(
            f.bgra,
            [
                1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3, 255, 4, 5, 6, 255, 4, 5, 6, 255, 4, 5, 6, 255
            ]
        );
    }
    #[test]
    fn rlex_runs_suites_and_raw_subrectangles() {
        let f = rlex(&[2, 1, 2, 3, 4, 5, 6, 3, 1], 3, 1).unwrap();
        assert_eq!(f.bgra, [1, 2, 3, 255, 1, 2, 3, 255, 4, 5, 6, 255]);
        let mut sub = vec![];
        for n in [1u16, 0, 1, 1] {
            sub.extend(n.to_le_bytes());
        }
        sub.extend(3u32.to_le_bytes());
        sub.extend([0, 20, 30, 40]);
        let f = Clear::default()
            .decode(&composite(0, 0, &[0, 0, 0, 2], &[], &sub), 2, 1)
            .unwrap();
        assert_eq!(f.bgra, [0, 0, 0, 255, 20, 30, 40, 255]);
    }
}
