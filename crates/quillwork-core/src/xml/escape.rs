//! Escaping, unescaping and character/name validation.

use std::borrow::Cow;

/// Is `c` allowed in an XML 1.0 document?
#[inline]
pub(crate) fn is_xml_char(c: char) -> bool {
    match c {
        '\t' | '\n' | '\r' => true,
        c if c < ' ' => false,
        '\u{FFFE}' | '\u{FFFF}' => false,
        _ => true,
    }
}

/// Resolve the body of an entity reference (the part between `&` and `;`).
/// Only the five predefined entities and character references exist: we never load a DTD.
pub(crate) fn resolve_ref(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "apos" => Some('\''),
        "quot" => Some('"'),
        _ => {
            let num = name.strip_prefix('#')?;
            let code = if let Some(hex) = num.strip_prefix('x') {
                if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                u32::from_str_radix(hex, 16).ok()?
            } else {
                if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                num.parse::<u32>().ok()?
            };
            char::from_u32(code).filter(|&c| is_xml_char(c))
        }
    }
}

/// Check that every `&` in `raw` starts a valid reference and that no `<` occurs.
/// Returns the byte offset (within `raw`) of the first problem.
pub(crate) fn validate_raw_value(raw: &str) -> Result<(), (usize, &'static str)> {
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'<' => return Err((i, "'<' in attribute value")),
            b'&' => {
                let rest = &raw[i + 1..];
                let Some(semi) = rest.find(';') else {
                    return Err((i, "unterminated entity reference"));
                };
                if semi > 16 || resolve_ref(&rest[..semi]).is_none() {
                    return Err((i, "unknown or invalid entity reference"));
                }
                i += semi + 2;
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// Turn raw (escaped) source text into its value.
///
/// Text: line ends are normalised (`\r\n` and lone `\r` become `\n`) before references are
/// expanded, so `&#13;` survives as `\r`. Attribute values additionally map literal tab, CR and
/// LF to a space (XML attribute-value normalisation for CDATA-typed attributes).
pub(crate) fn unescape(raw: &str, attr: bool) -> Cow<'_, str> {
    let needs = if attr {
        raw.bytes().any(|b| matches!(b, b'&' | b'\r' | b'\n' | b'\t'))
    } else {
        raw.bytes().any(|b| matches!(b, b'&' | b'\r'))
    };
    if !needs {
        return Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\r' => {
                if chars.peek().map(|&(_, n)| n) == Some('\n') {
                    chars.next();
                }
                out.push(if attr { ' ' } else { '\n' });
            }
            '\n' | '\t' if attr => out.push(' '),
            '&' => {
                let rest = &raw[i + 1..];
                match rest.find(';').and_then(|s| resolve_ref(&rest[..s]).map(|c| (s, c))) {
                    Some((semi, ch)) => {
                        out.push(ch);
                        // Skip the reference body and the ';'.
                        for _ in 0..rest[..=semi].chars().count() {
                            chars.next();
                        }
                    }
                    // Unreachable for validated input; keep the '&' literally rather than fail.
                    None => out.push('&'),
                }
            }
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

fn check_chars(value: &str) -> crate::Result<()> {
    match value.chars().find(|&c| !is_xml_char(c)) {
        None => Ok(()),
        Some(c) => Err(crate::Error::XmlValue(format!("character U+{:04X} is not allowed in XML", c as u32))),
    }
}

/// Escape a text node value. `&`, `<`, `>` become entities and `\r` a character reference
/// (so it is not normalised away on the next parse).
pub(crate) fn escape_text(value: &str) -> crate::Result<String> {
    check_chars(value)?;
    let mut out = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// Escape an attribute value for the given quote character (`"` or `'`).
/// Tab, LF and CR become character references so attribute-value normalisation keeps them.
pub(crate) fn escape_attr(value: &str, quote: u8) -> crate::Result<String> {
    check_chars(value)?;
    let mut out = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if quote == b'"' => out.push_str("&quot;"),
            '\'' if quote == b'\'' => out.push_str("&apos;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// Content for a comment: no `--`, must not end in `-`.
pub(crate) fn check_comment(value: &str) -> crate::Result<()> {
    check_chars(value)?;
    if value.contains("--") || value.ends_with('-') {
        return Err(crate::Error::XmlValue("comment may not contain '--' or end in '-'".into()));
    }
    Ok(())
}

/// Content for a CDATA section: may not contain `]]>`.
pub(crate) fn check_cdata(value: &str) -> crate::Result<()> {
    check_chars(value)?;
    if value.contains("]]>") {
        return Err(crate::Error::XmlValue("CDATA content may not contain ']]>'".into()));
    }
    Ok(())
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || (!c.is_ascii() && c != '\u{FEFF}' && is_xml_char(c))
}

fn is_name_char(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-' || c == '.'
}

fn is_ncname(s: &str) -> bool {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) if is_name_start(c) => cs.all(is_name_char),
        _ => false,
    }
}

/// ASCII fast path for [`check_qname`]: `Some(valid)` when `name` is all ASCII.
#[inline]
fn ascii_qname(name: &[u8]) -> Option<bool> {
    let mut colon = false;
    let mut at_start = true;
    for &b in name {
        if b >= 0x80 {
            return None;
        }
        let ok = if at_start {
            b.is_ascii_alphabetic() || b == b'_'
        } else if b == b':' {
            if colon {
                return Some(false);
            }
            colon = true;
            at_start = true;
            continue;
        } else {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
        };
        if !ok {
            return Some(false);
        }
        at_start = false;
    }
    Some(!at_start)
}

/// Validate a qualified name (`local` or `prefix:local`).
pub(crate) fn check_qname(name: &str) -> crate::Result<()> {
    if let Some(ok) = ascii_qname(name.as_bytes()) {
        return if ok { Ok(()) } else { Err(crate::Error::XmlValue(format!("not a valid qualified name: {name:?}"))) };
    }
    let ok = match name.split_once(':') {
        Some((p, l)) => is_ncname(p) && is_ncname(l),
        None => is_ncname(name),
    };
    if ok { Ok(()) } else { Err(crate::Error::XmlValue(format!("not a valid qualified name: {name:?}"))) }
}

/// `prefix:local` → (`prefix`, `local`); no colon → (`""`, name).
#[inline]
pub(crate) fn split_qname(name: &str) -> (&str, &str) {
    match name.split_once(':') {
        Some((p, l)) => (p, l),
        None => ("", name),
    }
}

/// If `name` declares a namespace, the prefix it binds (`""` for the default namespace).
#[inline]
pub(crate) fn xmlns_prefix(name: &str) -> Option<&str> {
    if name == "xmlns" { Some("") } else { name.strip_prefix("xmlns:") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_text_and_attr() {
        assert_eq!(unescape("a&amp;b&#169;&#xA9;", false), "a&b©©");
        assert_eq!(unescape("x\r\ny\rz", false), "x\ny\nz");
        assert_eq!(unescape("x&#13;y", false), "x\ry");
        assert_eq!(unescape("a\tb\nc\r\nd&#9;", true), "a b c d\t");
        assert!(matches!(unescape("plain", false), Cow::Borrowed(_)));
    }

    #[test]
    fn refs() {
        assert_eq!(resolve_ref("#0"), None);
        assert_eq!(resolve_ref("#x1"), None);
        assert_eq!(resolve_ref("#x110000"), None);
        assert_eq!(resolve_ref("nbsp"), None);
        assert_eq!(resolve_ref("#x20AC"), Some('€'));
    }

    #[test]
    fn escapes() {
        assert_eq!(escape_attr("a\"b'c\t\n\r<&>", b'"').unwrap(), "a&quot;b'c&#x9;&#xA;&#xD;&lt;&amp;&gt;");
        assert_eq!(escape_attr("a\"b'", b'\'').unwrap(), "a\"b&apos;");
        assert_eq!(escape_text("a<b>&\r\n\"").unwrap(), "a&lt;b&gt;&amp;&#xD;\n\"");
        assert!(escape_text("bad\u{1}").is_err());
    }

    #[test]
    fn qnames() {
        assert!(check_qname("w:p").is_ok());
        assert!(check_qname("p").is_ok());
        assert!(check_qname("w:").is_err());
        assert!(check_qname("a:b:c").is_err());
        assert!(check_qname("1a").is_err());
        assert!(check_qname("a b").is_err());
        assert!(check_qname(":a").is_err());
        assert!(check_qname("").is_err());
        assert!(check_qname("_a.b-c:d1").is_ok());
        assert!(check_qname("é:ü").is_ok());
        assert!(check_qname("a:-b").is_err());
    }
}
