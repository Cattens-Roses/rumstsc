use windows::Win32::{Graphics::Gdi::*, UI::WindowsAndMessaging::*};
pub(crate) fn create(image: &crate::CustomCursor) -> windows::core::Result<usize> {
    unsafe {
        let width = image.width as usize;
        let height = image.height as usize;
        let stride = width.div_ceil(16) * 2;
        let mut mask = vec![0u8; stride * height];
        for y in 0..height {
            for x in 0..width {
                if image.bgra[(y * width + x) * 4 + 3] == 0 || image.invert[y * width + x] {
                    mask[y * stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        // Windows' monochrome cursor format preserves screen inversion, used
        // by classic text carets and some resize arrows: AND rows, then XOR.
        if image.invert.iter().any(|p| *p) {
            let mut mono = mask.clone();
            mono.resize(stride * height * 2, 0);
            for y in 0..height {
                for x in 0..width {
                    let pixel = &image.bgra[(y * width + x) * 4..][..4];
                    if pixel[3] != 0 && pixel[..3].iter().map(|v| *v as u16).sum::<u16>() >= 384 {
                        mono[stride * height + y * stride + x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            let bitmap = CreateBitmap(
                width as i32,
                (height * 2) as i32,
                1,
                1,
                Some(mono.as_ptr().cast()),
            );
            if bitmap.is_invalid() {
                return Err(windows::core::Error::from_win32());
            }
            let result = CreateIconIndirect(&ICONINFO {
                fIcon: false.into(),
                xHotspot: image.hot_x.into(),
                yHotspot: image.hot_y.into(),
                hbmMask: bitmap,
                ..Default::default()
            });
            let _ = DeleteObject(bitmap.into());
            return result.map(|icon| icon.0 as usize);
        }
        let mask_bitmap = CreateBitmap(
            width as i32,
            height as i32,
            1,
            1,
            Some(mask.as_ptr().cast()),
        );
        if mask_bitmap.is_invalid() {
            return Err(windows::core::Error::from_win32());
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = match CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(color) => color,
            Err(error) => {
                let _ = DeleteObject(mask_bitmap.into());
                return Err(error);
            }
        };
        let pixels = image.premultiplied(1);
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast(), pixels.len());
        let result = CreateIconIndirect(&ICONINFO {
            fIcon: false.into(),
            xHotspot: image.hot_x.into(),
            yHotspot: image.hot_y.into(),
            hbmMask: mask_bitmap,
            hbmColor: color,
        });
        let _ = DeleteObject(mask_bitmap.into());
        let _ = DeleteObject(color.into());
        result.map(|icon| icon.0 as usize)
    }
}
