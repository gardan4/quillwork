//! Raw zip reading and splicing.
//!
//! We parse the zip structures ourselves (instead of going through a zip library) because the
//! package layer needs the exact byte ranges of every local header, data block and data
//! descriptor, and the raw central-directory records, so that a save can copy untouched entries
//! verbatim. Everything here is bounds-checked; malformed input yields `Error::Package`,
//! limit violations `Error::Limit`, and encrypted input `Error::Encrypted`. Nothing panics.

use std::io::{Read, Write};
use std::ops::Range;

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;

use super::Limits;
use crate::{Error, Result};

const LOCAL_SIG: u32 = 0x0403_4b50;
const CD_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const Z64_EOCD_SIG: u32 = 0x0606_4b50;
const Z64_LOC_SIG: u32 = 0x0706_4b50;
const DD_SIG: u32 = 0x0807_4b50;

const LOCAL_LEN: usize = 30;
const CD_LEN: usize = 46;
const EOCD_LEN: usize = 22;

/// Compound File Binary (OLE) magic: what Office writes for password-protected documents.
const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

pub(crate) const FLAG_ENCRYPTED: u16 = 0x0001;
pub(crate) const FLAG_DESCRIPTOR: u16 = 0x0008;
pub(crate) const FLAG_STRONG_ENCRYPTION: u16 = 0x0040;
pub(crate) const FLAG_UTF8: u16 = 0x0800;

pub(crate) const METHOD_STORED: u16 = 0;
pub(crate) const METHOD_DEFLATE: u16 = 8;
const METHOD_AES: u16 = 99;

fn bad(msg: impl Into<String>) -> Error {
    Error::Package(msg.into())
}

/// Bounds-checked little-endian reads.
fn u16_at(b: &[u8], pos: usize) -> Result<u16> {
    b.get(pos..pos.checked_add(2).ok_or_else(|| bad("offset overflow"))?)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| bad("truncated zip structure"))
}
fn u32_at(b: &[u8], pos: usize) -> Result<u32> {
    b.get(pos..pos.checked_add(4).ok_or_else(|| bad("offset overflow"))?)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| bad("truncated zip structure"))
}
fn u64_at(b: &[u8], pos: usize) -> Result<u64> {
    b.get(pos..pos.checked_add(8).ok_or_else(|| bad("offset overflow"))?)
        .map(|s| {
            let mut a = [0u8; 8];
            a.copy_from_slice(s);
            u64::from_le_bytes(a)
        })
        .ok_or_else(|| bad("truncated zip structure"))
}
fn slice(b: &[u8], start: usize, len: usize) -> Result<Range<usize>> {
    let end = start.checked_add(len).ok_or_else(|| bad("offset overflow"))?;
    if end > b.len() {
        return Err(bad("truncated zip structure"));
    }
    Ok(start..end)
}
fn to_usize(v: u64) -> Result<usize> {
    usize::try_from(v).map_err(|_| bad("offset does not fit in memory"))
}

/// One entry as found in the original bytes.
#[derive(Clone, Debug)]
pub(crate) struct RawEntry {
    pub name: String,
    /// The raw central-directory record (fixed part + name + extra + comment).
    pub cd_record: Range<usize>,
    pub version_made_by: u16,
    pub flags: u16,
    pub method: u16,
    pub mod_time: u16,
    pub mod_date: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    /// Local header start.
    pub local_offset: usize,
    /// Local header flags (may differ from the central copy in bit 3 handling).
    pub local_flags: u16,
    /// Range of the local header's name bytes and extra field.
    pub local_name: Range<usize>,
    pub local_extra: Range<usize>,
    /// Compressed data.
    pub data: Range<usize>,
    /// Local header + data + data descriptor: what a clean save copies verbatim.
    pub span: Range<usize>,
    pub has_descriptor: bool,
}

#[derive(Debug)]
pub(crate) struct Archive {
    pub entries: Vec<RawEntry>,
    /// The end-of-central-directory comment.
    pub comment: Range<usize>,
}

/// Iterate `(id, data range)` over a well-formed extra field. Stops at the first malformed
/// header (some producers pad extras with junk); callers that *need* a field treat a miss as
/// an error.
fn extras(b: &[u8], r: Range<usize>) -> Vec<(u16, Range<usize>)> {
    let mut out = Vec::new();
    let mut p = r.start;
    while p + 4 <= r.end {
        let id = u16::from_le_bytes([b[p], b[p + 1]]);
        let sz = u16::from_le_bytes([b[p + 2], b[p + 3]]) as usize;
        if p + 4 + sz > r.end {
            break;
        }
        out.push((id, p + 4..p + 4 + sz));
        p += 4 + sz;
    }
    out
}

/// Detect an OLE compound file (Office's encrypted-package container, or a legacy .doc).
pub(crate) fn check_not_ole(bytes: &[u8]) -> Result<()> {
    if bytes.len() >= 8 && bytes[..8] == CFB_MAGIC {
        // "EncryptedPackage" as a UTF-16LE directory-entry name.
        let needle: Vec<u8> = "EncryptedPackage".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        if bytes.windows(needle.len()).any(|w| w == needle.as_slice()) {
            return Err(Error::Encrypted(
                "the document is password-protected (OLE EncryptedPackage); remove the protection in Word first".into(),
            ));
        }
        return Err(bad("this is an OLE compound file (legacy .doc or a protected document), not an OOXML package"));
    }
    Ok(())
}

/// Find the end-of-central-directory record. Prefers the last record whose comment runs exactly
/// to the end of the file; falls back to the last record whose comment fits.
fn find_eocd(b: &[u8]) -> Result<usize> {
    if b.len() < EOCD_LEN {
        return Err(bad("not a zip file (too short)"));
    }
    let lowest = b.len().saturating_sub(EOCD_LEN + 0xFFFF);
    let mut fallback = None;
    let mut p = b.len() - EOCD_LEN;
    loop {
        if b[p..p + 4] == EOCD_SIG.to_le_bytes() {
            let clen = u16::from_le_bytes([b[p + 20], b[p + 21]]) as usize;
            let end = p + EOCD_LEN + clen;
            if end == b.len() {
                return Ok(p);
            }
            if end < b.len() && fallback.is_none() {
                fallback = Some(p);
            }
        }
        if p == lowest {
            break;
        }
        p -= 1;
    }
    fallback.ok_or_else(|| bad("not a zip file (no end of central directory)"))
}

/// Decode a zip entry name. UTF-8 when valid (or flagged), otherwise byte-per-char (deterministic
/// and lossless for lookups; OOXML part names are ASCII in practice).
fn decode_name(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_owned(),
        Err(_) => raw.iter().map(|&c| c as char).collect(),
    }
}

/// Validate an entry name against path tricks and (loosely) the OPC part-name grammar.
/// Directory entries (`word/`) are allowed when empty.
pub(crate) fn validate_name(name: &str, is_empty: bool) -> Result<()> {
    if name.is_empty() {
        return Err(bad("empty entry name"));
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(bad(format!("absolute entry name {name:?}")));
    }
    if name.contains('\\') {
        return Err(bad(format!("backslash in entry name {name:?}")));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(bad(format!("control character in entry name {name:?}")));
    }
    let body = match name.strip_suffix('/') {
        Some(dir) if is_empty => dir,
        Some(_) => return Err(bad(format!("non-empty directory entry {name:?}"))),
        None => name,
    };
    for (i, seg) in body.split('/').enumerate() {
        if seg.is_empty() {
            return Err(bad(format!("empty path segment in entry name {name:?}")));
        }
        if seg == "." || seg == ".." {
            return Err(bad(format!("path traversal in entry name {name:?}")));
        }
        if i == 0 && seg.contains(':') {
            return Err(bad(format!("drive or scheme prefix in entry name {name:?}")));
        }
    }
    Ok(())
}

/// Zip64 extended-information extra field values, read in the order the spec mandates (only the
/// fields whose 32-bit slot is saturated are present).
struct Zip64Fields {
    uncompressed: Option<u64>,
    compressed: Option<u64>,
    offset: Option<u64>,
}

fn read_zip64(b: &[u8], extra: Range<usize>, need_u: bool, need_c: bool, need_o: bool) -> Result<Zip64Fields> {
    let mut out = Zip64Fields { uncompressed: None, compressed: None, offset: None };
    if !(need_u || need_c || need_o) {
        return Ok(out);
    }
    let (_, r) = extras(b, extra)
        .into_iter()
        .find(|(id, _)| *id == 0x0001)
        .ok_or_else(|| bad("zip64 value missing its extra field"))?;
    let mut p = r.start;
    let mut take = |want: bool| -> Result<Option<u64>> {
        if !want {
            return Ok(None);
        }
        if p + 8 > r.end {
            return Err(bad("short zip64 extra field"));
        }
        let v = u64_at(b, p)?;
        p += 8;
        Ok(Some(v))
    };
    out.uncompressed = take(need_u)?;
    out.compressed = take(need_c)?;
    out.offset = take(need_o)?;
    Ok(out)
}

/// Parse the archive structure and enforce `limits`. Does not inflate anything.
pub(crate) fn parse(b: &[u8], limits: &Limits) -> Result<Archive> {
    check_not_ole(b)?;
    let eocd = find_eocd(b)?;
    let disk = u16_at(b, eocd + 4)?;
    let cd_disk = u16_at(b, eocd + 6)?;
    let n_disk = u16_at(b, eocd + 8)?;
    let n_total = u16_at(b, eocd + 10)?;
    let cd_size32 = u32_at(b, eocd + 12)?;
    let cd_off32 = u32_at(b, eocd + 16)?;
    let clen = u16_at(b, eocd + 20)? as usize;
    let comment = slice(b, eocd + EOCD_LEN, clen)?;

    let mut n_entries = n_total as u64;
    let mut cd_size = cd_size32 as u64;
    let mut cd_off = cd_off32 as u64;
    // Where the central directory must end at the latest.
    let mut cd_limit = eocd;

    let has_locator = eocd >= 20 && u32_at(b, eocd - 20)? == Z64_LOC_SIG;
    if has_locator {
        let loc = eocd - 20;
        let z_disk = u32_at(b, loc + 4)?;
        let z_off = to_usize(u64_at(b, loc + 8)?)?;
        let z_disks = u32_at(b, loc + 16)?;
        if z_disk != 0 || z_disks > 1 {
            return Err(bad("multi-disk zip64 archives are not supported"));
        }
        if z_off.checked_add(56).is_none_or(|e| e > loc) || u32_at(b, z_off)? != Z64_EOCD_SIG {
            return Err(bad("zip64 locator points at no zip64 end record"));
        }
        let rec_size = u64_at(b, z_off + 4)?;
        if rec_size < 44 || (z_off as u64).saturating_add(12).saturating_add(rec_size) > loc as u64 {
            return Err(bad("zip64 end record overruns its locator"));
        }
        let z_this_disk = u32_at(b, z_off + 16)?;
        let z_cd_disk = u32_at(b, z_off + 20)?;
        let z_n_disk = u64_at(b, z_off + 24)?;
        let z_n_total = u64_at(b, z_off + 32)?;
        let z_cd_size = u64_at(b, z_off + 40)?;
        let z_cd_off = u64_at(b, z_off + 48)?;
        if z_this_disk != 0 || z_cd_disk != 0 || z_n_disk != z_n_total {
            return Err(bad("multi-disk zip64 archives are not supported"));
        }
        // Every non-saturated classic field must agree with its zip64 twin.
        let agree = (n_total == 0xFFFF || n_total as u64 == z_n_total)
            && (n_disk == 0xFFFF || n_disk as u64 == z_n_disk)
            && (cd_size32 == 0xFFFF_FFFF || cd_size32 as u64 == z_cd_size)
            && (cd_off32 == 0xFFFF_FFFF || cd_off32 as u64 == z_cd_off);
        if !agree {
            return Err(bad("zip64 end record disagrees with the classic end record"));
        }
        n_entries = z_n_total;
        cd_size = z_cd_size;
        cd_off = z_cd_off;
        cd_limit = z_off;
    } else {
        if disk != 0 || cd_disk != 0 || n_disk != n_total {
            return Err(bad("multi-disk zip archives are not supported"));
        }
        if cd_off32 == 0xFFFF_FFFF || cd_size32 == 0xFFFF_FFFF {
            return Err(bad("zip64 sentinel without a zip64 end record"));
        }
    }

    if n_entries > limits.max_entries as u64 {
        return Err(Error::Limit(format!("{n_entries} entries (limit {})", limits.max_entries)));
    }
    let cd_start = to_usize(cd_off)?;
    let cd_end = cd_start.checked_add(to_usize(cd_size)?).ok_or_else(|| bad("central directory overflow"))?;
    if cd_end > cd_limit {
        return Err(bad("central directory overruns the end record"));
    }
    // Each record is at least 46 bytes: a cheap sanity check before looping.
    if n_entries.saturating_mul(CD_LEN as u64) > cd_size {
        return Err(bad("central directory too small for its entry count"));
    }

    let mut entries = Vec::with_capacity(n_entries as usize);
    let mut seen = std::collections::HashSet::with_capacity(n_entries as usize);
    let mut total: u64 = 0;
    let mut p = cd_start;
    for _ in 0..n_entries {
        if p + CD_LEN > cd_end || u32_at(b, p)? != CD_SIG {
            return Err(bad("corrupt central directory record"));
        }
        let made_by = u16_at(b, p + 4)?;
        let flags = u16_at(b, p + 8)?;
        let method = u16_at(b, p + 10)?;
        let mod_time = u16_at(b, p + 12)?;
        let mod_date = u16_at(b, p + 14)?;
        let crc = u32_at(b, p + 16)?;
        let csize32 = u32_at(b, p + 20)?;
        let usize32 = u32_at(b, p + 24)?;
        let nlen = u16_at(b, p + 28)? as usize;
        let elen = u16_at(b, p + 30)? as usize;
        let klen = u16_at(b, p + 32)? as usize;
        let disk_start = u16_at(b, p + 34)?;
        let loff32 = u32_at(b, p + 42)?;
        let name_r = slice(b, p + CD_LEN, nlen)?;
        let extra_r = slice(b, name_r.end, elen)?;
        let rec_end = extra_r.end + klen;
        if rec_end > cd_end {
            return Err(bad("central directory record overruns the directory"));
        }
        let cd_record = p..rec_end;
        p = rec_end;

        if flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION) != 0 || method == METHOD_AES {
            return Err(Error::Encrypted("the package contains encrypted entries; remove the protection first".into()));
        }
        if disk_start != 0 && disk_start != 0xFFFF {
            return Err(bad("entry on another disk"));
        }
        let z = read_zip64(b, extra_r.clone(), usize32 == 0xFFFF_FFFF, csize32 == 0xFFFF_FFFF, loff32 == 0xFFFF_FFFF)?;
        let usize_ = z.uncompressed.unwrap_or(usize32 as u64);
        let csize = z.compressed.unwrap_or(csize32 as u64);
        let local_offset = to_usize(z.offset.unwrap_or(loff32 as u64))?;

        let name_bytes = &b[name_r.clone()];
        let name = decode_name(name_bytes);
        validate_name(&name, usize_ == 0)?;
        if !seen.insert(name.to_lowercase()) {
            return Err(bad(format!("duplicate entry name {name:?}")));
        }

        // Limits, on declared sizes; inflation later never produces more than declared.
        if usize_ > limits.max_part_size {
            return Err(Error::Limit(format!(
                "entry {name:?} is {usize_} bytes uncompressed (limit {})",
                limits.max_part_size
            )));
        }
        if usize_ > limits.ratio_min_size && usize_ > csize.saturating_mul(limits.max_ratio) {
            return Err(Error::Limit(format!(
                "entry {name:?} compresses {usize_}:{csize}, over the {}:1 ratio limit",
                limits.max_ratio
            )));
        }
        total = total.saturating_add(usize_);
        if total > limits.max_total_uncompressed {
            return Err(Error::Limit(format!("package is over {} bytes uncompressed", limits.max_total_uncompressed)));
        }
        if method == METHOD_STORED && csize != usize_ {
            return Err(bad(format!("stored entry {name:?} has differing sizes")));
        }

        // Local header.
        let lo = local_offset;
        if lo.checked_add(LOCAL_LEN).is_none_or(|e| e > cd_start) || u32_at(b, lo)? != LOCAL_SIG {
            return Err(bad(format!("entry {name:?}: no local header at its offset")));
        }
        let l_flags = u16_at(b, lo + 6)?;
        let l_method = u16_at(b, lo + 8)?;
        let l_crc = u32_at(b, lo + 14)?;
        let l_csize = u32_at(b, lo + 18)?;
        let l_usize = u32_at(b, lo + 22)?;
        let l_nlen = u16_at(b, lo + 26)? as usize;
        let l_elen = u16_at(b, lo + 28)? as usize;
        let local_name = slice(b, lo + LOCAL_LEN, l_nlen)?;
        let local_extra = slice(b, local_name.end, l_elen)?;
        if &b[local_name.clone()] != name_bytes {
            return Err(bad(format!("entry {name:?}: local and central names differ")));
        }
        if l_flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION) != 0 {
            return Err(Error::Encrypted("the package contains encrypted entries; remove the protection first".into()));
        }
        if l_method != method {
            return Err(bad(format!("entry {name:?}: local and central methods differ")));
        }
        let data_start = local_extra.end;
        let data_end = data_start.checked_add(to_usize(csize)?).ok_or_else(|| bad("entry data overflow"))?;
        if data_end > cd_start {
            return Err(bad(format!("entry {name:?}: data runs into the central directory")));
        }

        let has_descriptor = l_flags & FLAG_DESCRIPTOR != 0;
        let mut span_end = data_end;
        if has_descriptor {
            // Signature optional; sizes 4 or 8 bytes. Pick the reading that agrees with the
            // central directory.
            // An entry whose local extra carries a zip64 record uses 8-byte descriptor sizes.
            let local_z64 = extras(b, local_extra.clone()).iter().any(|(id, _)| *id == 0x0001);
            let candidates: [(usize, bool); 4] = if local_z64 {
                [(24, true), (20, false), (16, true), (12, false)]
            } else {
                [(16, true), (12, false), (24, true), (20, false)]
            };
            let mut found = None;
            for (len, sig) in candidates {
                let Some(end) = data_end.checked_add(len) else { continue };
                if end > cd_start {
                    continue;
                }
                let q = if sig {
                    if u32_at(b, data_end)? != DD_SIG {
                        continue;
                    }
                    data_end + 4
                } else {
                    data_end
                };
                let body = if sig { len - 4 } else { len };
                let (dc, du) = if body == 12 {
                    (u32_at(b, q + 4)? as u64, u32_at(b, q + 8)? as u64)
                } else {
                    (u64_at(b, q + 4)?, u64_at(b, q + 12)?)
                };
                if u32_at(b, q)? == crc && dc == csize && du == usize_ {
                    found = Some(end);
                    break;
                }
            }
            span_end = found.ok_or_else(|| bad(format!("entry {name:?}: data descriptor missing or disagrees")))?;
        } else {
            let ok = l_crc == crc
                && (l_csize == 0xFFFF_FFFF || l_csize as u64 == csize)
                && (l_usize == 0xFFFF_FFFF || l_usize as u64 == usize_);
            if !ok {
                return Err(bad(format!("entry {name:?}: local header disagrees with the central directory")));
            }
        }

        entries.push(RawEntry {
            name,
            cd_record,
            version_made_by: made_by,
            flags,
            method,
            mod_time,
            mod_date,
            crc32: crc,
            compressed_size: csize,
            uncompressed_size: usize_,
            local_offset: lo,
            local_flags: l_flags,
            local_name,
            local_extra,
            data: data_start..data_end,
            span: lo..span_end,
            has_descriptor,
        });
    }
    if p != cd_end {
        return Err(bad("central directory size disagrees with its records"));
    }

    // No two entries may share bytes (the "overlapping files" zip bomb).
    let mut spans: Vec<Range<usize>> = entries.iter().map(|e| e.span.clone()).collect();
    spans.sort_by_key(|r| r.start);
    for w in spans.windows(2) {
        if w[1].start < w[0].end {
            return Err(bad("overlapping zip entries"));
        }
    }

    Ok(Archive { entries, comment })
}

/// Inflate (or copy) an entry's data, never producing more than its declared size, and verify
/// its CRC.
pub(crate) fn inflate(b: &[u8], e: &RawEntry) -> Result<Vec<u8>> {
    let data = &b[e.data.clone()];
    let want = e.uncompressed_size;
    let out = match e.method {
        METHOD_STORED => data.to_vec(),
        METHOD_DEFLATE => {
            let mut out = Vec::with_capacity(want.min(16 << 20) as usize);
            DeflateDecoder::new(data)
                .take(want.saturating_add(1))
                .read_to_end(&mut out)
                .map_err(|err| bad(format!("entry {:?}: corrupt deflate data: {err}", e.name)))?;
            out
        }
        m => return Err(bad(format!("entry {:?}: unsupported compression method {m}", e.name))),
    };
    if out.len() as u64 != want {
        return Err(bad(format!("entry {:?}: inflated to {} bytes, declared {want}", e.name, out.len())));
    }
    if crc32fast::hash(&out) != e.crc32 {
        return Err(bad(format!("entry {:?}: crc mismatch", e.name)));
    }
    Ok(out)
}

/// A sink that accepts output only while it matches `expect`.
struct MatchSink<'a> {
    expect: &'a [u8],
    pos: usize,
}
impl Write for MatchSink<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let end = self.pos + buf.len();
        if end > self.expect.len() || self.expect[self.pos..end] != *buf {
            return Err(std::io::Error::other("diverged"));
        }
        self.pos = end;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn reproduces(plain: &[u8], compressed: &[u8], level: u32) -> bool {
    let mut enc = DeflateEncoder::new(MatchSink { expect: compressed, pos: 0 }, Compression::new(level));
    if enc.write_all(plain).is_err() {
        return false;
    }
    matches!(enc.finish(), Ok(s) if s.pos == compressed.len())
}

/// Guess the deflate level an entry was written with: the zlib level that reproduces its bytes
/// exactly (cheap: each try aborts at the first differing output byte), else 9 when the
/// general-purpose flag bits say "maximum" and 6 otherwise.
pub(crate) fn guess_level(plain: Option<&[u8]>, compressed: &[u8], flags: u16) -> u32 {
    let hint = match (flags >> 1) & 3 {
        1 => 9, // maximum
        2 => 2, // fast
        3 => 1, // super fast
        _ => 6, // normal
    };
    if let Some(plain) = plain {
        let mut order = vec![hint];
        order.extend([6, 9, 1, 5, 7, 8, 4, 3, 2].into_iter().filter(|l| *l != hint));
        for level in order {
            if reproduces(plain, compressed, level) {
                return level;
            }
        }
    }
    // Nothing reproduces the original bytes (Word's compressor is not zlib). The flag bits are
    // a weak hint: Word sets "super fast" yet compresses between zlib 6 and 9, and level 1
    // would grow the part ~20%. Keep "maximum" when flagged, else use zlib's default.
    if hint == 9 { 9 } else { 6 }
}

pub(crate) fn deflate(data: &[u8], level: u32) -> Vec<u8> {
    let mut enc = DeflateEncoder::new(Vec::with_capacity(data.len() / 4 + 64), Compression::new(level));
    // Writing into a Vec cannot fail.
    let _ = enc.write_all(data);
    enc.finish().unwrap_or_default()
}

/// Copy an extra field without its zip64 (0x0001) record: rewritten entries carry their sizes
/// in the 32-bit slots.
fn strip_zip64(b: &[u8], r: Range<usize>) -> Vec<u8> {
    let parsed = extras(b, r.clone());
    let well_formed: usize = parsed.iter().map(|(_, d)| d.len() + 4).sum();
    if well_formed != r.len() {
        // Malformed tail: keep the bytes as they are unless a zip64 record is among the parsed ones.
        if !parsed.iter().any(|(id, _)| *id == 0x0001) {
            return b[r].to_vec();
        }
    }
    let mut out = Vec::with_capacity(r.len());
    for (id, d) in parsed {
        if id == 0x0001 {
            continue;
        }
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&(d.len() as u16).to_le_bytes());
        out.extend_from_slice(&b[d]);
    }
    out
}

/// The zip writer: appends entries and collects central-directory records.
pub(crate) struct Writer<'a> {
    src: &'a [u8],
    pub out: Vec<u8>,
    cd: Vec<u8>,
    count: usize,
}

fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn off32(off: usize) -> Result<u32> {
    u32::try_from(off)
        .ok()
        .filter(|o| *o != u32::MAX)
        .ok_or_else(|| Error::Limit("output over 4 GiB (zip64 output is not supported)".into()))
}

pub(crate) struct NewEntry<'d> {
    pub name: &'d str,
    pub method: u16,
    pub crc32: u32,
    pub uncompressed_size: u64,
    pub compressed: &'d [u8],
    pub version_made_by: u16,
    pub mod_time: u16,
    pub mod_date: u16,
}

impl<'a> Writer<'a> {
    pub fn new(src: &'a [u8], capacity: usize) -> Self {
        Writer { src, out: Vec::with_capacity(capacity), cd: Vec::new(), count: 0 }
    }

    /// Copy a clean entry verbatim; its central record is copied too, with only the local
    /// header offset changed.
    pub fn copy_clean(&mut self, e: &RawEntry) -> Result<()> {
        let b = self.src;
        let new_off = self.out.len();
        self.out.extend_from_slice(&b[e.span.clone()]);
        let rec_start = self.cd.len();
        self.cd.extend_from_slice(&b[e.cd_record.clone()]);
        let rec = &mut self.cd[rec_start..];
        let stored32 = u32::from_le_bytes([rec[42], rec[43], rec[44], rec[45]]);
        if stored32 == 0xFFFF_FFFF {
            // Offset lives in the zip64 extra, after the (optional) sizes.
            let nlen = u16::from_le_bytes([rec[28], rec[29]]) as usize;
            let elen = u16::from_le_bytes([rec[30], rec[31]]) as usize;
            let extra_abs = e.cd_record.start + CD_LEN + nlen;
            let (_, r) = extras(b, extra_abs..extra_abs + elen)
                .into_iter()
                .find(|(id, _)| *id == 0x0001)
                .ok_or_else(|| bad("zip64 extra vanished"))?;
            let mut skip = 0;
            if u32::from_le_bytes([rec[24], rec[25], rec[26], rec[27]]) == 0xFFFF_FFFF {
                skip += 8;
            }
            if u32::from_le_bytes([rec[20], rec[21], rec[22], rec[23]]) == 0xFFFF_FFFF {
                skip += 8;
            }
            let at = r.start - e.cd_record.start + skip;
            rec[at..at + 8].copy_from_slice(&(new_off as u64).to_le_bytes());
        } else {
            rec[42..46].copy_from_slice(&off32(new_off)?.to_le_bytes());
        }
        self.count += 1;
        Ok(())
    }

    /// Write an original entry with new content, keeping every header field we can.
    pub fn write_dirty(&mut self, e: &RawEntry, method: u16, crc: u32, usize_: u64, comp: &[u8]) -> Result<()> {
        let b = self.src;
        let off = off32(self.out.len())?;
        let csize = u32::try_from(comp.len()).map_err(|_| Error::Limit("part over 4 GiB".into()))?;
        let usz = u32::try_from(usize_).map_err(|_| Error::Limit("part over 4 GiB".into()))?;
        let lo = e.local_offset;
        let l_extra = strip_zip64(b, e.local_extra.clone());
        let name = &b[e.local_name.clone()];
        let o = &mut self.out;
        put32(o, LOCAL_SIG);
        o.extend_from_slice(&b[lo + 4..lo + 6]); // version needed
        put16(o, e.local_flags & !FLAG_DESCRIPTOR);
        put16(o, method);
        o.extend_from_slice(&b[lo + 10..lo + 14]); // time, date
        put32(o, crc);
        put32(o, csize);
        put32(o, usz);
        put16(o, name.len() as u16);
        put16(o, l_extra.len() as u16);
        o.extend_from_slice(name);
        o.extend_from_slice(&l_extra);
        o.extend_from_slice(comp);

        let r = e.cd_record.start;
        let nlen = u16_at(b, r + 28)? as usize;
        let elen = u16_at(b, r + 30)? as usize;
        let klen = u16_at(b, r + 32)? as usize;
        let name_r = r + CD_LEN..r + CD_LEN + nlen;
        let extra_r = name_r.end..name_r.end + elen;
        let comment_r = extra_r.end..extra_r.end + klen;
        let c_extra = strip_zip64(b, extra_r);
        let c = &mut self.cd;
        put32(c, CD_SIG);
        c.extend_from_slice(&b[r + 4..r + 8]); // version made by, needed
        put16(c, e.flags & !FLAG_DESCRIPTOR);
        put16(c, method);
        c.extend_from_slice(&b[r + 12..r + 16]); // time, date
        put32(c, crc);
        put32(c, csize);
        put32(c, usz);
        put16(c, nlen as u16);
        put16(c, c_extra.len() as u16);
        put16(c, klen as u16);
        put16(c, 0); // disk start
        c.extend_from_slice(&b[r + 36..r + 42]); // internal + external attributes
        put32(c, off);
        c.extend_from_slice(&b[name_r]);
        c.extend_from_slice(&c_extra);
        c.extend_from_slice(&b[comment_r]);
        self.count += 1;
        Ok(())
    }

    pub fn write_new(&mut self, n: &NewEntry<'_>) -> Result<()> {
        let off = off32(self.out.len())?;
        let csize = u32::try_from(n.compressed.len()).map_err(|_| Error::Limit("part over 4 GiB".into()))?;
        let usz = u32::try_from(n.uncompressed_size).map_err(|_| Error::Limit("part over 4 GiB".into()))?;
        let flags = if n.name.is_ascii() { 0 } else { FLAG_UTF8 };
        let needed: u16 = if n.method == METHOD_DEFLATE { 20 } else { 10 };
        let o = &mut self.out;
        put32(o, LOCAL_SIG);
        put16(o, needed);
        put16(o, flags);
        put16(o, n.method);
        put16(o, n.mod_time);
        put16(o, n.mod_date);
        put32(o, n.crc32);
        put32(o, csize);
        put32(o, usz);
        put16(o, n.name.len() as u16);
        put16(o, 0);
        o.extend_from_slice(n.name.as_bytes());
        o.extend_from_slice(n.compressed);

        let c = &mut self.cd;
        put32(c, CD_SIG);
        put16(c, n.version_made_by);
        put16(c, needed);
        put16(c, flags);
        put16(c, n.method);
        put16(c, n.mod_time);
        put16(c, n.mod_date);
        put32(c, n.crc32);
        put32(c, csize);
        put32(c, usz);
        put16(c, n.name.len() as u16);
        put16(c, 0); // extra
        put16(c, 0); // comment
        put16(c, 0); // disk start
        put16(c, 0); // internal attributes
        put32(c, 0); // external attributes
        put32(c, off);
        c.extend_from_slice(n.name.as_bytes());
        self.count += 1;
        Ok(())
    }

    /// Append the central directory and end record (classic, non-zip64), keeping `comment`.
    pub fn finish(mut self, comment: &[u8]) -> Result<Vec<u8>> {
        let cd_off = off32(self.out.len())?;
        let cd_size = u32::try_from(self.cd.len()).map_err(|_| Error::Limit("central directory too large".into()))?;
        let n = u16::try_from(self.count)
            .ok()
            .filter(|n| *n != u16::MAX)
            .ok_or_else(|| Error::Limit("too many entries for a classic zip".into()))?;
        let cd = std::mem::take(&mut self.cd);
        let o = &mut self.out;
        o.extend_from_slice(&cd);
        put32(o, EOCD_SIG);
        put16(o, 0);
        put16(o, 0);
        put16(o, n);
        put16(o, n);
        put32(o, cd_size);
        put32(o, cd_off);
        put16(o, comment.len() as u16);
        o.extend_from_slice(comment);
        Ok(self.out)
    }
}
