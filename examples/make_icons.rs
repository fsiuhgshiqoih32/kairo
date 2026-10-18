//! Writes assets/icon.png (256 px) and assets/icon.ico (16-256 px) from the drawn icon.
//! `cargo run --example make_icons`

fn main() {
    std::fs::create_dir_all("assets").unwrap();
    let png = image::RgbaImage::from_raw(256, 256, rewindr::art::app_icon(256)).unwrap();
    png.save("assets/icon.png").unwrap();
    // ICO with PNG-compressed entries.
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let mut images = Vec::new();
    for s in sizes {
        let img = image::RgbaImage::from_raw(s, s, rewindr::art::app_icon(s as usize)).unwrap();
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        images.push((s, buf.into_inner()));
    }
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, data) in &images {
        let b = if *s >= 256 { 0 } else { *s as u8 };
        ico.extend_from_slice(&[b, b, 0, 0, 1, 0, 32, 0]);
        ico.extend_from_slice(&(data.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &images {
        ico.extend_from_slice(data);
    }
    std::fs::write("assets/icon.ico", ico).unwrap();
    println!("wrote assets/icon.png and assets/icon.ico");
}
