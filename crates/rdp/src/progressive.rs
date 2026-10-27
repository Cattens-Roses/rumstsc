//! Stateful RemoteFX Progressive, MS-RDPEGFX 2.2.4.2 / 3.3.8.2.
//! RLGR1, SRL, scalar quantization, inverse 5/3 DWT and YCbCr are implemented here.
use crate::{Result, bitmap::Frame, invalid, wire::Reader};
use std::collections::BTreeMap;
const N: usize = 4096;
pub(crate) const TILE_BUDGET: usize = (128 * 1024 * 1024) / (3 * N * 5 + 128);
type Quant = [u8; 10];
struct Bits<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Bits<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn read(&mut self, count: u8) -> Result<u32> {
        if count > 24 || self.position + count as usize > self.bytes.len() * 8 {
            return Err(invalid("progressive bitstream truncated"));
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1)
                | ((self.bytes[self.position / 8] >> (7 - self.position % 8)) & 1) as u32;
            self.position += 1;
        }
        Ok(value)
    }
}
fn gr(bits: &mut Bits<'_>, parameter: &mut i32) -> Result<i32> {
    let mut quotient = 0;
    while bits.read(1)? != 0 {
        quotient += 1;
        if quotient > 65535 {
            return Err(invalid("RLGR magnitude overflow"));
        }
    }
    let value = (quotient << (*parameter >> 3)) | bits.read((*parameter >> 3) as u8)? as i32;
    if quotient == 0 {
        *parameter = (*parameter - 2).max(0);
    } else if quotient > 1 {
        *parameter = (*parameter + quotient).min(80);
    }
    if value > 65535 {
        return Err(invalid("RLGR coefficient overflow"));
    }
    Ok(value)
}
fn rlgr(input: &[u8], count: usize) -> Result<Vec<i32>> {
    let mut bits = Bits::new(input);
    let (mut kp, mut krp) = (8i32, 8i32);
    let mut values = Vec::with_capacity(count);
    while values.len() < count {
        if kp >> 3 != 0 {
            while bits.read(1)? == 0 {
                let run = 1usize << (kp >> 3);
                values.resize((values.len() + run).min(count), 0);
                kp = (kp + 4).min(80);
                if values.len() == count {
                    return Ok(values);
                }
            }
            let run = bits.read((kp >> 3) as u8)? as usize;
            values.resize((values.len() + run).min(count), 0);
            if values.len() == count {
                break;
            }
            let negative = bits.read(1)? != 0;
            let magnitude = gr(&mut bits, &mut krp)? + 1;
            values.push(if negative { -magnitude } else { magnitude });
            kp = (kp - 6).max(0);
        } else {
            let value = gr(&mut bits, &mut krp)?;
            if value == 0 {
                kp = (kp + 3).min(80);
                values.push(0);
            } else {
                kp = (kp - 3).max(0);
                values.push(if value & 1 != 0 {
                    -((value + 1) >> 1)
                } else {
                    value >> 1
                });
            }
        }
    }
    Ok(values)
}
struct Srl<'a> {
    bits: Bits<'a>,
    kp: u8,
    zeros: usize,
    nonzero: bool,
}
impl<'a> Srl<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            bits: Bits::new(input),
            kp: 8,
            zeros: 0,
            nonzero: false,
        }
    }
    fn value(&mut self, magnitude_bits: u8) -> Result<i32> {
        if self.zeros > 0 {
            self.zeros -= 1;
            return Ok(0);
        }
        if !self.nonzero {
            let k = self.kp >> 3;
            if self.bits.read(1)? == 0 {
                self.zeros = (1usize << k) - 1;
                self.kp = (self.kp + 4).min(80);
                return Ok(0);
            }
            self.zeros = self.bits.read(k)? as usize;
            self.nonzero = true;
            if self.zeros > 0 {
                self.zeros -= 1;
                return Ok(0);
            }
        }
        self.nonzero = false;
        self.kp = self.kp.saturating_sub(6);
        let negative = self.bits.read(1)? != 0;
        let maximum = (1i32 << magnitude_bits) - 1;
        let mut magnitude = 1;
        while magnitude < maximum && self.bits.read(1)? == 0 {
            magnitude += 1;
        }
        Ok(if negative { -magnitude } else { magnitude })
    }
}
#[derive(Clone, Copy)]
struct Band {
    offset: usize,
    w: usize,
    h: usize,
    quant: usize,
}
fn bands(reduce: bool) -> Vec<Band> {
    let dims = if reduce {
        [64, 33, 17, 9]
    } else {
        [64, 32, 16, 8]
    };
    let mut result = vec![];
    let mut offset = 0;
    for level in 0..3 {
        let low = dims[level + 1];
        let high = dims[level] - low;
        for (w, h, quant) in [
            (high, low, 7 - level * 3),
            (low, high, 8 - level * 3),
            (high, high, 9 - level * 3),
        ] {
            result.push(Band {
                offset,
                w,
                h,
                quant,
            });
            offset += w * h;
        }
    }
    result.push(Band {
        offset,
        w: dims[3],
        h: dims[3],
        quant: 0,
    });
    result
}
fn inverse_line(low: &[i64], high: &[i64], length: usize) -> [i64; 65] {
    let padded = low.len() == high.len() + 2;
    let high_count = high.len() + usize::from(padded);
    let get_high = |i: usize| {
        if padded && i >= high.len() {
            0
        } else {
            high[i.min(high.len() - 1)]
        }
    };
    let mut out = [0; 65];
    for i in 0..low.len() {
        out[2 * i] = low[i] - ((get_high(i.saturating_sub(1)) + get_high(i) + 1) >> 1);
    }
    for i in 0..high_count {
        let next = (2 * i + 2).min(2 * (low.len() - 1));
        out[2 * i + 1] = 2 * get_high(i) + ((out[2 * i] + out[next]) >> 1);
    }
    debug_assert!(length <= low.len() + high_count);
    out
}
fn inverse(input: &[i32], layout: &[Band]) -> Vec<i64> {
    let coefficients: Vec<i64> = input.iter().map(|v| *v as i64).collect();
    let ll = layout[9];
    let mut low = coefficients[ll.offset..].to_vec();
    for level in (0..3).rev() {
        let hl = layout[level * 3];
        let lh = layout[level * 3 + 1];
        let hh = layout[level * 3 + 2];
        let width = hl.w + lh.w;
        let height = hl.h + lh.h;
        let mut rows = vec![0; width * height];
        for y in 0..hl.h {
            let line = inverse_line(
                &low[y * lh.w..(y + 1) * lh.w],
                &coefficients[hl.offset + y * hl.w..hl.offset + (y + 1) * hl.w],
                width,
            );
            rows[y * width..(y + 1) * width].copy_from_slice(&line[..width]);
        }
        for y in 0..lh.h {
            let line = inverse_line(
                &coefficients[lh.offset + y * lh.w..lh.offset + (y + 1) * lh.w],
                &coefficients[hh.offset + y * hh.w..hh.offset + (y + 1) * hh.w],
                width,
            );
            rows[(y + hl.h) * width..(y + hl.h + 1) * width].copy_from_slice(&line[..width]);
        }
        low = vec![0; width * height];
        for x in 0..width {
            let mut a = [0; 33];
            let mut b = [0; 32];
            for y in 0..hl.h {
                a[y] = rows[y * width + x];
            }
            for y in 0..lh.h {
                b[y] = rows[(y + hl.h) * width + x];
            }
            let line = inverse_line(&a[..hl.h], &b[..lh.h], height);
            for (y, &value) in line[..height].iter().enumerate() {
                low[y * width + x] = value;
            }
        }
    }
    low
}
fn quant(r: &mut Reader<'_>, progressive: bool) -> Result<Quant> {
    let mut q = [0; 10];
    for pair in q.as_chunks_mut::<2>().0.iter_mut() {
        let v = r.u8()?;
        pair[0] = v & 15;
        pair[1] = v >> 4;
    }
    if progressive && q.iter().any(|v| *v > 8) {
        return Err(invalid("progressive quantization range"));
    }
    Ok(q)
}
fn scaled(value: i32, shift: i32) -> Result<i32> {
    let result = if shift >= 0 {
        (value as i64) << shift
    } else {
        (value as i64) >> -shift
    };
    i32::try_from(result).map_err(|_| invalid("progressive coefficient range"))
}
fn add(target: &mut i32, value: i32) -> Result<()> {
    *target = target
        .checked_add(value)
        .ok_or_else(|| invalid("progressive reference overflow"))?;
    Ok(())
}
struct Tile {
    coeff: [Vec<i32>; 3],
    signs: [Vec<i8>; 3],
    bits: [Quant; 3],
    reduce: bool,
}
impl Tile {
    fn new(reduce: bool) -> Self {
        Self {
            coeff: std::array::from_fn(|_| vec![0; N]),
            signs: std::array::from_fn(|_| vec![0; N]),
            bits: [[0; 10]; 3],
            reduce,
        }
    }
    fn first(
        &mut self,
        component: usize,
        encoded: &[u8],
        q: Quant,
        p: Quant,
        difference: bool,
    ) -> Result<()> {
        self.first_layout(component, encoded, q, p, difference, &bands(self.reduce))
    }
    fn first_layout(
        &mut self,
        component: usize,
        encoded: &[u8],
        q: Quant,
        p: Quant,
        difference: bool,
        layout: &[Band],
    ) -> Result<()> {
        let mut decoded = rlgr(encoded, self.coeff[component].len())?;
        let ll = layout.last().unwrap().offset;
        for i in ll + 1..decoded.len() {
            decoded[i] = decoded[i]
                .checked_add(decoded[i - 1])
                .ok_or_else(|| invalid("progressive LL delta overflow"))?;
        }
        for band in layout {
            let index = band.quant;
            self.bits[component][index] = q[index] + p[index];
            for (i, &value) in decoded
                .iter()
                .enumerate()
                .skip(band.offset)
                .take(band.w * band.h)
            {
                self.signs[component][i] = value.signum() as i8;
                let delta = scaled(value, (q[index] + p[index]) as i32 - 1)?;
                if !difference {
                    self.coeff[component][i] = delta;
                } else {
                    add(&mut self.coeff[component][i], delta)?;
                }
            }
        }
        Ok(())
    }
    fn upgrade(
        &mut self,
        component: usize,
        srl: &[u8],
        raw: &[u8],
        q: Quant,
        p: Quant,
    ) -> Result<()> {
        self.upgrade_layout(component, srl, raw, q, p, &bands(self.reduce))
    }
    fn upgrade_layout(
        &mut self,
        component: usize,
        srl: &[u8],
        raw: &[u8],
        q: Quant,
        p: Quant,
        layout: &[Band],
    ) -> Result<()> {
        let mut srl = Srl::new(srl);
        let mut raw = Bits::new(raw);
        for band in layout {
            let total = q[band.quant] + p[band.quant];
            let old = self.bits[component][band.quant];
            if total > old {
                return Err(invalid("progressive upgrade decreases quality"));
            }
            let count = old - total;
            if count == 0 {
                continue;
            }
            if count > 23 {
                return Err(invalid("progressive upgrade bit width"));
            }
            for i in band.offset..band.offset + band.w * band.h {
                let sign = self.signs[component][i];
                let value = if band.quant == 0 || sign != 0 {
                    raw.read(count)? as i32 * if sign < 0 && band.quant != 0 { -1 } else { 1 }
                } else {
                    let value = srl.value(count)?;
                    self.signs[component][i] = value.signum() as i8;
                    value
                };
                add(
                    &mut self.coeff[component][i],
                    scaled(value, total as i32 - 1)?,
                )?;
            }
            self.bits[component][band.quant] = total;
        }
        Ok(())
    }
    fn render(&self) -> Result<Frame> {
        let layout = bands(self.reduce);
        let channels: [Vec<i64>; 3] = std::array::from_fn(|c| inverse(&self.coeff[c], &layout));
        let mut frame = Frame::new(64, 64)?;
        for (i, pixel) in frame.bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let y = channels[0][i] + 128 * 32;
            let cb = channels[1][i];
            let cr = channels[2][i];
            pixel.copy_from_slice(&[
                ((y + ((115933 * cb) >> 16)) >> 5).clamp(0, 255) as u8,
                ((y - ((22527 * cb + 46792 * cr) >> 16)) >> 5).clamp(0, 255) as u8,
                ((y + ((91957 * cr) >> 16)) >> 5).clamp(0, 255) as u8,
                255,
            ]);
        }
        Ok(frame)
    }
}
#[derive(Default)]
pub(crate) struct Progressive {
    tiles: BTreeMap<(u16, u16), Tile>,
    in_frame: bool,
}
impl Progressive {
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }
    pub fn update(
        &mut self,
        input: &[u8],
        frame: &mut Frame,
        available: usize,
    ) -> Result<Vec<[u16; 4]>> {
        let mut r = Reader::new(input);
        let mut dirty = vec![];
        while r.remaining() > 0 {
            let kind = r.u16()?;
            let size = r.u32()? as usize;
            if size < 6 {
                return Err(invalid("progressive block length"));
            }
            let mut b = Reader::new(r.take(size - 6)?);
            match kind {
                0xccc0 => {
                    b.take(6)?;
                }
                0xccc1 => {
                    b.u32()?;
                    b.u16()?;
                    self.in_frame = true;
                }
                0xccc2 => self.in_frame = false,
                0xccc3 => {
                    b.u8()?;
                    if b.u16()? != 64 {
                        return Err(invalid("progressive tile size"));
                    }
                    b.u8()?;
                }
                0xccc4 if self.in_frame => dirty.extend(self.region(&mut b, frame, available)?),
                _ => {
                    b.rest();
                }
            }
            if b.remaining() != 0 {
                return Err(invalid("progressive block trailing data"));
            }
        }
        Ok(dirty)
    }
    fn region(
        &mut self,
        r: &mut Reader<'_>,
        frame: &mut Frame,
        available: usize,
    ) -> Result<Vec<[u16; 4]>> {
        if r.u8()? != 64 {
            return Err(invalid("progressive region tile size"));
        }
        let rect_count = r.u16()? as usize;
        let q_count = r.u8()? as usize;
        let p_count = r.u8()? as usize;
        let flags = r.u8()?;
        let tile_count = r.u16()? as usize;
        let bytes = r.u32()? as usize;
        if rect_count == 0
            || rect_count > 4096
            || q_count > 7
            || flags & !1 != 0
            || tile_count > 8192
        {
            return Err(invalid("progressive region limits"));
        }
        let reduce = flags & 1 != 0;
        let mut rects = Vec::with_capacity(rect_count);
        let mut area = 0u64;
        for _ in 0..rect_count {
            let x = r.u16()?;
            let y = r.u16()?;
            let w = r.u16()?;
            let h = r.u16()?;
            if w == 0
                || h == 0
                || x as u32 + w as u32 > frame.width as u32
                || y as u32 + h as u32 > frame.height as u32
            {
                return Err(invalid("progressive region bounds"));
            }
            area += w as u64 * h as u64;
            if area > frame.width as u64 * frame.height as u64 * 8 {
                return Err(invalid("progressive region work limit"));
            }
            rects.push([x, y, x + w, y + h]);
        }
        let mut qs = vec![];
        for _ in 0..q_count {
            qs.push(quant(r, false)?);
        }
        let mut ps = vec![];
        for _ in 0..p_count {
            r.u8()?;
            ps.push([quant(r, true)?, quant(r, true)?, quant(r, true)?]);
        }
        let mut tiles = Reader::new(r.take(bytes)?);
        let initial = self.tiles.len();
        for _ in 0..tile_count {
            let kind = tiles.u16()?;
            let length = tiles.u32()? as usize;
            if length < 6 {
                return Err(invalid("progressive tile block length"));
            }
            let mut t = Reader::new(tiles.take(length - 6)?);
            if ![0xccc5, 0xccc6, 0xccc7].contains(&kind) {
                continue;
            }
            let indexes = [t.u8()? as usize, t.u8()? as usize, t.u8()? as usize];
            let q = indexes
                .map(|i| qs.get(i).copied())
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| invalid("progressive quant index"))?;
            let key = (t.u16()?, t.u16()?);
            if key.0 as u32 * 64 >= frame.width as u32 || key.1 as u32 * 64 >= frame.height as u32 {
                return Err(invalid("progressive tile coordinates"));
            }
            let difference = if kind != 0xccc7 {
                t.u8()? & 1 != 0
            } else {
                false
            };
            let quality = if kind != 0xccc5 { t.u8()? } else { 255 };
            let p = if quality == 255 {
                [[0; 10]; 3]
            } else {
                *ps.get(quality as usize)
                    .ok_or_else(|| invalid("progressive quality index"))?
            };
            if !self.tiles.contains_key(&key) {
                if kind == 0xccc7 || difference {
                    return Err(invalid("progressive reference tile missing"));
                }
                if self.tiles.len() - initial >= available {
                    return Err(invalid("progressive history memory limit"));
                }
                self.tiles.insert(key, Tile::new(reduce));
            }
            let tile = self.tiles.get_mut(&key).unwrap();
            if tile.reduce != reduce {
                return Err(invalid(
                    "progressive transform changed without context reset",
                ));
            }
            if kind == 0xccc7 {
                let lengths = [
                    t.u16()? as usize,
                    t.u16()? as usize,
                    t.u16()? as usize,
                    t.u16()? as usize,
                    t.u16()? as usize,
                    t.u16()? as usize,
                ];
                for c in 0..3 {
                    let srl = t.take(lengths[c * 2])?;
                    let raw = t.take(lengths[c * 2 + 1])?;
                    tile.upgrade(c, srl, raw, q[c], p[c])?;
                }
            } else {
                let lengths = [t.u16()? as usize, t.u16()? as usize, t.u16()? as usize];
                let tail = t.u16()? as usize;
                for c in 0..3 {
                    tile.first(c, t.take(lengths[c])?, q[c], p[c], difference)?;
                }
                t.take(tail)?;
            }
            if t.remaining() != 0 {
                return Err(invalid("progressive tile trailing data"));
            }
        }
        if tiles.remaining() != 0 {
            return Err(invalid("progressive tile count"));
        }
        let mut clips: BTreeMap<(u16, u16), Vec<[u16; 4]>> = BTreeMap::new();
        let mut intersections = 0;
        for &v in &rects {
            for ty in v[1] / 64..=(v[3] - 1) / 64 {
                for tx in v[0] / 64..=(v[2] - 1) / 64 {
                    intersections += 1;
                    if intersections > 32768 {
                        return Err(invalid("progressive clipping work limit"));
                    }
                    clips.entry((tx, ty)).or_default().push([
                        v[0].max(tx * 64),
                        v[1].max(ty * 64),
                        v[2].min(tx * 64 + 64),
                        v[3].min(ty * 64 + 64),
                    ]);
                }
            }
        }
        for (key, clips) in clips {
            let tile = self
                .tiles
                .get(&key)
                .ok_or_else(|| invalid("progressive region tile missing"))?
                .render()?;
            for clip in clips {
                frame.memory_blt(
                    &tile,
                    [
                        clip[0] as i16,
                        clip[1] as i16,
                        (clip[2] - clip[0]) as i16,
                        (clip[3] - clip[1]) as i16,
                        (clip[0] - key.0 * 64) as i16,
                        (clip[1] - key.1 * 64) as i16,
                    ],
                    0xcc,
                    None,
                )?;
            }
        }
        Ok(rects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn microsoft_rlgr_vector() {
        assert_eq!(
            rlgr(&[0xa8, 0x62, 0x6d, 0xff, 0xf7, 0], 14).unwrap(),
            [-2, 0, 0, 0, 0, 0, 0, 0, 0, 1, 3, 1, -7, 6]
        );
    }
    #[test]
    fn progressive_first_difference_and_all_upgrade_passes() {
        // MS-RDPEGFX 4.1.2 reference coefficients and published first-pass streams.
        // The second upgrade SRL stream is independently encoded from its coefficient table.
        let layout = [
            Band {
                offset: 0,
                w: 5,
                h: 1,
                quant: 7,
            },
            Band {
                offset: 5,
                w: 5,
                h: 1,
                quant: 8,
            },
            Band {
                offset: 10,
                w: 4,
                h: 1,
                quant: 0,
            },
        ];
        let mut t = Tile::new(false);
        t.coeff[0] = vec![0; 14];
        t.signs[0] = vec![0; 14];
        let q = [1; 10];
        let mut p25 = [0; 10];
        p25[7] = 4;
        p25[8] = 3;
        p25[0] = 2;
        let mut p50 = [0; 10];
        p50[8] = 1;
        p50[0] = 2;
        t.first_layout(
            0,
            &[0xa8, 0x62, 0x6d, 0xff, 0xf7, 0],
            q,
            p25,
            false,
            &layout,
        )
        .unwrap();
        assert_eq!(
            t.coeff[0],
            [-32, 0, 0, 0, 0, 0, 0, 0, 0, 8, 12, 16, -12, 12]
        );
        t.upgrade_layout(
            0,
            &[0xa0, 1, 0x80, 0, 0xc9, 0x49, 0xe0],
            &[0x20],
            q,
            p50,
            &layout,
        )
        .unwrap();
        assert_eq!(
            t.coeff[0],
            [-34, -13, 15, -3, 0, 0, -6, 4, -2, 8, 12, 16, -12, 12]
        );
        t.first_layout(
            0,
            &[0x88, 0x76, 0xbd, 0xff, 0xfe, 0xf2],
            q,
            p25,
            true,
            &layout,
        )
        .unwrap();
        assert_eq!(
            t.coeff[0],
            [-2, -13, 15, -3, 0, 0, -6, 4, -2, 8, 24, 12, 16, 4]
        );
        t.upgrade_layout(0, &[128, 7, 0, 21, 167, 0], &[0], q, p50, &layout)
            .unwrap();
        assert_eq!(
            t.coeff[0],
            [-2, -2, 3, -3, 1, 2, -6, 4, -2, 2, 24, 12, 16, 4]
        );
        t.upgrade_layout(0, &[0xb0], &[0xc6, 0x40], q, [0; 10], &layout)
            .unwrap();
        assert_eq!(
            t.coeff[0],
            [-2, -2, 3, -3, 1, 3, -7, 5, -2, 1, 24, 13, 18, 5]
        );
    }
    #[test]
    fn independent_reduced_transform_and_difference_history() {
        let mut p = Progressive::default();
        let mut frame = Frame::new(64, 64).unwrap();
        for (pass, data) in [
            include_bytes!("../tests/data/progressive-reduced-gradient.bin").as_slice(),
            include_bytes!("../tests/data/progressive-reduced-difference-gradient.bin").as_slice(),
        ]
        .into_iter()
        .enumerate()
        {
            p.update(data, &mut frame, TILE_BUDGET - 1).unwrap();
            let mut maximum = 0;
            let mut total = 0u64;
            for y in 0..64i32 {
                for x in 0..64i32 {
                    let luma = 64 + ((x * 13 + y * 7 + x * y) % 96) + pass as i32 * 4;
                    let cb = (x - 32) / 2;
                    let cr = (y - 32) / 2;
                    let expected = [
                        luma + ((115933i64 * cb as i64) >> 16) as i32,
                        luma - ((22527i64 * cb as i64 + 46792i64 * cr as i64) >> 16) as i32,
                        luma + ((91957i64 * cr as i64) >> 16) as i32,
                    ];
                    for (c, v) in expected.into_iter().enumerate() {
                        let error =
                            (frame.bgra[((y * 64 + x) * 4) as usize + c] as i32 - v).unsigned_abs();
                        maximum = maximum.max(error);
                        total += error as u64;
                    }
                }
            }
            println!(
                "reduced pass {pass} maximum {maximum}, mean {}",
                total as f64 / (N * 3) as f64
            );
            assert!(maximum <= 12 && total < (N * 3 * 3) as u64);
        }
        let mut missing = Progressive::default();
        assert!(
            missing
                .update(
                    include_bytes!("../tests/data/progressive-reduced-difference-gradient.bin"),
                    &mut frame,
                    TILE_BUDGET
                )
                .is_err()
        );
    }
    #[test]
    fn clipping_limits_and_missing_reference_are_bounded() {
        let fixture = include_bytes!("../tests/data/progressive-gradient.bin");
        let mut p = Progressive::default();
        let mut frame = Frame::new(64, 64).unwrap();
        assert!(p.update(fixture, &mut frame, 0).is_err());
        assert_eq!(p.tile_count(), 0);
        let mut clipped = fixture.to_vec();
        let start = clipped.windows(2).position(|v| v == [0xc4, 0xcc]).unwrap() + 18;
        for (i, v) in [5u16, 6, 10, 11].iter().enumerate() {
            clipped[start + i * 2..start + i * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
        p.update(&clipped, &mut frame, TILE_BUDGET).unwrap();
        for y in 0..64 {
            for x in 0..64 {
                if !(5..15).contains(&x) || !(6..17).contains(&y) {
                    assert_eq!(
                        &frame.bgra[(y * 64 + x) * 4..(y * 64 + x + 1) * 4],
                        &[0, 0, 0, 255]
                    );
                }
            }
        }
        let mut broken = fixture.to_vec();
        broken[2..6].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            Progressive::default()
                .update(&broken, &mut frame, TILE_BUDGET)
                .is_err()
        );
        for prefix in 0..fixture.len() {
            let mut p = Progressive::default();
            let mut f = Frame::new(64, 64).unwrap();
            let _ = p.update(&fixture[..prefix], &mut f, TILE_BUDGET);
        }
    }
    #[test]
    fn independent_encoder_gradient() {
        let mut p = Progressive::default();
        let mut frame = Frame::new(64, 64).unwrap();
        p.update(
            include_bytes!("../tests/data/progressive-gradient.bin"),
            &mut frame,
            TILE_BUDGET,
        )
        .unwrap();
        let mut maximum = 0;
        let mut total = 0u64;
        for y in 0..64 {
            for x in 0..64 {
                let pixel = &frame.bgra[(y * 64 + x) * 4..(y * 64 + x + 1) * 4];
                for (c, expected) in [3 * x, 3 * y, 2 * (x + y)].into_iter().enumerate() {
                    let error = (pixel[c] as i32 - expected as i32).unsigned_abs();
                    maximum = maximum.max(error);
                    total += error as u64;
                }
                assert_eq!(pixel[3], 255);
            }
        }
        println!(
            "gradient maximum {maximum}, mean {}",
            total as f64 / (N * 3) as f64
        );
        assert!(maximum <= 12 && total < (N * 3 * 3) as u64);
    }
}
