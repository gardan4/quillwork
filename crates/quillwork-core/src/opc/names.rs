//! Part-name and relationship-target helpers.

/// Strip one leading `/`: the API accepts both `/word/document.xml` and `word/document.xml`.
pub(crate) fn norm(name: &str) -> &str {
    name.strip_prefix('/').unwrap_or(name)
}

/// Decode `%XX` escapes (invalid escapes are kept literally; invalid UTF-8 is replaced).
pub fn pct_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode what may not appear literally in a part-name URI.
pub fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || "-._~/!$&'()*+,;=:@[]".contains(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// The relationships part for a source part (`""` is the package itself).
///
/// `word/document.xml` → `word/_rels/document.xml.rels`; `""` → `_rels/.rels`.
pub fn rels_part_for(source: &str) -> String {
    let source = norm(source);
    match source.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{source}.rels"),
    }
}

/// The source part of a relationships part, or `None` if `name` is not one.
/// `_rels/.rels` → `Some("")`.
pub fn source_of_rels(name: &str) -> Option<String> {
    let name = norm(name);
    let (dir, file) = match name.rsplit_once('/') {
        Some((d, f)) => (d, f),
        None => return None,
    };
    let file = file.strip_suffix(".rels")?;
    let parent = if dir == "_rels" { "" } else { dir.strip_suffix("/_rels")? };
    Some(if parent.is_empty() { file.to_owned() } else { format!("{parent}/{file}") })
}

/// Resolve an internal relationship target against its source part, returning a part name
/// (no leading `/`, percent-decoded). `None` if the target escapes the package root or looks
/// like a URL.
pub fn resolve_target(source: &str, target: &str) -> Option<String> {
    let target = target.split('#').next().unwrap_or(target);
    if target.is_empty() {
        return None;
    }
    let (base, rel) = match target.strip_prefix('/') {
        Some(abs) => ("", abs),
        None => {
            let first = target.split('/').next().unwrap_or("");
            if first.contains(':') {
                return None; // a scheme: not a part
            }
            (norm(source).rsplit_once('/').map(|(d, _)| d).unwrap_or(""), target)
        }
    };
    let mut segs: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segs.pop()?;
            }
            s => segs.push(s),
        }
    }
    if segs.is_empty() {
        return None;
    }
    Some(pct_decode(&segs.join("/")))
}

/// The relative target to write in `source`'s rels to point at `part`.
pub fn relative_target(source: &str, part: &str) -> String {
    let src_dir: Vec<&str> = norm(source).split('/').collect::<Vec<_>>();
    let src_dir = &src_dir[..src_dir.len().saturating_sub(1)];
    let tgt: Vec<&str> = norm(part).split('/').collect();
    let common = src_dir.iter().zip(&tgt).take_while(|(a, b)| a == b).count();
    let common = common.min(tgt.len().saturating_sub(1));
    let mut out = String::new();
    for _ in common..src_dir.len() {
        out.push_str("../");
    }
    out.push_str(&tgt[common..].join("/"));
    pct_encode(&out)
}
