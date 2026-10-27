//! Image assets: every media part of the package, content-addressed (`a_` + the first 16 hex
//! digits of the SHA-256 of its bytes), with its pixel size when the header says so.
//!
//! The viewer fetches bytes by id through the host; EMF, WMF and TIFF are flagged
//! `needs_conversion` (browsers can't draw them) and the host converts them to PNG.

use std::collections::BTreeMap;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::wml::WordDoc;
use crate::{Error, Result};

/// One media part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Asset {
    /// `a_` + 16 lowercase hex digits.
    pub id: String,
    /// Part name in the package (`word/media/image1.png`).
    pub part: String,
    pub content_type: String,
    pub bytes_len: usize,
    /// Pixel size from the image header (PNG, JPEG, GIF, WebP, BMP), if readable.
    pub px: Option<[u32; 2]>,
    /// The format isn't drawable by a browser (EMF, WMF, TIFF): the host converts it to PNG.
    pub needs_conversion: bool,
}

/// The id of an asset with these bytes.
pub fn asset_id(bytes: &[u8]) -> String {
    let h = Sha256::digest(bytes);
    let mut s = String::with_capacity(18);
    s.push_str("a_");
    for b in &h[..8] {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn is_media(name: &str, ct: Option<&str>) -> bool {
    ct.is_some_and(|c| c.starts_with("image/")) || name.to_ascii_lowercase().contains("/media/")
}

fn content_type(doc: &WordDoc, name: &str) -> String {
    let pkg = doc.package();
    pkg.and_then(|p| p.content_type_of(name))
        .map(str::to_owned)
        .or_else(|| name.rsplit_once('.').and_then(|(_, e)| crate::opc::media_content_type(e)).map(str::to_owned))
        .unwrap_or_else(|| "application/octet-stream".into())
}

/// Content types a browser can't draw.
pub(crate) fn needs_conversion(ct: &str) -> bool {
    matches!(ct, "image/x-emf" | "image/emf" | "image/x-wmf" | "image/wmf" | "image/tiff" | "image/x-tiff")
}

/// Every media part of the package, in part-name order. Identical bytes in two parts give two
/// entries with the same id.
pub fn assets(doc: &WordDoc) -> Vec<Asset> {
    let Some(pkg) = doc.package() else { return Vec::new() };
    let mut names: Vec<&str> = pkg.names().filter(|n| !n.ends_with('/')).collect();
    names.sort_unstable();
    let mut out = Vec::new();
    for name in names {
        let ct = pkg.content_type_of(name);
        if !is_media(name, ct) {
            continue;
        }
        let Ok(bytes) = pkg.part(name) else { continue };
        let ct = content_type(doc, name);
        out.push(Asset {
            id: asset_id(&bytes),
            part: name.to_owned(),
            needs_conversion: needs_conversion(&ct),
            px: pixel_size(&bytes),
            content_type: ct,
            bytes_len: bytes.len(),
        });
    }
    out
}

/// `(content type, bytes)` of the asset with this id.
pub fn asset_bytes(doc: &WordDoc, id: &str) -> Result<(String, Vec<u8>)> {
    let pkg = doc.package().ok_or_else(|| Error::NotFound(format!("asset {id}")))?;
    if !(id.len() == 18 && id.starts_with("a_") && id[2..].bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(Error::NotFound(format!("asset {id}")));
    }
    let mut names: Vec<&str> = pkg.names().collect();
    names.sort_unstable();
    for name in names {
        if !is_media(name, pkg.content_type_of(name)) {
            continue;
        }
        let Ok(bytes) = pkg.part(name) else { continue };
        if asset_id(&bytes) == id {
            return Ok((content_type(doc, name), bytes.into_owned()));
        }
    }
    Err(Error::NotFound(format!("asset {id}")))
}

/// The view model's `assets` map entry for one asset.
pub(crate) fn vm_entry(a: &Asset, extent_emu: Option<(i64, i64)>) -> super::vm::VmAsset {
    let (mime, source) = if a.needs_conversion || !BROWSER.contains(&a.content_type.as_str()) {
        ("image/png".to_owned(), Some(a.content_type.clone()))
    } else {
        (a.content_type.clone(), None)
    };
    let px =
        a.px.or_else(|| {
            // 96 dpi: 9525 EMU per pixel.
            extent_emu.map(|(w, h)| {
                [(w / 9525).clamp(1, u32::MAX as i64) as u32, (h / 9525).clamp(1, u32::MAX as i64) as u32]
            })
        })
        .unwrap_or([1, 1]);
    super::vm::VmAsset { mime, source, px: [px[0].max(1), px[1].max(1)], bytes: a.bytes_len as u64 }
}

const BROWSER: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp", "image/svg+xml"];

/// Collect the assets the given media parts use, keyed by id.
pub(crate) fn by_part(doc: &WordDoc) -> BTreeMap<String, Asset> {
    assets(doc).into_iter().map(|a| (a.part.clone(), a)).collect()
}

// ------------------------------------------------------------------------------ headers

fn be16(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?)))
}
fn le16(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?)))
}
fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}
fn le32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}
fn le24(b: &[u8], i: usize) -> Option<u32> {
    let s = b.get(i..i + 3)?;
    Some(u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16)
}

/// Width and height in pixels from a PNG, JPEG, GIF, WebP or BMP header.
pub(crate) fn pixel_size(b: &[u8]) -> Option<[u32; 2]> {
    let px = if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        [be32(b, 16)?, be32(b, 20)?]
    } else if b.starts_with(b"GIF8") {
        [le16(b, 6)?, le16(b, 8)?]
    } else if b.starts_with(b"BM") {
        [le32(b, 18)?, i32::from_le_bytes(b.get(22..26)?.try_into().ok()?).unsigned_abs()]
    } else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        match b.get(12..16)? {
            b"VP8 " => [le16(b, 26)? & 0x3fff, le16(b, 28)? & 0x3fff],
            b"VP8L" => {
                let v = le32(b, 21)?;
                [(v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1]
            }
            b"VP8X" => [le24(b, 24)? + 1, le24(b, 27)? + 1],
            _ => return None,
        }
    } else if b.starts_with(&[0xFF, 0xD8]) {
        jpeg_size(b)?
    } else {
        return None;
    };
    (px[0] > 0 && px[1] > 0).then_some(px)
}

fn jpeg_size(b: &[u8]) -> Option<[u32; 2]> {
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let m = b[i + 1];
        if m == 0xFF {
            i += 1;
            continue;
        }
        if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
            i += 2;
            continue;
        }
        let len = be16(b, i + 2)? as usize;
        // SOF0..SOF15 except DHT (C4), JPG (C8), DAC (CC).
        if (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
            return Some([be16(b, i + 7)?, be16(b, i + 5)?]);
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let id = asset_id(b"abc");
        assert_eq!(id, "a_ba7816bf8f01cfea");
    }

    #[test]
    fn png_header() {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend_from_slice(&640u32.to_be_bytes());
        b.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(pixel_size(&b), Some([640, 480]));
        assert_eq!(pixel_size(b"nope"), None);
    }

    #[test]
    fn gif_header() {
        let b = b"GIF89a\x10\x00\x20\x00";
        assert_eq!(pixel_size(b), Some([16, 32]));
    }

    #[test]
    fn jpeg_header() {
        // SOI, APP0 (len 4), SOF0 (len 11): precision 8, h 0x0020, w 0x0010.
        let b = [0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 11, 8, 0, 0x20, 0, 0x10, 3, 0, 0, 0];
        assert_eq!(pixel_size(&b), Some([16, 32]));
    }
}
