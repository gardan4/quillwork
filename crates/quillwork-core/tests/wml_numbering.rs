//! List labels from inline numbering definitions.

mod wml_support;

use quillwork_core::wml::numbering::format_number;
use quillwork_core::wml::{Numbering, WordDoc};
use wml_support::{lvl, mini, p};

fn labels(doc: &WordDoc) -> Vec<Option<String>> {
    let n = Numbering::load(doc);
    n.labels(doc, &doc.stories()[0]).into_iter().map(|(_, l)| l.map(|l| l.text)).collect()
}

fn s(v: &[Option<&str>]) -> Vec<Option<String>> {
    v.iter().map(|x| x.map(str::to_owned)).collect()
}

#[test]
fn multi_level_decimal() {
    let abs = format!(
        r#"<w:abstractNum w:abstractNumId="0">{}{}{}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        lvl(0, 1, "decimal", "%1.", ""),
        lvl(1, 1, "decimal", "%1.%2.", ""),
        lvl(2, 1, "decimal", "%1.%2.%3", "")
    );
    let body = [
        p("a", Some((1, 0)), None),
        p("b", Some((1, 1)), None),
        p("c", Some((1, 2)), None),
        p("d", Some((1, 2)), None),
        p("e", Some((1, 1)), None),
        p("plain", None, None),
        p("f", Some((1, 0)), None),
        p("g", Some((1, 1)), None),
        p("h", Some((1, 2)), None),
    ]
    .concat();
    let doc = mini(&body, Some(&abs), None);
    assert_eq!(
        labels(&doc),
        s(&[
            Some("1."),
            Some("1.1."),
            Some("1.1.1"),
            Some("1.1.2"),
            Some("1.2."),
            None,
            Some("2."),
            Some("2.1."),
            Some("2.1.1")
        ])
    );
}

#[test]
fn letters_roman_and_start() {
    let abs = format!(
        r#"<w:abstractNum w:abstractNumId="0">{}{}{}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        lvl(0, 1, "upperRoman", "%1.", ""),
        lvl(1, 1, "lowerLetter", "%2)", ""),
        lvl(2, 4, "lowerRoman", "(%3)", "")
    );
    let body = [
        p("", Some((1, 0)), None),
        p("", Some((1, 1)), None),
        p("", Some((1, 1)), None),
        p("", Some((1, 2)), None),
        p("", Some((1, 2)), None),
        p("", Some((1, 0)), None),
        p("", Some((1, 0)), None),
        p("", Some((1, 0)), None),
        p("", Some((1, 1)), None),
    ]
    .concat();
    let doc = mini(&body, Some(&abs), None);
    assert_eq!(
        labels(&doc),
        s(&[
            Some("I."),
            Some("a)"),
            Some("b)"),
            Some("(iv)"),
            Some("(v)"),
            Some("II."),
            Some("III."),
            Some("IV."),
            Some("a)")
        ])
    );
}

#[test]
fn format_numbers() {
    assert_eq!(format_number(1, "lowerLetter"), "a");
    assert_eq!(format_number(26, "lowerLetter"), "z");
    assert_eq!(format_number(27, "lowerLetter"), "aa");
    assert_eq!(format_number(28, "upperLetter"), "BB");
    assert_eq!(format_number(1994, "upperRoman"), "MCMXCIV");
    assert_eq!(format_number(49, "lowerRoman"), "xlix");
    assert_eq!(format_number(7, "decimalZero"), "07");
    assert_eq!(format_number(12, "decimal"), "12");
    assert_eq!(format_number(3, "someUnknownFormat"), "3");
}

#[test]
fn lvl_restart_and_is_lgl() {
    // Level 2 restarts only after level 0 (lvlRestart=1), not after level 1.
    // Level 1 shows legal (decimal) numbering of the roman parent.
    let abs = format!(
        r#"<w:abstractNum w:abstractNumId="0">{}{}{}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        lvl(0, 1, "upperRoman", "%1", ""),
        lvl(1, 1, "decimal", "%1.%2", "<w:isLgl/>"),
        lvl(2, 1, "decimal", "n%3", r#"<w:lvlRestart w:val="1"/>"#)
    );
    let body = [
        p("", Some((1, 0)), None),
        p("", Some((1, 2)), None),
        p("", Some((1, 1)), None),
        p("", Some((1, 2)), None),
        p("", Some((1, 0)), None),
        p("", Some((1, 1)), None),
        p("", Some((1, 2)), None),
    ]
    .concat();
    let doc = mini(&body, Some(&abs), None);
    assert_eq!(labels(&doc), s(&[Some("I"), Some("n1"), Some("1.1"), Some("n2"), Some("II"), Some("2.1"), Some("n1")]));
}

#[test]
fn start_override_gets_its_own_sequence() {
    let abs = format!(
        r#"<w:abstractNum w:abstractNumId="0">{}</w:abstractNum>
           <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
           <w:num w:numId="2"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="1"/></w:lvlOverride></w:num>
           <w:num w:numId="3"><w:abstractNumId w:val="0"/></w:num>"#,
        lvl(0, 1, "decimal", "%1.", "")
    );
    let body = [
        p("", Some((1, 0)), None),
        p("", Some((1, 0)), None),
        p("", Some((2, 0)), None),
        p("", Some((2, 0)), None),
        // numId 3 shares the abstract counter with numId 1.
        p("", Some((3, 0)), None),
    ]
    .concat();
    let doc = mini(&body, Some(&abs), None);
    assert_eq!(labels(&doc), s(&[Some("1."), Some("2."), Some("1."), Some("2."), Some("3.")]));
}

#[test]
fn style_linked_numbering_and_num_id_zero() {
    let abs = format!(
        r#"<w:abstractNum w:abstractNumId="0">{}{}</w:abstractNum><w:num w:numId="5"><w:abstractNumId w:val="0"/></w:num>"#,
        lvl(0, 1, "decimal", "Article %1", r#"<w:pStyle w:val="Art"/>"#),
        lvl(1, 1, "decimal", "%1.%2", r#"<w:pStyle w:val="Sub"/>"#)
    );
    // Art carries the numId; Sub is based on Art and only names the level through the
    // abstract level's pStyle. Body inherits Sub's numbering through basedOn.
    let styles = r#"
      <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
      <w:style w:type="paragraph" w:styleId="Art"><w:basedOn w:val="Normal"/><w:pPr><w:numPr><w:numId w:val="5"/></w:numPr></w:pPr></w:style>
      <w:style w:type="paragraph" w:styleId="Sub"><w:basedOn w:val="Art"/></w:style>
      <w:style w:type="paragraph" w:styleId="SubBody"><w:basedOn w:val="Sub"/></w:style>"#;
    let off = r#"<w:p><w:pPr><w:pStyle w:val="Art"/><w:numPr><w:numId w:val="0"/></w:numPr></w:pPr><w:r><w:t>x</w:t></w:r></w:p>"#;
    let body = [
        p("", None, Some("Art")),
        p("", None, Some("Sub")),
        p("", None, Some("SubBody")),
        off.to_owned(),
        p("", None, None),
        p("", None, Some("Art")),
        p("", None, Some("Sub")),
    ]
    .concat();
    let doc = mini(&body, Some(&abs), Some(styles));
    assert_eq!(
        labels(&doc),
        s(&[Some("Article 1"), Some("1.1"), Some("1.2"), None, None, Some("Article 2"), Some("2.1")])
    );
}

#[test]
fn bullets_and_missing_definitions() {
    let abs = format!(
        r#"<w:abstractNum w:abstractNumId="0">{}</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
        lvl(0, 1, "bullet", "\u{2022}", "")
    );
    let body = [p("", Some((1, 0)), None), p("", Some((9, 0)), None), p("", Some((1, 0)), None)].concat();
    let doc = mini(&body, Some(&abs), None);
    assert_eq!(labels(&doc), s(&[Some("\u{2022}"), None, Some("\u{2022}")]));
    // No numbering part at all: nothing numbered, nothing panics.
    let doc = mini(&body, None, None);
    assert_eq!(labels(&doc), vec![None, None, None]);
}
