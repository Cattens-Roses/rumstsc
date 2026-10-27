//! Run `cargo run --manifest-path scripts/icon-export/Cargo.toml` after changing rdpicon.svg.
use std::{error::Error, fs, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = fs::read_to_string(root.join("rdpicon.svg"))?;
    // The monochrome source uses currentColor. Use a visible brand color for
    // OS launchers; GPUI applies the current theme's accent to the original SVG.
    let source = source.replace("color=\"#000000\"", "color=\"#667eea\"");
    let tree = resvg::usvg::Tree::from_str(&source, &resvg::usvg::Options::default())?;
    let mut images = vec![];
    for size in [16u32, 24, 32, 48, 64, 128, 256, 512] {
        fs::create_dir_all(root.join(format!("assets/icons/{size}x{size}")))?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).ok_or("icon size")?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(
                size as f32 / tree.size().width(),
                size as f32 / tree.size().height(),
            ),
            &mut pixmap.as_mut(),
        );
        if size == 64 {
            let mut net_wm_icon = Vec::new();
            net_wm_icon.extend(size.to_le_bytes());
            net_wm_icon.extend(size.to_le_bytes());
            for pixel in pixmap.pixels() {
                let p = pixel.demultiply();
                let argb = ((p.alpha() as u32) << 24)
                    | ((p.red() as u32) << 16)
                    | ((p.green() as u32) << 8)
                    | p.blue() as u32;
                net_wm_icon.extend(argb.to_le_bytes());
            }
            fs::write(root.join("assets/rumstsc.argb"), net_wm_icon)?;
        }
        let png = pixmap.encode_png()?;
        fs::write(
            root.join(format!("assets/icons/{size}x{size}/rumstsc.png")),
            &png,
        )?;
        if size <= 256 {
            images.push((size, png));
        }
    }
    let mut ico = vec![0, 0, 1, 0];
    ico.extend((images.len() as u16).to_le_bytes());
    let mut offset = 6 + images.len() as u32 * 16;
    for (size, png) in &images {
        ico.extend([*size as u8, *size as u8, 0, 0]);
        ico.extend(1u16.to_le_bytes());
        ico.extend(32u16.to_le_bytes());
        ico.extend((png.len() as u32).to_le_bytes());
        ico.extend(offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        ico.extend(png);
    }
    fs::write(root.join("assets/rumstsc.ico"), ico)?;
    fs::write(root.join("assets/rumstsc.svg"), source)?;
    Ok(())
}
