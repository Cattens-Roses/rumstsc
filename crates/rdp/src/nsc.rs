//! NSCodec color planes and run coding used by ClearCodec subrectangles.
use crate::{Result, bitmap::Frame, invalid, wire::Reader};
fn plane(input: &[u8], size: usize) -> Result<Vec<u8>> {
    if input.len() > size || input.is_empty() {
        return Err(invalid("NSCodec plane length"));
    }
    if input.len() == size {
        return Ok(input.to_vec());
    }
    if size < 4 || input.len() < 4 {
        return Err(invalid("NSCodec RLE tail"));
    }
    let mut r = Reader::new(&input[..input.len() - 4]);
    let mut out = Vec::with_capacity(size);
    while r.remaining() > 0 {
        let value = r.u8()?;
        if r.remaining() > 0 {
            // Peek without consuming a following literal: preserve it in a tiny reader.
            let rest = r.rest();
            if rest[0] == value {
                let mut token = Reader::new(rest);
                token.u8()?;
                let length = token.u8()?;
                let length = if length == 255 {
                    token.u32()? as usize
                } else {
                    length as usize + 2
                };
                if length > size - 4 - out.len() {
                    return Err(invalid("NSCodec run expansion"));
                }
                out.resize(out.len() + length, value);
                r = Reader::new(token.rest());
                continue;
            }
            r = Reader::new(rest);
        }
        if out.len() >= size - 4 {
            return Err(invalid("NSCodec literal expansion"));
        }
        out.push(value);
    }
    out.extend_from_slice(&input[input.len() - 4..]);
    if out.len() != size {
        return Err(invalid("NSCodec expanded plane length"));
    }
    Ok(out)
}
pub(crate) fn chroma(value: u8, loss: u8) -> i32 {
    let value = (value as i32) << loss;
    if value & 0x100 != 0 {
        value | !0x1ff
    } else {
        value & 0x1ff
    }
}
pub(crate) fn decode(input: &[u8], width: u16, height: u16) -> Result<Frame> {
    let mut result = Frame::new(width, height)?;
    let w = width as usize;
    let h = height as usize;
    let mut r = Reader::new(input);
    let lengths = [
        r.u32()? as usize,
        r.u32()? as usize,
        r.u32()? as usize,
        r.u32()? as usize,
    ];
    let loss = r.u8()?;
    let sampling = r.u8()?;
    r.u16()?;
    if !(1..=7).contains(&loss) || sampling > 1 {
        return Err(invalid("NSCodec color parameters"));
    }
    let stride = if sampling != 0 { w.div_ceil(8) * 8 } else { w };
    let chroma_stride = if sampling != 0 { stride / 2 } else { w };
    let chroma_height = if sampling != 0 { h.div_ceil(2) } else { h };
    let luma = plane(r.take(lengths[0])?, stride * h)?;
    let co = plane(r.take(lengths[1])?, chroma_stride * chroma_height)?;
    let cg = plane(r.take(lengths[2])?, chroma_stride * chroma_height)?;
    let alpha = if lengths[3] != 0 {
        plane(r.take(lengths[3])?, w * h)?
    } else {
        vec![255; w * h]
    };
    if r.remaining() != 0 {
        return Err(invalid("NSCodec trailing bytes"));
    }
    for y in 0..h {
        for x in 0..w {
            let ci = if sampling != 0 {
                (y / 2) * chroma_stride + x / 2
            } else {
                y * w + x
            };
            let co = chroma(co[ci], loss);
            let cg = chroma(cg[ci], loss);
            let l = luma[y * stride + x] as i32;
            let p = &mut result.bgra[(y * w + x) * 4..][..4];
            p[0] = (l - (co >> 1) - (cg >> 1)).clamp(0, 255) as u8;
            p[1] = (l + (cg >> 1)).clamp(0, 255) as u8;
            p[2] = (l + (co >> 1) - (cg >> 1)).clamp(0, 255) as u8;
            p[3] = alpha[y * w + x];
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ns_codec_runs_and_color_recovery() {
        assert_eq!(
            plane(&[34, 34, 34, 34, 34, 34, 34], 40).unwrap(),
            vec![34; 40]
        );
        assert_eq!(
            plane(&[255, 255, 144, 255, 255, 255, 255], 150).unwrap(),
            vec![255; 150]
        );
        let mut p = vec![];
        for n in [4u32, 4, 4, 0] {
            p.extend(n.to_le_bytes());
        }
        p.extend([1, 0, 0, 0]);
        p.extend([10, 20, 30, 40]);
        p.extend([0; 8]);
        let frame = decode(&p, 2, 2).unwrap();
        assert_eq!(
            frame.bgra,
            [
                10, 10, 10, 255, 20, 20, 20, 255, 30, 30, 30, 255, 40, 40, 40, 255
            ]
        );
        assert!(plane(&[1, 1, 255, 255, 255, 255, 255, 0, 0, 0, 0], 20).is_err());
    }
}
