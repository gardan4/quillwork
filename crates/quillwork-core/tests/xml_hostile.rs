//! Hostile and malformed input: rejected with an error, never a panic, never unbounded work.

mod xml_support;

use quillwork_core::Error;
use quillwork_core::xml::{Document, Limits};

fn err(s: &[u8]) -> Error {
    match Document::parse(s) {
        Ok(_) => panic!("accepted: {}", String::from_utf8_lossy(s)),
        Err(e) => e,
    }
}

#[test]
fn doctype_and_entity_bombs_are_rejected() {
    let laughs = br#"<?xml version="1.0"?>
<!DOCTYPE lolz [
 <!ENTITY lol "lol">
 <!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
 <!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">
]>
<lolz>&lol3;</lolz>"#;
    assert!(matches!(err(laughs), Error::XmlDoctype { .. }));
    assert!(matches!(err(b"<!DOCTYPE a SYSTEM \"file:///etc/passwd\"><a/>"), Error::XmlDoctype { .. }));
    // An undeclared entity without a DTD is not expanded, just rejected.
    assert!(matches!(err(b"<a>&xxe;</a>"), Error::XmlParse { .. }));
    assert!(matches!(err(b"<a b=\"&xxe;\"/>"), Error::XmlParse { .. }));
}

#[test]
fn depth_is_capped() {
    let deep = |n: usize| [&b"<a>".repeat(n)[..], &b"</a>".repeat(n)[..]].concat();
    assert!(Document::parse(&deep(256)).is_ok());
    assert!(matches!(err(&deep(257)), Error::Limit(_)));
    // Far beyond the cap: still a clean error, no stack overflow.
    assert!(matches!(err(&deep(1_000_000)), Error::Limit(_)));
    let limits = Limits { max_depth: 4, ..Limits::default() };
    assert!(Document::parse_with(deep(4), &limits).is_ok());
    assert!(Document::parse_with(deep(5), &limits).is_err());
}

#[test]
fn node_text_and_attr_caps() {
    let limits = Limits { max_nodes: 10, ..Limits::default() };
    let many = [&b"<a>"[..], &b"<b/>".repeat(20), &b"</a>"[..]].concat();
    assert!(matches!(Document::parse_with(many, &limits), Err(Error::Limit(_))));
    let mut d = Document::parse_with(b"<a/>".to_vec(), &limits).unwrap();
    let r = d.root();
    let mut hit = false;
    for _ in 0..20 {
        match d.create_element("x") {
            Ok(x) => d.append_child(r, x).unwrap(),
            Err(Error::Limit(_)) => {
                hit = true;
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(hit, "node cap applies to created nodes too");

    let limits = Limits { max_text_len: 8, ..Limits::default() };
    assert!(Document::parse_with(b"<a>12345678</a>".to_vec(), &limits).is_ok());
    assert!(Document::parse_with(b"<a>123456789</a>".to_vec(), &limits).is_err());
    assert!(Document::parse_with(b"<a>1234&amp;5678</a>".to_vec(), &limits).is_err(), "merged pieces count");
    assert!(Document::parse_with(b"<a><!--123456789--></a>".to_vec(), &limits).is_err());

    let limits = Limits { max_attrs: 3, ..Limits::default() };
    assert!(Document::parse_with(b"<a b='1' c='2' d='3'/>".to_vec(), &limits).is_ok());
    assert!(Document::parse_with(b"<a b='1' c='2' d='3' e='4'/>".to_vec(), &limits).is_err());

    let limits = Limits { max_input_len: 10, ..Limits::default() };
    assert!(Document::parse_with(b"<abcdefghij/>".to_vec(), &limits).is_err());
}

#[test]
fn malformed_documents_are_rejected() {
    let bad: &[&[u8]] = &[
        b"",
        b"   ",
        b"<a>",
        b"<a></b>",
        b"</a>",
        b"<a/><b/>",
        b"text<a/>",
        b"<a/>text",
        b"<a b=\"1\" b=\"2\"/>",
        b"<a b=\"<\"/>",
        b"<a b=1/>",
        b"<a b/>",
        b"<a>&</a>",
        b"<a>&#0;</a>",
        b"<a>&#x110000;</a>",
        b"<a><!-- a -- b --></a>",
        b"<a/><?xml version=\"1.0\"?>",
        b"\xFF\xFE<\x00a\x00/\x00>\x00",
        b"<a>\xC3\x28</a>",
        b"<1a/>",
        b"<a:b:c/>",
        b"<a><![CDATA[x</a>",
        b"<a xmlns:p=\"\"/>",
    ];
    for b in bad {
        let _ = err(b);
    }
}

/// Seeded random corruption of real parts (truncate, flip, insert, delete, splice markup):
/// parse must return Ok or Err, never panic, and anything accepted must round-trip.
#[test]
fn random_mutations_never_panic() {
    let mut seeds: Vec<Vec<u8>> = Vec::new();
    for p in xml_support::synthetic_docx().iter().chain(xml_support::private_docx().iter()) {
        for (_, data) in xml_support::xml_parts(p) {
            // Keep the loop fast: sample small parts whole and a window of big ones.
            seeds.push(if data.len() > 20_000 { data[..20_000].to_vec() } else { data });
        }
    }
    assert!(!seeds.is_empty());
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let tokens: &[&[u8]] = &[
        b"<",
        b">",
        b"/>",
        b"</",
        b"&",
        b"&amp;",
        b"&#",
        b";",
        b"\"",
        b"'",
        b"<!DOCTYPE x>",
        b"<![CDATA[",
        b"]]>",
        b"<!--",
        b"-->",
        b"<?",
        b"?>",
        b"xmlns:w=\"\"",
        b"\r",
        b"\0",
        b"\xF0\x9F",
    ];
    let (mut ok, mut rejected) = (0usize, 0usize);
    for i in 0..4000 {
        let seed = &seeds[i % seeds.len()];
        let mut m = seed.clone();
        for _ in 0..1 + rng.below(4) {
            if m.is_empty() {
                break;
            }
            let at = rng.below(m.len());
            match rng.below(5) {
                0 => m.truncate(at),
                1 => m[at] ^= 1 << rng.below(8),
                2 => {
                    m.remove(at);
                }
                3 => {
                    let t = tokens[rng.below(tokens.len())];
                    m.splice(at..at, t.iter().copied());
                }
                _ => m[at] = rng.below(256) as u8,
            }
        }
        match Document::parse(&m) {
            Ok(mut d) => {
                ok += 1;
                assert!(d.to_bytes() == m, "accepted input must round-trip");
                assert!(d.to_bytes_walked() == m, "accepted input must round-trip (walked)");
                // Exercise a few edits on whatever was accepted.
                let r = d.root();
                let _ = d.set_attr(r, "x", "1 & \"2\"");
                let _ = d.set_text(r, "t");
                let _ = Document::parse(&d.to_bytes()).expect("edited output re-parses");
            }
            Err(_) => rejected += 1,
        }
    }
    eprintln!("random mutations: {ok} accepted (round-tripped), {rejected} rejected, 0 panics");
}

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}
