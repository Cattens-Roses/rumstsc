//! Stateful MS-RDPEGDI primary ScrBlt orders, implemented independently.
use crate::{Error, Result, bitmap::Frame, invalid, wire::Reader};
#[derive(Clone)]
pub struct Orders {
    kind: u8,
    bounds: [i16; 4],
    values: [i16; 6],
    rop: u8,
    cache: crate::cache::Cache,
    memory: [i16; 6],
    memory_rop: u8,
    cache_id: u16,
    cache_index: u16,
}
impl Default for Orders {
    fn default() -> Self {
        Self {
            kind: 1,
            bounds: [0; 4],
            values: [0; 6],
            rop: 0,
            cache: crate::cache::Cache::default(),
            memory: [0; 6],
            memory_rop: 0,
            cache_id: 0,
            cache_index: 0,
        }
    }
}
fn coord(r: &mut Reader<'_>, previous: i16, delta: bool) -> Result<i16> {
    if delta {
        previous
            .checked_add(r.u8()? as i8 as i16)
            .ok_or_else(|| invalid("drawing coordinate overflow"))
    } else {
        Ok(r.u16()? as i16)
    }
}
impl Orders {
    pub(crate) fn reactivate(&mut self) {
        let cache = std::mem::take(&mut self.cache);
        *self = Self::default();
        self.cache = cache;
    }
    /// The updateType has already been consumed by the Share Data parser.
    pub fn update(&mut self, data: &[u8], frame: &mut Frame) -> Result<()> {
        self.update_with_commit(data, frame, |_| {})
    }
    pub(crate) fn update_with_commit(
        &mut self,
        data: &[u8],
        frame: &mut Frame,
        mut commit: impl FnMut(&Frame),
    ) -> Result<()> {
        let mut r = Reader::new(data);
        r.take(2)?;
        let count = r.u16()?;
        r.take(2)?;
        for _ in 0..count {
            let control = r.u8()?;
            if control == 0x36 {
                let action = r.u32()?;
                frame.frame_marker(action)?;
                if action == 1 {
                    commit(frame);
                }
                continue;
            }
            if control & 3 == 3 {
                let length = r.u16()? as i16 as i32 + 13;
                if length < 6 {
                    return Err(invalid("secondary drawing order length"));
                }
                let extra = r.u16()?;
                let kind = r.u8()?;
                self.cache
                    .secondary(kind, extra, r.take(length as usize - 6)?)?;
                continue;
            }
            if control & 3 != 1 {
                return Err(Error::Unsupported(
                    "secondary or alternate graphics order".into(),
                ));
            }
            if control & 8 != 0 {
                self.kind = r.u8()?;
            }
            if ![2, 13].contains(&self.kind) {
                return Err(Error::Unsupported(format!("drawing order {}", self.kind)));
            }
            let field_bytes = if self.kind == 13 { 2 } else { 1 };
            let omitted = (control >> 6) as usize;
            if omitted > field_bytes {
                return Err(invalid("drawing field flags length"));
            }
            let mut fields = 0u16;
            for i in 0..field_bytes - omitted {
                fields |= (r.u8()? as u16) << (i * 8);
            }
            if fields & if self.kind == 13 { !0x01ff } else { !0x007f } != 0 {
                return Err(invalid("drawing field flags"));
            }
            if control & 4 != 0 && control & 0x20 == 0 {
                let flags = r.u8()?;
                for i in 0..4 {
                    // If both bits are present, the delta encoding takes priority.
                    if flags & (0x10 << i) != 0 {
                        self.bounds[i] = coord(&mut r, self.bounds[i], true)?;
                    } else if flags & (1 << i) != 0 {
                        self.bounds[i] = coord(&mut r, self.bounds[i], false)?;
                    }
                }
            }
            if self.kind == 13 {
                let delta = control & 0x10 != 0;
                if fields & 1 != 0 {
                    self.cache_id = r.u16()?;
                }
                for i in 0..4 {
                    if fields & (2 << i) != 0 {
                        self.memory[i] = coord(&mut r, self.memory[i], delta)?;
                    }
                }
                if fields & 0x20 != 0 {
                    self.memory_rop = r.u8()?;
                }
                for i in 4..6 {
                    if fields & (1 << (i + 2)) != 0 {
                        self.memory[i] = coord(&mut r, self.memory[i], delta)?;
                    }
                }
                if fields & 0x100 != 0 {
                    self.cache_index = r.u16()?;
                }
                let source = self.cache.get(self.cache_id, self.cache_index)?;
                let mut values = self.memory;
                values[5] =
                    i16::try_from(source.height as i32 - values[3] as i32 - values[5] as i32)
                        .map_err(|_| invalid("bitmap cache inverted source"))?;
                frame.memory_blt(
                    source,
                    values,
                    self.memory_rop,
                    (control & 4 != 0).then_some(self.bounds),
                )?;
                continue;
            }
            let delta = control & 0x10 != 0;
            for i in 0..4 {
                if fields & (1 << i) != 0 {
                    self.values[i] = coord(&mut r, self.values[i], delta)?;
                }
            }
            if fields & 0x10 != 0 {
                self.rop = r.u8()?;
            }
            for i in 4..6 {
                if fields & (1 << (i + 1)) != 0 {
                    self.values[i] = coord(&mut r, self.values[i], delta)?;
                }
            }
            let clip = (control & 4 != 0).then_some(self.bounds);
            frame.screen_blt(self.values, self.rop, clip)?;
        }
        if r.remaining() != 0 {
            return Err(invalid("trailing drawing order data"));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn update(order: &[u8]) -> Vec<u8> {
        let mut p = vec![0, 0, 1, 0, 0, 0];
        p.extend(order);
        p
    }
    fn full(values: [i16; 6], rop: u8) -> Vec<u8> {
        let mut p = vec![0x09, 2, 0x7f];
        for n in &values[..4] {
            p.extend(n.to_le_bytes());
        }
        p.push(rop);
        for n in &values[4..] {
            p.extend(n.to_le_bytes());
        }
        p
    }
    fn patterned(w: u16, h: u16) -> Frame {
        let mut f = Frame::new(w, h).unwrap();
        for (i, p) in f.bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            p.copy_from_slice(&[(i + 1) as u8, 31, 79, 255]);
        }
        f
    }
    fn batch(orders: &[Vec<u8>]) -> Vec<u8> {
        let mut p = vec![0, 0];
        p.extend((orders.len() as u16).to_le_bytes());
        p.extend([0, 0]);
        for o in orders {
            p.extend(o);
        }
        p
    }
    fn marker(action: u32) -> Vec<u8> {
        let mut p = vec![0x36];
        p.extend(action.to_le_bytes());
        p
    }
    fn cached(compressed: bool, color: u8) -> Vec<u8> {
        let mut body = vec![0, 0, 1, 2, 24];
        let encoded = if compressed {
            vec![0x62, color, 2, 3]
        } else {
            vec![color, 2, 3, 0, color + 1, 2, 3, 0]
        };
        body.extend((encoded.len() as u16).to_le_bytes());
        body.extend(7u16.to_le_bytes());
        body.extend(encoded);
        let mut order = vec![3];
        order.extend(((body.len() + 6) as i16 - 13).to_le_bytes());
        order.extend(if compressed { 0x400u16 } else { 0 }.to_le_bytes());
        order.push(if compressed { 2 } else { 0 });
        order.extend(body);
        order
    }
    fn mem(values: [i16; 6]) -> Vec<u8> {
        let mut p = vec![9, 13, 0xff, 1, 0, 0];
        for n in &values[..4] {
            p.extend(n.to_le_bytes());
        }
        p.push(0xcc);
        for n in &values[4..] {
            p.extend(n.to_le_bytes());
        }
        p.extend(7u16.to_le_bytes());
        p
    }
    #[test]
    fn cache_upload_reuse_replacement_and_bottom_up_source() {
        let mut orders = Orders::default();
        let mut frame = Frame::new(3, 2).unwrap();
        orders
            .update(
                &batch(&[cached(false, 10), mem([0, 0, 1, 1, 0, 0])]),
                &mut frame,
            )
            .unwrap();
        assert_eq!(frame.bgra[0], 10); // y=0 in MemBlt is the bottom cached row.
        orders
            .update(&update(&mem([1, 0, 1, 1, 0, 1])), &mut frame)
            .unwrap();
        assert_eq!(frame.bgra[4], 11);
        orders
            .update(
                &batch(&[cached(true, 44), mem([2, 0, 1, 1, 0, 0])]),
                &mut frame,
            )
            .unwrap();
        assert_eq!(frame.bgra[8], 44);
        assert_eq!(orders.cache.get(0, 7).unwrap().height, 2);
        orders.reactivate();
        orders
            .update(&update(&mem([0, 0, 1, 1, 0, 0])), &mut frame)
            .unwrap();
        assert_eq!(frame.bgra[0], 44);
    }
    #[test]
    fn cache_bounds_and_missing_entries_fail() {
        let mut orders = Orders::default();
        let mut frame = Frame::new(3, 2).unwrap();
        assert!(
            orders
                .update(&update(&mem([0, 0, 1, 1, 0, 0])), &mut frame)
                .is_err()
        );
        let valid = cached(false, 10);
        for end in 0..valid.len() {
            assert!(
                Orders::default()
                    .update(&update(&valid[..end]), &mut frame)
                    .is_err()
            );
        }
        let mut invalid = valid.clone();
        invalid[6] = 3;
        assert!(orders.update(&update(&invalid), &mut frame).is_err());
        orders.update(&update(&valid), &mut frame).unwrap();
        assert!(
            orders
                .update(&update(&mem([0, 0, 2, 1, 0, 0])), &mut frame)
                .is_err()
        );
    }
    #[test]
    fn logical_frames_hide_partial_strips_until_end() {
        let mut frame = patterned(4, 4);
        let mut orders = Orders::default();
        orders
            .update(&update(&full([0, 0, 1, 1, 0, 0], 0xcc)), &mut frame)
            .unwrap();
        let before = frame.snapshot_since(0, 0).unwrap();
        orders
            .update(
                &batch(&[marker(0), full([0, 0, 4, 2, 0, 0], 0xff)]),
                &mut frame,
            )
            .unwrap();
        assert!(
            frame
                .snapshot_since(before.generation, before.revision)
                .is_none()
        );
        orders
            .update(&update(&full([0, 2, 4, 2, 0, 0], 0xff)), &mut frame)
            .unwrap();
        assert!(
            frame
                .snapshot_since(before.generation, before.revision)
                .is_none()
        );
        orders.update(&update(&marker(1)), &mut frame).unwrap();
        let ready = frame
            .snapshot_since(before.generation, before.revision)
            .unwrap();
        assert!(ready.tiles[0].bgra.iter().all(|v| *v == 255));
        assert_eq!(frame.logical_frames, 1);
    }
    #[test]
    fn complete_frame_is_captured_before_next_start_in_same_pdu() {
        let mut frame = patterned(2, 2);
        let mut orders = Orders::default();
        orders
            .update(
                &batch(&[marker(0), full([0, 0, 2, 2, 0, 0], 0xff)]),
                &mut frame,
            )
            .unwrap();
        let mut commits = vec![];
        orders
            .update_with_commit(
                &batch(&[marker(1), marker(0), full([0, 0, 2, 2, 0, 0], 0)]),
                &mut frame,
                |f| commits.push(f.snapshot_since(0, 0).unwrap()),
            )
            .unwrap();
        assert_eq!(commits.len(), 1);
        assert!(commits[0].tiles[0].bgra.iter().all(|v| *v == 255));
        assert!(frame.snapshot_since(0, 0).is_none());
        orders.update(&update(&marker(1)), &mut frame).unwrap();
        assert_eq!(frame.logical_frames, 2);
        assert_eq!(
            &frame.snapshot_since(0, 0).unwrap().tiles[0].bgra[..4],
            &[0, 0, 0, 255]
        );
    }
    #[test]
    fn invalid_frame_marker_sequences_fail() {
        let mut frame = patterned(2, 2);
        let mut orders = Orders::default();
        assert!(orders.update(&update(&marker(1)), &mut frame).is_err());
        assert!(orders.update(&update(&marker(2)), &mut frame).is_err());
        orders.update(&update(&marker(0)), &mut frame).unwrap();
        assert!(orders.update(&update(&marker(0)), &mut frame).is_err());
        for end in 0..5 {
            assert!(
                Orders::default()
                    .update(&update(&marker(0)[..end]), &mut patterned(2, 2))
                    .is_err()
            );
        }
    }
    #[test]
    fn overlapping_screen_copies_match_original_pixels_in_every_direction() {
        for [dx, dy, sx, sy, w, h] in [
            [0, 0, 0, 1, 4, 3],
            [0, 1, 0, 0, 4, 3],
            [0, 0, 1, 0, 3, 4],
            [1, 0, 0, 0, 3, 4],
            [1, 1, 0, 0, 3, 3],
            [0, 0, 1, 1, 3, 3],
        ] {
            let mut frame = patterned(4, 4);
            let before = frame.bgra.clone();
            let mut orders = Orders::default();
            orders
                .update(&update(&full([dx, dy, w, h, sx, sy], 0xcc)), &mut frame)
                .unwrap();
            for y in 0..4 {
                for x in 0..4 {
                    let src = if x >= dx && x < dx + w && y >= dy && y < dy + h {
                        ((sy + y - dy) * 4 + sx + x - dx) as usize * 4
                    } else {
                        (y * 4 + x) as usize * 4
                    };
                    let dst = (y * 4 + x) as usize * 4;
                    assert_eq!(&frame.bgra[dst..dst + 4], &before[src..src + 4]);
                }
            }
            assert_eq!(frame.screen_blits, 1);
        }
    }
    #[test]
    fn every_source_destination_raster_operation_matches_truth_table() {
        for nibble in 0u8..16 {
            let rop = nibble | (nibble << 4);
            let mut frame = patterned(2, 1);
            let original = frame.bgra.clone();
            frame.screen_blt([1, 0, 1, 1, 0, 0], rop, None).unwrap();
            for channel in 0..3 {
                let s = original[channel];
                let d = original[4 + channel];
                let mut expected = 0;
                for bit in 0..8 {
                    let index = ((s >> bit) & 1) * 2 + ((d >> bit) & 1);
                    expected |= ((rop >> index) & 1) << bit;
                }
                assert_eq!(frame.bgra[4 + channel], expected);
            }
            assert_eq!(frame.bgra[7], 255);
        }
    }
    #[test]
    fn delta_fields_and_omitted_flags_reuse_state_across_updates() {
        let mut frame = patterned(8, 8);
        let mut orders = Orders::default();
        orders
            .update(&update(&full([0, 0, 4, 4, 0, 1], 0xcc)), &mut frame)
            .unwrap();
        orders
            .update(&update(&[0x11, 0x42, 1, 1]), &mut frame)
            .unwrap();
        assert_eq!(orders.values, [0, 1, 4, 4, 0, 2]);
        orders.update(&update(&[0x41]), &mut frame).unwrap();
        assert_eq!(orders.rop, 0xcc);
        assert_eq!(frame.screen_blits, 3);
    }
    #[test]
    fn bounds_clip_source_offset_and_zero_bounds_reuse() {
        let mut frame = patterned(4, 4);
        let before = frame.bgra.clone();
        let mut orders = Orders::default();
        let mut order = full([0, 0, 4, 3, 0, 1], 0xcc);
        order[0] |= 4;
        let bounds = [0x0f, 1, 0, 1, 0, 2, 0, 2, 0];
        order.splice(3..3, bounds);
        orders.update(&update(&order), &mut frame).unwrap();
        for y in 0..4 {
            for x in 0..4 {
                let dst = (y * 4 + x) * 4;
                let src = if (1..=2).contains(&x) && (1..=2).contains(&y) {
                    ((y + 1) * 4 + x) * 4
                } else {
                    dst
                };
                assert_eq!(&frame.bgra[dst..dst + 4], &before[src..src + 4]);
            }
        }
        orders.update(&update(&[0x65]), &mut frame).unwrap();
        assert_eq!(orders.bounds, [1, 1, 2, 2]);
    }
    #[test]
    fn clipped_negative_destination_preserves_source_translation() {
        let mut frame = patterned(4, 4);
        let before = frame.bgra.clone();
        frame.screen_blt([-1, -1, 3, 3, 0, 0], 0xcc, None).unwrap();
        for y in 0..2 {
            for x in 0..2 {
                let dst = (y * 4 + x) * 4;
                let src = ((y + 1) * 4 + x + 1) * 4;
                assert_eq!(&frame.bgra[dst..dst + 4], &before[src..src + 4]);
            }
        }
    }
    #[test]
    fn bounds_deltas_override_absolute_bits_and_coordinate_overflow_fails() {
        let mut frame = patterned(4, 4);
        let mut orders = Orders::default();
        let mut order = full([0, 0, 4, 3, 0, 1], 0xcc);
        order[0] |= 4;
        order.splice(3..3, [0x0f, 1, 0, 1, 0, 2, 0, 2, 0]);
        orders.update(&update(&order), &mut frame).unwrap();
        orders
            .update(&update(&[0x45, 0x33, 1, 255]), &mut frame)
            .unwrap();
        assert_eq!(orders.bounds, [2, 0, 2, 2]);
        orders
            .update(&update(&full([0, 0, 32767, 0, 0, 0], 0xff)), &mut frame)
            .unwrap();
        assert!(orders.update(&update(&[0x11, 4, 1]), &mut frame).is_err());
    }
    #[test]
    fn malformed_unadvertised_orders_and_out_of_range_sources_fail() {
        let good = update(&full([0, 0, 2, 2, 0, 1], 0xcc));
        for end in 0..good.len() {
            assert!(
                Orders::default()
                    .update(&good[..end], &mut patterned(4, 4))
                    .is_err()
            );
        }
        for order in [&[3][..], &[9, 0, 0], &[0x89, 2], &[9, 2, 0x80]] {
            assert!(
                Orders::default()
                    .update(&update(order), &mut patterned(4, 4))
                    .is_err()
            );
        }
        assert!(
            patterned(4, 4)
                .screen_blt([0, 0, 4, 4, 1, 0], 0xcc, None)
                .is_err()
        );
        assert!(
            patterned(4, 4)
                .screen_blt([0, 0, 1, 1, 0, 0], 0xf0, None)
                .is_err()
        );
        // Source-independent ROPs do not read invalid source coordinates.
        patterned(4, 4)
            .screen_blt([0, 0, 4, 4, -10, -10], 0xff, None)
            .unwrap();
    }
    #[test]
    fn scroll_moves_large_surface_with_only_order_bytes_and_marks_tiles() {
        let mut frame = Frame::new(1920, 1080).unwrap();
        let mut orders = Orders::default();
        let payload = update(&full([0, 0, 1920, 1000, 0, 80], 0xcc));
        assert_eq!(payload.len(), 22);
        orders.update(&payload, &mut frame).unwrap();
        let first = frame.snapshot_since(0, 0).unwrap();
        orders.update(&update(&[0x41]), &mut frame).unwrap();
        let next = frame
            .snapshot_since(first.generation, first.revision)
            .unwrap();
        assert_eq!(next.tiles.len(), 15 * 8);
        assert_eq!(frame.screen_blits, 2);
    }
}
