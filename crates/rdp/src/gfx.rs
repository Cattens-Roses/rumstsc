//! RDPGFX surfaces, frames, cache and AVC420. Wire framing is independent of
//! the system H.264 decoder; GPUI never parses or decodes video packets.
use crate::{
    Error, Result,
    bitmap::Frame,
    invalid,
    wire::{Reader, u16le, u32le},
    zgfx::Zgfx,
};
use std::collections::BTreeMap;
const LIMIT: usize = 64 * 1024 * 1024;
const SURFACE_LIMIT: usize = 128 * 1024 * 1024;
const CACHE_LIMIT: usize = 16 * 1024 * 1024;
struct Surface {
    frame: Frame,
    video: Option<rumstsc_video::Decoder>,
    dirty: Option<[u16; 4]>,
    mapping: Option<(u16, u16)>,
}
#[derive(Default)]
pub(crate) struct Gfx {
    zgfx: Zgfx,
    clear: crate::clear::Clear,
    pending: Vec<u8>,
    surfaces: BTreeMap<u16, Surface>,
    cache: BTreeMap<u16, Frame>,
    progressive: BTreeMap<(u16, u32), crate::progressive::Progressive>,
    pub progressive_updates: u64,
    confirmed: bool,
    frame: Option<u32>,
    decoded: u32,
    pub counters: [u64; 4],
    pub completed: u64,
}
pub(crate) fn packet(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut p = vec![];
    u16le(&mut p, kind);
    u16le(&mut p, 0);
    u32le(&mut p, (body.len() + 8) as u32);
    p.extend(body);
    p
}
pub(crate) fn capabilities() -> Vec<u8> {
    let mut body = vec![];
    u16le(&mut body, 1);
    u32le(&mut body, 0x00080105);
    u32le(&mut body, 4);
    u32le(&mut body, 0x12);
    packet(0x12, &body)
}
fn rectangle(r: &mut Reader<'_>) -> Result<[u16; 4]> {
    let v = [r.u16()?, r.u16()?, r.u16()?, r.u16()?];
    if v[0] >= v[2] || v[1] >= v[3] {
        return Err(invalid("empty/inverted GFX rectangle"));
    }
    Ok(v)
}
fn checked_rect(frame: &Frame, v: [u16; 4]) -> Result<[u16; 4]> {
    if v[2] > frame.width || v[3] > frame.height {
        return Err(invalid("GFX surface rectangle bounds"));
    }
    Ok(v)
}
fn crop(frame: &Frame, v: [u16; 4]) -> Result<Frame> {
    checked_rect(frame, v)?;
    let mut result = Frame::new(v[2] - v[0], v[3] - v[1])?;
    result.memory_blt(
        frame,
        [
            0,
            0,
            result.width as i16,
            result.height as i16,
            v[0] as i16,
            v[1] as i16,
        ],
        0xcc,
        None,
    )?;
    Ok(result)
}
impl Surface {
    fn dirty(&mut self, v: [u16; 4]) {
        self.dirty = Some(match self.dirty {
            None => v,
            Some(old) => [
                old[0].min(v[0]),
                old[1].min(v[1]),
                old[2].max(v[2]),
                old[3].max(v[3]),
            ],
        });
    }
    fn blit(&mut self, source: &Frame, x: u16, y: u16) -> Result<()> {
        let right = x
            .checked_add(source.width)
            .ok_or_else(|| invalid("GFX target overflow"))?;
        let bottom = y
            .checked_add(source.height)
            .ok_or_else(|| invalid("GFX target overflow"))?;
        checked_rect(&self.frame, [x, y, right, bottom])?;
        self.frame.memory_blt(
            source,
            [
                x as i16,
                y as i16,
                source.width as i16,
                source.height as i16,
                0,
                0,
            ],
            0xcc,
            None,
        )?;
        self.dirty([x, y, right, bottom]);
        Ok(())
    }
}
impl Gfx {
    pub fn update(
        &mut self,
        input: &[u8],
        desktop: &mut Frame,
        mut commit: impl FnMut(&Frame),
    ) -> Result<Vec<Vec<u8>>> {
        let expanded = self.zgfx.decode(input)?;
        if expanded.len() > LIMIT - self.pending.len() {
            return Err(invalid("GFX receive limit"));
        }
        self.pending.extend(expanded);
        let mut used = 0;
        let mut replies = vec![];
        while self.pending.len() - used >= 8 {
            let mut header = Reader::new(&self.pending[used..]);
            let kind = header.u16()?;
            let flags = header.u16()?;
            let size = header.u32()? as usize;
            if !(8..=LIMIT).contains(&size) || flags != 0 {
                return Err(invalid("GFX header"));
            }
            if self.pending.len() - used < size {
                break;
            }
            // Move only the current PDU to avoid aliasing the persistent receive buffer
            // while mutating the surface/cache state. Network payloads remain bounded.
            let body = self.pending[used + 8..used + size].to_vec();
            let mut r = Reader::new(&body);
            if !self.confirmed && kind != 0x13 {
                return Err(invalid("GFX command before capabilities confirmation"));
            }
            match kind {
                0x13 => {
                    if self.confirmed || r.u32()? != 0x80105 || r.u32()? != 4 {
                        return Err(invalid("GFX capabilities selection"));
                    }
                    let flags = r.u32()?;
                    if flags & !0x12 != 0 || flags & 2 == 0 {
                        return Err(invalid("GFX unsupported capabilities selection"));
                    }
                    self.confirmed = true;
                }
                9 => {
                    let id = r.u16()?;
                    let w = r.u16()?;
                    let h = r.u16()?;
                    let format = r.u8()?;
                    if self.surfaces.contains_key(&id)
                        || self.surfaces.len() >= 16
                        || ![0x20, 0x21].contains(&format)
                    {
                        return Err(invalid("GFX surface definition"));
                    }
                    Frame::validate_dimensions(w, h)?;
                    let current: usize = self.surfaces.values().map(|s| s.frame.bgra.len()).sum();
                    if w as usize * h as usize * 4 > SURFACE_LIMIT - current {
                        return Err(invalid("GFX surface memory limit"));
                    }
                    self.surfaces.insert(
                        id,
                        Surface {
                            frame: Frame::new(w, h)?,
                            video: None,
                            dirty: None,
                            mapping: None,
                        },
                    );
                }
                10 => {
                    let id = r.u16()?;
                    self.progressive.retain(|(surface, _), _| *surface != id);
                    if self.surfaces.remove(&id).is_none() {
                        return Err(invalid("GFX unknown surface deletion"));
                    }
                }
                14 => {
                    if size != 340 || self.frame.is_some() {
                        return Err(invalid("GFX reset framing"));
                    }
                    let w = u16::try_from(r.u32()?).map_err(|_| invalid("GFX desktop width"))?;
                    let h = u16::try_from(r.u32()?).map_err(|_| invalid("GFX desktop height"))?;
                    let monitors = r.u32()?;
                    if monitors > 16 {
                        return Err(invalid("GFX monitor count"));
                    }
                    r.take(monitors as usize * 20)?;
                    r.rest();
                    *desktop = Frame::new(w, h)?;
                    for s in self.surfaces.values_mut() {
                        s.mapping = None;
                    }
                }
                15 => {
                    let id = r.u16()?;
                    if r.u16()? != 0 {
                        return Err(invalid("GFX mapping reserved"));
                    }
                    let x = u16::try_from(r.u32()?).map_err(|_| invalid("GFX output x"))?;
                    let y = u16::try_from(r.u32()?).map_err(|_| invalid("GFX output y"))?;
                    let s = self.surface(id)?;
                    if x as u32 + s.frame.width as u32 > desktop.width as u32
                        || y as u32 + s.frame.height as u32 > desktop.height as u32
                    {
                        return Err(invalid("GFX output mapping bounds"));
                    }
                    s.mapping = Some((x, y));
                    s.dirty([0, 0, s.frame.width, s.frame.height]);
                }
                11 => {
                    r.u32()?;
                    let id = r.u32()?;
                    if self.frame.replace(id).is_some() {
                        return Err(invalid("nested GFX frame"));
                    }
                    desktop.frame_marker(0)?;
                }
                12 => {
                    let id = r.u32()?;
                    if self.frame.take() != Some(id) {
                        return Err(invalid("GFX frame ID mismatch"));
                    }
                    for s in self.surfaces.values_mut() {
                        if let (Some((x, y)), Some(v)) = (s.mapping, s.dirty.take()) {
                            desktop.memory_blt(
                                &s.frame,
                                [
                                    (x + v[0]) as i16,
                                    (y + v[1]) as i16,
                                    (v[2] - v[0]) as i16,
                                    (v[3] - v[1]) as i16,
                                    v[0] as i16,
                                    v[1] as i16,
                                ],
                                0xcc,
                                None,
                            )?;
                        }
                    }
                    desktop.frame_marker(1)?;
                    commit(desktop);
                    self.decoded = self.decoded.wrapping_add(1);
                    self.completed += 1;
                    let mut ack = vec![];
                    u32le(&mut ack, 0);
                    u32le(&mut ack, id);
                    u32le(&mut ack, self.decoded);
                    replies.push(packet(13, &ack));
                    if replies.len() > 64 {
                        return Err(invalid("GFX acknowledgement batch limit"));
                    }
                }
                4 => {
                    let id = r.u16()?;
                    let color = r.take(4)?.to_vec();
                    let count = r.u16()?;
                    let s = self.surface(id)?;
                    for _ in 0..count {
                        let v = checked_rect(&s.frame, rectangle(&mut r)?)?;
                        for y in v[1]..v[3] {
                            let start = (y as usize * s.frame.width as usize + v[0] as usize) * 4;
                            for p in s.frame.bgra[start..start + (v[2] - v[0]) as usize * 4]
                                .as_chunks_mut::<4>()
                                .0
                                .iter_mut()
                            {
                                p.copy_from_slice(&color);
                                p[3] = 255;
                            }
                        }
                        s.dirty(v);
                    }
                }
                5 => {
                    let source = r.u16()?;
                    let target = r.u16()?;
                    let v = rectangle(&mut r)?;
                    let count = r.u16()?;
                    let source = crop(&self.surface(source)?.frame, v)?;
                    for _ in 0..count {
                        let x = r.u16()?;
                        let y = r.u16()?;
                        self.surface(target)?.blit(&source, x, y)?;
                    }
                }
                6 => {
                    let id = r.u16()?;
                    r.take(8)?;
                    let slot = r.u16()?;
                    let v = rectangle(&mut r)?;
                    if slot == 0 || slot > 4096 {
                        return Err(invalid("GFX cache slot"));
                    }
                    let entry = crop(&self.surface(id)?.frame, v)?;
                    let current: usize = self.cache.values().map(|f| f.bgra.len()).sum();
                    let replaced = self.cache.get(&slot).map_or(0, |f| f.bgra.len());
                    if entry.bgra.len() > CACHE_LIMIT - (current - replaced) {
                        return Err(invalid("GFX bitmap cache budget"));
                    }
                    self.cache.insert(slot, entry);
                }
                7 => {
                    let slot = r.u16()?;
                    let id = r.u16()?;
                    let count = r.u16()?;
                    let source = self
                        .cache
                        .get(&slot)
                        .ok_or_else(|| invalid("GFX missing cache entry"))?;
                    let s = self
                        .surfaces
                        .get_mut(&id)
                        .ok_or_else(|| invalid("GFX unknown destination"))?;
                    for _ in 0..count {
                        let x = r.u16()?;
                        let y = r.u16()?;
                        s.blit(source, x, y)?;
                    }
                }
                8 => {
                    let slot = r.u16()?;
                    self.cache
                        .remove(&slot)
                        .ok_or_else(|| invalid("GFX evict missing cache entry"))?;
                }
                1 => {
                    let id = r.u16()?;
                    let codec = r.u16()?;
                    let format = r.u8()?;
                    let v = rectangle(&mut r)?;
                    let length = r.u32()? as usize;
                    let bytes = r.take(length)?;
                    if ![0x20, 0x21].contains(&format) {
                        return Err(invalid("GFX pixel format"));
                    }
                    let cleared = if codec == 0xa {
                        let mut source =
                            crate::planar::decode(bytes, v[2] - v[0], v[3] - v[1], false)?;
                        // MS-RDPEGFX 3.3.5.1: color codecs update RGB, not alpha.
                        // Desktop surfaces are opaque; a transmitted zero alpha
                        // plane must not make the GPUI texture transparent.
                        for pixel in source.bgra.as_chunks_mut::<4>().0.iter_mut() {
                            pixel[3] = 255;
                        }
                        Some(source)
                    } else if codec == 8 {
                        Some(self.clear.decode(bytes, v[2] - v[0], v[3] - v[1])?)
                    } else {
                        None
                    };
                    let s = self.surface(id)?;
                    checked_rect(&s.frame, v)?;
                    match codec {
                        8 | 0xa => s.blit(&cleared.unwrap(), v[0], v[1])?,
                        0 => {
                            let w = v[2] - v[0];
                            let h = v[3] - v[1];
                            if bytes.len() != w as usize * h as usize * 4 {
                                return Err(invalid("GFX raw bitmap length"));
                            }
                            let mut source = Frame::new(w, h)?;
                            source.bgra.copy_from_slice(bytes);
                            for p in source.bgra.as_chunks_mut::<4>().0.iter_mut() {
                                p[3] = 255;
                            }
                            s.blit(&source, v[0], v[1])?;
                        }
                        0xb => {
                            let mut meta = Reader::new(bytes);
                            let count = meta.u32()? as usize;
                            if count > 4096 {
                                return Err(invalid("AVC region count"));
                            }
                            let mut regions = Vec::with_capacity(count);
                            for _ in 0..count {
                                regions.push(checked_rect(&s.frame, rectangle(&mut meta)?)?);
                            }
                            meta.take(count * 2)?;
                            if s.video.is_none() {
                                s.video = Some(
                                    rumstsc_video::Decoder::with_size(
                                        s.frame.width,
                                        s.frame.height,
                                    )
                                    .map_err(Error::Unsupported)?,
                                );
                            }
                            let picture = s
                                .video
                                .as_mut()
                                .unwrap()
                                .decode(meta.rest())
                                .map_err(Error::Protocol)?
                                .ok_or_else(|| invalid("AVC packet produced no picture"))?;
                            let source =
                                Frame::from_bgra(picture.width, picture.height, picture.bgra)?;
                            for region in regions {
                                checked_rect(&source, region)?;
                                s.frame.memory_blt(
                                    &source,
                                    [
                                        region[0] as i16,
                                        region[1] as i16,
                                        (region[2] - region[0]) as i16,
                                        (region[3] - region[1]) as i16,
                                        region[0] as i16,
                                        region[1] as i16,
                                    ],
                                    0xcc,
                                    None,
                                )?;
                                s.dirty(region);
                            }
                        }
                        _ => return Err(Error::Unsupported(format!("RDPGFX codec 0x{codec:04x}"))),
                    }
                    self.counters[match codec {
                        0xb => 0,
                        8 => 1,
                        0xa => 2,
                        _ => 3,
                    }] += 1;
                }
                2 => {
                    let id = r.u16()?;
                    let codec = r.u16()?;
                    let context = r.u32()?;
                    let format = r.u8()?;
                    let length = r.u32()? as usize;
                    let bytes = r.take(length)?;
                    if ![0x20, 0x21].contains(&format) {
                        return Err(invalid("GFX progressive pixel format"));
                    }
                    if codec != 9 {
                        return Err(Error::Unsupported(format!(
                            "RDPGFX WireToSurface2 codec 0x{codec:04x}"
                        )));
                    }
                    if !self.progressive.contains_key(&(id, context))
                        && self.progressive.len() >= 32
                    {
                        return Err(invalid("GFX progressive context limit"));
                    }
                    let used: usize = self.progressive.values().map(|p| p.tile_count()).sum();
                    let surface = self
                        .surfaces
                        .get_mut(&id)
                        .ok_or_else(|| invalid("unknown GFX progressive surface"))?;
                    let progressive = self.progressive.entry((id, context)).or_default();
                    let dirty = progressive.update(
                        bytes,
                        &mut surface.frame,
                        crate::progressive::TILE_BUDGET.saturating_sub(used),
                    )?;
                    for rect in dirty {
                        surface.dirty(rect);
                    }
                    self.progressive_updates += 1;
                }
                3 => {
                    let id = r.u16()?;
                    let context = r.u32()?;
                    self.surface(id)?;
                    self.progressive.remove(&(id, context));
                }
                _ => return Err(Error::Unsupported(format!("RDPGFX command 0x{kind:04x}"))),
            }
            if r.remaining() != 0 {
                return Err(invalid("GFX command length"));
            }
            used += size;
        }
        self.pending.drain(..used);
        Ok(replies)
    }
    fn surface(&mut self, id: u16) -> Result<&mut Surface> {
        self.surfaces
            .get_mut(&id)
            .ok_or_else(|| invalid("unknown GFX surface"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_to_surface_two_context_lifetime_and_frame_commit() {
        let mut gfx = Gfx::default();
        let mut desktop = Frame::new(64, 64).unwrap();
        let mut commands = packet(0x13, &[5, 1, 8, 0, 4, 0, 0, 0, 0x12, 0, 0, 0]);
        commands.extend(packet(9, &[1, 0, 64, 0, 64, 0, 0x20]));
        commands.extend(packet(15, &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        let update = |data: &[u8]| {
            let mut body = vec![1, 0, 9, 0];
            u32le(&mut body, 17);
            body.push(0x20);
            u32le(&mut body, data.len() as u32);
            body.extend(data);
            packet(2, &body)
        };
        for (index, data) in [
            include_bytes!("../tests/data/progressive-reduced-gradient.bin").as_slice(),
            include_bytes!("../tests/data/progressive-reduced-difference-gradient.bin").as_slice(),
        ]
        .into_iter()
        .enumerate()
        {
            let mut start = vec![];
            u32le(&mut start, 0);
            u32le(&mut start, index as u32);
            commands.extend(packet(11, &start));
            commands.extend(update(data));
            commands.extend(packet(12, &(index as u32).to_le_bytes()));
            let mut wire = vec![0xe0, 4];
            wire.extend(&commands);
            let replies = gfx
                .update(&wire, &mut desktop, |f| {
                    assert!(f.bgra.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
                })
                .unwrap();
            assert_eq!(replies.len(), 1);
            commands.clear();
        }
        assert_eq!(gfx.progressive_updates, 2);
        assert_eq!(gfx.completed, 2);
        assert_eq!(gfx.progressive.len(), 1);
        let mut body = vec![1, 0];
        u32le(&mut body, 17);
        let mut wire = vec![0xe0, 4];
        wire.extend(packet(3, &body));
        gfx.update(&wire, &mut desktop, |_| {}).unwrap();
        assert!(gfx.progressive.is_empty());
        let mut wire = vec![0xe0, 4];
        wire.extend(update(include_bytes!(
            "../tests/data/progressive-reduced-difference-gradient.bin"
        )));
        assert!(gfx.update(&wire, &mut desktop, |_| {}).is_err());
    }
    #[test]
    fn planar_zero_alpha_is_opaque_in_desktop_and_ui_snapshot() {
        for format in [0x20, 0x21] {
            let mut gfx = Gfx::default();
            let mut desktop = Frame::new(2, 1).unwrap();
            let mut commands = packet(0x13, &[5, 1, 8, 0, 4, 0, 0, 0, 0x12, 0, 0, 0]);
            commands.extend(packet(9, &[1, 0, 2, 0, 1, 0, format]));
            commands.extend(packet(15, &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
            commands.extend(packet(11, &[0, 0, 0, 0, 1, 0, 0, 0]));
            // Raw planar ARGB: alpha=0, red=50/60, green=30/40, blue=10/20.
            let encoded = [0, 0, 0, 50, 60, 30, 40, 10, 20];
            let mut surface = vec![1, 0, 10, 0, format];
            for coordinate in [0, 0, 2, 1] {
                u16le(&mut surface, coordinate);
            }
            u32le(&mut surface, encoded.len() as u32);
            surface.extend(encoded);
            commands.extend(packet(1, &surface));
            commands.extend(packet(12, &1u32.to_le_bytes()));
            let mut wire = vec![0xe0, 4];
            wire.extend(commands);
            let replies = gfx.update(&wire, &mut desktop, |_| {}).unwrap();
            assert_eq!(replies.len(), 1);
            let expected = [10, 30, 50, 255, 20, 40, 60, 255];
            assert_eq!(desktop.bgra, expected);
            assert_eq!(gfx.surfaces[&1].frame.bgra, expected);
            let snapshot = desktop.snapshot_since(0, 0).unwrap();
            assert_eq!(snapshot.tiles.len(), 1);
            assert_eq!(snapshot.tiles[0].bgra, expected);
        }
    }
}
