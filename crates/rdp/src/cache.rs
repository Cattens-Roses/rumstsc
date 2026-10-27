//! Bounded, session-local bitmap cache. Nothing is persisted to disk.
use crate::{
    Result,
    bitmap::Frame,
    invalid,
    wire::{Reader, u16le},
};
use std::{collections::BTreeMap, sync::Arc};
pub(crate) const ENTRIES: [u16; 3] = [120, 120, 512];
pub(crate) const CELL_BYTES: [u16; 3] = [768, 3072, 12288];
#[derive(Clone, Default)]
pub(crate) struct Cache {
    entries: BTreeMap<(u8, u16), Arc<Frame>>,
}
impl Cache {
    pub fn get(&self, id: u16, index: u16) -> Result<&Frame> {
        if id >> 8 > 5 {
            return Err(invalid("bitmap color table index"));
        }
        self.entries
            .get(&(id as u8, index))
            .map(Arc::as_ref)
            .ok_or_else(|| invalid("missing bitmap cache entry"))
    }
    pub fn secondary(&mut self, kind: u8, extra: u16, body: &[u8]) -> Result<()> {
        if ![0, 2].contains(&kind) {
            return Err(invalid("unadvertised secondary drawing order"));
        }
        if extra & !0x0400 != 0 {
            return Err(invalid("cache bitmap flags"));
        }
        let mut r = Reader::new(body);
        let id = r.u8()?;
        r.u8()?;
        let w = r.u8()? as u16;
        let h = r.u8()? as u16;
        let bpp = r.u8()? as u16;
        let length = r.u16()?;
        let index = r.u16()?;
        if id > 2
            || index >= ENTRIES[id as usize]
            || w == 0
            || h == 0
            || ![16, 24, 32].contains(&bpp)
            || w as usize * h as usize * (bpp as usize / 8) > CELL_BYTES[id as usize] as usize
        {
            return Err(invalid("bitmap cache cell bounds"));
        }
        let encoded = r.take(length as usize)?;
        if r.remaining() != 0 {
            return Err(invalid("cache bitmap order length"));
        }
        let mut update = vec![];
        for n in [
            1,
            0,
            0,
            w - 1,
            h - 1,
            w,
            h,
            bpp,
            if kind == 2 { 1 | (extra & 0x0400) } else { 0 },
            length,
        ] {
            u16le(&mut update, n);
        }
        update.extend(encoded);
        let mut frame = Frame::new(w, h)?;
        frame.update(&update)?;
        self.entries.insert((id, index), Arc::new(frame));
        Ok(())
    }
}
