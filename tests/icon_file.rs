//! `assets/platzhirsch.ico`, the icon embedded in `platzhirsch.exe` by `build.rs`, is
//! made from the same drawing as the window icon. The check below fails when the two
//! drift apart; the ignored test writes the file again:
//!
//! `cargo test --test icon_file -- --ignored`
//!
//! It lives here and not in `src/`, because the shipped program never writes files.
use std::path::{Path, PathBuf};

use platzhirsch::ui::icon_rgba;

/// The sizes in the file, smallest first.
const SIZES: [u32; 4] = [16, 32, 48, 256];

fn icon_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("platzhirsch.ico")
}

/// One icon image as a 32-bit DIB: header, colour rows bottom-up in BGRA, then the
/// AND mask (1 bit per pixel, rows padded to 4 bytes, set where the pixel is clear).
fn dib(size: u32) -> Vec<u8> {
    let rgba = icon_rgba(size);
    let n = size as usize;
    let mask_row = n.div_ceil(32) * 4;
    let image_len = n * n * 4 + mask_row * n;
    let mut out = Vec::with_capacity(40 + image_len);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    // The height counts the colour rows and the mask rows.
    out.extend_from_slice(&(2 * size as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(image_len as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for y in (0..n).rev() {
        for px in rgba[y * n * 4..(y + 1) * n * 4].as_chunks::<4>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    for y in (0..n).rev() {
        let mut row = vec![0u8; mask_row];
        for x in 0..n {
            if rgba[(y * n + x) * 4 + 3] == 0 {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }
    out
}

/// The whole `.ico` file: directory, one entry per size, then the images.
fn ico_bytes() -> Vec<u8> {
    let images: Vec<Vec<u8>> = SIZES.iter().map(|&s| dib(s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * SIZES.len();
    for (&size, image) in SIZES.iter().zip(&images) {
        // 256 is written as 0 in the one-byte width and height.
        let side = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in &images {
        out.extend_from_slice(image);
    }
    out
}

#[test]
fn icon_file_matches_the_window_icon() {
    let file = std::fs::read(icon_path()).expect("assets/platzhirsch.ico is missing");
    assert!(
        file == ico_bytes(),
        "assets/platzhirsch.ico differs from the window icon; run `cargo test --test icon_file -- --ignored`"
    );
}

#[test]
fn icon_has_the_size_asked_for() {
    for size in SIZES {
        assert_eq!(icon_rgba(size).len(), (size * size * 4) as usize);
    }
}

#[test]
#[ignore = "writes assets/platzhirsch.ico; run it after changing the icon drawing"]
fn write_icon_file() {
    std::fs::write(icon_path(), ico_bytes()).expect("write assets/platzhirsch.ico");
}
