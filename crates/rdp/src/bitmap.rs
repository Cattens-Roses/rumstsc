//! Slow-path raw and interleaved-RLE bitmap updates.
use crate::{Error, Result, invalid, wire::Reader};
use std::sync::atomic::{AtomicU64, Ordering};

pub const TILE_SIZE: u16 = 128;
#[derive(Debug)]
pub struct TilePixels {
    pub index: usize,
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    pub bgra: Vec<u8>,
}
pub struct Snapshot {
    pub generation: u64,
    pub revision: u64,
    pub width: u16,
    pub height: u16,
    pub tiles: Vec<TilePixels>,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u16,
    pub height: u16,
    pub bgra: Vec<u8>,
    pub revision: u64,
    pub compressed_rectangles: u64,
    pub raw_rectangles: u64,
    pub screen_blits: u64,
    pub logical_frames: u64,
    frame_open: bool,
    generation: u64,
    tile_revisions: Vec<u64>,
}
impl Frame {
    pub(crate) fn validate_dimensions(width: u16, height: u16) -> Result<()> {
        if width == 0 || height == 0 || width > 4096 || height > 2160 {
            return Err(invalid("desktop dimensions must be within 4096 x 2160"));
        }
        Ok(())
    }
    pub fn new(width: u16, height: u16) -> Result<Self> {
        Self::validate_dimensions(width, height)?;
        Self::from_bgra(
            width,
            height,
            [0, 0, 0, 255].repeat(width as usize * height as usize),
        )
    }
    pub(crate) fn from_bgra(width: u16, height: u16, bgra: Vec<u8>) -> Result<Self> {
        Self::validate_dimensions(width, height)?;
        if bgra.len() != width as usize * height as usize * 4 {
            return Err(invalid("BGRA surface length"));
        }
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
        Ok(Self {
            width,
            height,
            bgra,
            revision: 0,
            compressed_rectangles: 0,
            raw_rectangles: 0,
            screen_blits: 0,
            logical_frames: 0,
            frame_open: false,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            tile_revisions: vec![
                0;
                width.div_ceil(TILE_SIZE) as usize
                    * height.div_ceil(TILE_SIZE) as usize
            ],
        })
    }
    /// Copy only tiles changed since the last displayed snapshot. Revision
    /// tracking is per tile, so skipped UI ticks do not lose intermediate changes.
    /// A new framebuffer generation always produces a complete initial image.
    pub fn snapshot_since(&self, generation: u64, revision: u64) -> Option<Snapshot> {
        if self.frame_open
            || self.revision == 0
            || (generation == self.generation && revision == self.revision)
        {
            return None;
        }
        let columns = self.width.div_ceil(TILE_SIZE) as usize;
        let mut tiles = vec![];
        for (index, &changed) in self.tile_revisions.iter().enumerate() {
            if generation == self.generation && changed <= revision {
                continue;
            }
            let left = (index % columns) as u16 * TILE_SIZE;
            let top = (index / columns) as u16 * TILE_SIZE;
            let width = TILE_SIZE.min(self.width - left);
            let height = TILE_SIZE.min(self.height - top);
            let mut bgra = Vec::with_capacity(width as usize * height as usize * 4);
            for y in top..top + height {
                let start = (y as usize * self.width as usize + left as usize) * 4;
                bgra.extend_from_slice(&self.bgra[start..start + width as usize * 4]);
            }
            tiles.push(TilePixels {
                index,
                left,
                top,
                width,
                height,
                bgra,
            });
        }
        Some(Snapshot {
            generation: self.generation,
            revision: self.revision,
            width: self.width,
            height: self.height,
            tiles,
        })
    }
    pub(crate) fn reset_frame_marker(&mut self) -> bool {
        std::mem::replace(&mut self.frame_open, false)
    }
    pub(crate) fn frame_marker(&mut self, action: u32) -> Result<()> {
        match (action, self.frame_open) {
            (0, false) => self.frame_open = true,
            (1, true) => {
                self.frame_open = false;
                self.logical_frames += 1;
            }
            _ => return Err(invalid("invalid logical frame marker sequence")),
        }
        Ok(())
    }
    fn mark_dirty(&mut self, left: usize, top: usize, right: usize, bottom: usize) {
        let columns = self.width.div_ceil(TILE_SIZE) as usize;
        for ty in top / TILE_SIZE as usize..=bottom / TILE_SIZE as usize {
            for tx in left / TILE_SIZE as usize..=right / TILE_SIZE as usize {
                self.tile_revisions[ty * columns + tx] = self.revision + 1;
            }
        }
    }
    /// Move/copy pixels already held by the client, including overlap and clipping.
    /// ScrBlt ROP3 operations may depend on source and destination, never pattern.
    pub fn screen_blt(&mut self, values: [i16; 6], rop: u8, clip: Option<[i16; 4]>) -> Result<()> {
        if rop & 15 != rop >> 4 {
            return Err(invalid("ScrBlt raster operation depends on pattern"));
        }
        let [x, y, w, h, sx, sy] = values.map(i32::from);
        if w < 0 || h < 0 {
            return Err(invalid("negative ScrBlt dimensions"));
        }
        let [cl, ct, cr, cb] = clip.map(|v| v.map(i32::from)).unwrap_or([
            0,
            0,
            self.width as i32 - 1,
            self.height as i32 - 1,
        ]);
        if cl > cr || ct > cb {
            return Err(invalid("invalid drawing bounds"));
        }
        let left = x.max(0).max(cl);
        let top = y.max(0).max(ct);
        let right = (x + w).min(self.width as i32).min(cr + 1);
        let bottom = (y + h).min(self.height as i32).min(cb + 1);
        if left >= right || top >= bottom {
            return Ok(());
        }
        let src_x = sx + left - x;
        let src_y = sy + top - y;
        let width = (right - left) as usize;
        let height = (bottom - top) as usize;
        let source_used = (rop ^ (rop >> 2)) & 3 != 0;
        if source_used
            && (src_x < 0
                || src_y < 0
                || src_x + width as i32 > self.width as i32
                || src_y + height as i32 > self.height as i32)
        {
            return Err(invalid("ScrBlt source exceeds desktop"));
        }
        let stride = self.width as usize * 4;
        // Walk away from the direction of overlap so every source pixel is read
        // before its original value can be overwritten by the destination.
        for row in 0..height {
            let row = if source_used && top > src_y {
                height - 1 - row
            } else {
                row
            };
            let dst = (top as usize + row) * stride + left as usize * 4;
            let src = if source_used {
                (src_y as usize + row) * stride + src_x as usize * 4
            } else {
                0
            };
            if rop == 0xcc {
                self.bgra.copy_within(src..src + width * 4, dst);
                continue;
            }
            for col in 0..width {
                let col = if source_used && top == src_y && left > src_x {
                    width - 1 - col
                } else {
                    col
                };
                let d = dst + col * 4;
                let source = src + col * 4;
                for channel in 0..3 {
                    let s = if source_used {
                        self.bgra[source + channel]
                    } else {
                        0
                    };
                    let old = self.bgra[d + channel];
                    self.bgra[d + channel] = (if rop & 1 != 0 { !s & !old } else { 0 })
                        | (if rop & 2 != 0 { !s & old } else { 0 })
                        | (if rop & 4 != 0 { s & !old } else { 0 })
                        | (if rop & 8 != 0 { s & old } else { 0 });
                }
                self.bgra[d + 3] = 255;
            }
        }
        self.mark_dirty(
            left as usize,
            top as usize,
            right as usize - 1,
            bottom as usize - 1,
        );
        self.revision += 1;
        self.screen_blits += 1;
        Ok(())
    }
    /// Copy a top-down BGRA surface, with source/destination ROP and clipping.
    pub(crate) fn memory_blt(
        &mut self,
        source: &Frame,
        values: [i16; 6],
        rop: u8,
        clip: Option<[i16; 4]>,
    ) -> Result<()> {
        if rop & 15 != rop >> 4 {
            return Err(invalid("bitmap ROP depends on pattern"));
        }
        let [x, y, w, h, sx, sy] = values.map(i32::from);
        if w < 0 || h < 0 {
            return Err(invalid("negative bitmap blit size"));
        }
        let [cl, ct, cr, cb] = clip.map(|v| v.map(i32::from)).unwrap_or([
            0,
            0,
            self.width as i32 - 1,
            self.height as i32 - 1,
        ]);
        if cl > cr || ct > cb {
            return Err(invalid("invalid bitmap blit bounds"));
        }
        let left = x.max(0).max(cl);
        let top = y.max(0).max(ct);
        let right = (x + w).min(self.width as i32).min(cr + 1);
        let bottom = (y + h).min(self.height as i32).min(cb + 1);
        if left >= right || top >= bottom {
            return Ok(());
        }
        let source_used = (rop ^ (rop >> 2)) & 3 != 0;
        let src_x = sx + left - x;
        let src_y = sy + top - y;
        if source_used
            && (src_x < 0
                || src_y < 0
                || src_x + right - left > source.width as i32
                || src_y + bottom - top > source.height as i32)
        {
            return Err(invalid("bitmap blit source bounds"));
        }
        for row in top..bottom {
            let dst = (row as usize * self.width as usize + left as usize) * 4;
            let width = (right - left) as usize;
            let src = if source_used {
                ((src_y + row - top) as usize * source.width as usize + src_x as usize) * 4
            } else {
                0
            };
            if rop == 0xcc {
                self.bgra[dst..dst + width * 4].copy_from_slice(&source.bgra[src..src + width * 4]);
                continue;
            }
            for col in 0..width {
                for channel in 0..3 {
                    let s = if source_used {
                        source.bgra[src + col * 4 + channel]
                    } else {
                        0
                    };
                    let d = &mut self.bgra[dst + col * 4 + channel];
                    let old = *d;
                    *d = (if rop & 1 != 0 { !s & !old } else { 0 })
                        | (if rop & 2 != 0 { !s & old } else { 0 })
                        | (if rop & 4 != 0 { s & !old } else { 0 })
                        | (if rop & 8 != 0 { s & old } else { 0 });
                }
                self.bgra[dst + col * 4 + 3] = 255;
            }
        }
        self.mark_dirty(
            left as usize,
            top as usize,
            right as usize - 1,
            bottom as usize - 1,
        );
        self.revision += 1;
        Ok(())
    }
    pub fn update(&mut self, data: &[u8]) -> Result<()> {
        let mut r = Reader::new(data);
        let count = r.u16()?;
        for _ in 0..count {
            let left = r.u16()? as usize;
            let top = r.u16()? as usize;
            let right = r.u16()? as usize;
            let bottom = r.u16()? as usize;
            let w = r.u16()? as usize;
            let h = r.u16()? as usize;
            let bpp = r.u16()?;
            let flags = r.u16()?;
            let len = r.u16()? as usize;
            let encoded = r.take(len)?;
            if flags & !0x0401 != 0 || flags == 0x0400 {
                return Err(invalid("bitmap compression flags"));
            }
            if ![15, 16, 24, 32].contains(&bpp) {
                return Err(Error::Unsupported(format!("{bpp}-bit bitmap")));
            }
            if right < left
                || bottom < top
                || right >= self.width as usize
                || bottom >= self.height as usize
                || w == 0
                || h == 0
                || w > 4096
                || h > 2160
                || right - left + 1 > w
                || bottom - top + 1 > h
            {
                return Err(invalid("bitmap rectangle exceeds desktop"));
            }
            let bytes = (bpp as usize).div_ceil(8);
            let decoded;
            let (pixels, stride) = if flags & 1 != 0 {
                if bpp == 32 {
                    let compressed = if flags & 0x400 == 0 {
                        let mut header = Reader::new(encoded);
                        let first = header.u16()?;
                        let main = header.u16()? as usize;
                        let scan = header.u16()? as usize;
                        let size = header.u16()? as usize;
                        if first != 0
                            || main != header.remaining()
                            || scan != w * 4
                            || size != w * h * 4
                        {
                            return Err(invalid("planar bitmap compression header"));
                        }
                        header.rest()
                    } else {
                        encoded
                    };
                    let source = crate::planar::decode(compressed, w as u16, h as u16, true)?;
                    for y in 0..=bottom - top {
                        let src = y * w * 4;
                        let dst = ((top + y) * self.width as usize + left) * 4;
                        let count = (right - left + 1) * 4;
                        self.bgra[dst..dst + count].copy_from_slice(&source.bgra[src..src + count]);
                    }
                    self.mark_dirty(left, top, right, bottom);
                    self.compressed_rectangles += 1;
                    continue;
                }
                let compressed = if flags & 0x0400 == 0 {
                    let mut header = Reader::new(encoded);
                    let first_row = header.u16()?;
                    let main = header.u16()? as usize;
                    let scan = header.u16()? as usize;
                    let size = header.u16()? as usize;
                    if first_row != 0
                        || main != header.remaining()
                        || scan != w * bytes
                        || size != w * h * bytes
                    {
                        return Err(invalid("bitmap compression header"));
                    }
                    header.rest()
                } else {
                    encoded
                };
                decoded = crate::rle::decode(compressed, w, h, bytes)?;
                (decoded.as_slice(), w * bytes)
            } else {
                (encoded, (w * bytes).div_ceil(4) * 4)
            };
            if stride * h != pixels.len() {
                return Err(invalid("bitmap buffer length"));
            }
            for y in 0..=bottom - top {
                for x in 0..=right - left {
                    let src = (h - 1 - y) * stride + x * bytes;
                    let dst = ((top + y) * self.width as usize + left + x) * 4;
                    let rgb = if bpp >= 24 {
                        [pixels[src], pixels[src + 1], pixels[src + 2], 255]
                    } else {
                        let v = u16::from_le_bytes([pixels[src], pixels[src + 1]]) as u32;
                        let (red, green, blue) = if bpp == 16 {
                            ((v >> 11) & 31, (v >> 5) & 63, v & 31)
                        } else {
                            ((v >> 10) & 31, (v >> 5) & 31, v & 31)
                        };
                        [
                            ((blue * 255 + 15) / 31) as u8,
                            ((green * 255 + if bpp == 16 { 31 } else { 15 })
                                / if bpp == 16 { 63 } else { 31 })
                                as u8,
                            ((red * 255 + 15) / 31) as u8,
                            255,
                        ]
                    };
                    self.bgra[dst..dst + 4].copy_from_slice(&rgb);
                }
            }
            self.mark_dirty(left, top, right, bottom);
            if flags & 1 != 0 {
                self.compressed_rectangles += 1;
            } else {
                self.raw_rectangles += 1;
            }
        }
        if r.remaining() != 0 {
            return Err(invalid("trailing bitmap data"));
        }
        self.revision += 1;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::u16le;
    #[test]
    fn tiled_snapshots_reconstruct_edges_and_skip_unchanged_frames() {
        let mut frame = Frame::new(259, 131).unwrap();
        assert!(frame.snapshot_since(0, 0).is_none());
        frame
            .update(&update([258, 130, 258, 130, 1, 1, 24, 0, 4], &[1, 2, 3, 0]))
            .unwrap();
        let snapshot = frame.snapshot_since(0, 0).unwrap();
        assert_eq!(snapshot.tiles.len(), 6);
        let edge = snapshot.tiles.last().unwrap();
        assert_eq!(
            (edge.left, edge.top, edge.width, edge.height),
            (256, 128, 3, 3)
        );
        let mut reconstructed = vec![0; frame.bgra.len()];
        for tile in &snapshot.tiles {
            for y in 0..tile.height as usize {
                let dst = ((tile.top as usize + y) * frame.width as usize + tile.left as usize) * 4;
                let src = y * tile.width as usize * 4;
                reconstructed[dst..dst + tile.width as usize * 4]
                    .copy_from_slice(&tile.bgra[src..src + tile.width as usize * 4]);
            }
        }
        assert_eq!(reconstructed, frame.bgra);
        assert!(
            frame
                .snapshot_since(snapshot.generation, snapshot.revision)
                .is_none()
        );
    }
    #[test]
    fn tiled_snapshots_coalesce_skipped_updates_and_tile_boundaries() {
        let mut frame = Frame::new(384, 256).unwrap();
        frame
            .update(&update([0, 0, 0, 0, 1, 1, 24, 0, 4], &[1, 2, 3, 0]))
            .unwrap();
        let first = frame.snapshot_since(0, 0).unwrap();
        // One update crosses four tiles. Another touches a fifth tile before the
        // UI takes a snapshot; all five must survive coalescing.
        frame
            .update(&update([127, 127, 128, 128, 2, 2, 24, 0, 16], &[4; 16]))
            .unwrap();
        frame
            .update(&update([383, 255, 383, 255, 1, 1, 24, 0, 4], &[5, 6, 7, 0]))
            .unwrap();
        let next = frame
            .snapshot_since(first.generation, first.revision)
            .unwrap();
        assert_eq!(
            next.tiles.iter().map(|t| t.index).collect::<Vec<_>>(),
            [0, 1, 3, 4, 5]
        );
        assert_eq!(
            &next.tiles.last().unwrap().bgra[128 * 128 * 4 - 4..],
            &[5, 6, 7, 255]
        );
    }
    #[test]
    fn small_changes_copy_one_tile_and_reactivation_forces_full_snapshot() {
        let mut frame = Frame::new(1920, 1080).unwrap();
        let pixel = update([1, 1, 1, 1, 1, 1, 24, 0, 4], &[1, 2, 3, 0]);
        frame.update(&pixel).unwrap();
        let first = frame.snapshot_since(0, 0).unwrap();
        frame.update(&pixel).unwrap();
        let next = frame
            .snapshot_since(first.generation, first.revision)
            .unwrap();
        assert_eq!(next.tiles.len(), 1);
        assert_eq!(next.tiles[0].bgra.len(), 65536);
        assert_eq!(frame.bgra.len(), 8294400);
        // A server reactivation may reset revision to the same value the UI saw.
        // Generations prevent the previous desktop's cached tiles being reused.
        let mut replacement = Frame::new(1920, 1080).unwrap();
        replacement.update(&pixel).unwrap();
        let reset = replacement
            .snapshot_since(first.generation, first.revision)
            .unwrap();
        assert_ne!(reset.generation, first.generation);
        assert_eq!(reset.tiles.len(), 15 * 9);
    }
    #[test]
    fn compressed_rectangles_with_and_without_header() {
        for flags in [1, 0x401] {
            let mut pixels = vec![];
            if flags == 1 {
                for n in [0, 4, 6, 6] {
                    u16le(&mut pixels, n);
                }
            }
            pixels.extend([0x62, 1, 2, 3]);
            let p = update([0, 0, 1, 0, 2, 1, 24, flags, pixels.len() as u16], &pixels);
            let mut f = Frame::new(2, 1).unwrap();
            f.update(&p).unwrap();
            assert_eq!(f.bgra, [1, 2, 3, 255, 1, 2, 3, 255]);
            assert_eq!(f.compressed_rectangles, 1);
            for n in 0..p.len() {
                assert!(Frame::new(2, 1).unwrap().update(&p[..n]).is_err());
            }
        }
    }
    fn update(rect: [u16; 9], pixels: &[u8]) -> Vec<u8> {
        let mut p = vec![1, 0];
        for n in rect {
            u16le(&mut p, n);
        }
        p.extend(pixels);
        p
    }
    #[test]
    fn padded_bottom_up_bgr() {
        let p = update([0, 0, 0, 1, 1, 2, 24, 0, 8], &[255, 0, 0, 0, 0, 0, 255, 0]);
        let mut f = Frame::new(1, 2).unwrap();
        f.update(&p).unwrap();
        assert_eq!(f.bgra, [0, 0, 255, 255, 255, 0, 0, 255]);
        for n in 0..p.len() {
            assert!(Frame::new(1, 2).unwrap().update(&p[..n]).is_err());
        }
    }
    #[test]
    fn rgb565_and_bounds() {
        let mut f = Frame::new(1, 1).unwrap();
        f.update(&update([0, 0, 0, 0, 1, 1, 16, 0, 4], &[0xe0, 7, 0, 0]))
            .unwrap();
        assert_eq!(f.bgra, [0, 255, 0, 255]);
        assert!(
            f.update(&update([0, 0, 1, 0, 2, 1, 16, 0, 4], &[0; 4]))
                .is_err()
        );
        assert!(
            f.update(&update([0, 0, 0, 0, 1, 1, 16, 1, 4], &[0; 4]))
                .is_err()
        );
        assert!(Frame::new(65535, 65535).is_err());
    }
}
