//! A paragraph whose mark is removed in a view runs on into the next paragraph, but never across
//! a table boundary: not into a table's first cell, not out of a cell, not into the next cell.
//! Both text views ([`text::text`] and [`paragraph_texts`]) must agree.

mod wml_support;

use quillwork_core::wml::view::paragraph_texts;
use quillwork_core::wml::{TextMode, text};
use wml_support::mini;

/// A paragraph holding `text` whose mark is tracked as `kind` (`del` or `ins`).
fn marked(text: &str, kind: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:rPr><w:{kind} w:id="9" w:author="A" w:date="2026-01-01T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
    )
}

fn plain(text: &str) -> String {
    format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
}

fn table(cells: &[&str]) -> String {
    let tcs: String = cells.iter().map(|c| format!("<w:tc>{c}</w:tc>")).collect();
    format!("<w:tbl><w:tr>{tcs}</w:tr></w:tbl>")
}

fn views(body: &str, mode: TextMode) -> (Vec<String>, String) {
    let doc = mini(body, None, None);
    let st = &doc.stories()[0];
    let d = doc.part(st.part);
    (paragraph_texts(d, st.root, mode), text::text(d, st.root, mode))
}

fn check(body: &str, mode: TextMode, want: &[&str]) {
    let (paras, joined) = views(body, mode);
    assert_eq!(paras, want, "paragraph_texts");
    assert_eq!(joined, want.join("\n"), "text");
}

#[test]
fn removed_mark_before_table_keeps_its_line() {
    let body = format!("{}{}{}", marked("Before", "del"), table(&[&plain("Cell")]), plain("After"));
    check(&body, TextMode::AcceptAll, &["Before", "Cell", "After"]);
    let body = format!("{}{}", marked("Before", "ins"), table(&[&plain("Cell")]));
    check(&body, TextMode::RejectAll, &["Before", "Cell"]);
}

#[test]
fn removed_mark_does_not_leave_its_cell() {
    let cell1 = format!("{}{}", plain("A1"), marked("A2", "del"));
    let body = format!("{}{}", table(&[&cell1, &plain("B")]), plain("After"));
    check(&body, TextMode::AcceptAll, &["A1", "A2", "B", "After"]);
    // Last cell of the table: does not run on into the paragraph after it.
    let body = format!("{}{}", table(&[&marked("Last", "del")]), plain("After"));
    check(&body, TextMode::AcceptAll, &["Last", "After"]);
}

#[test]
fn removed_mark_before_nested_table_keeps_its_line() {
    let cell = format!("{}{}{}", marked("Outer", "del"), table(&[&plain("Inner")]), plain("Tail"));
    check(&table(&[&cell]), TextMode::AcceptAll, &["Outer", "Inner", "Tail"]);
}

#[test]
fn removed_mark_still_runs_on_within_a_container() {
    let body = format!("{}{}", marked("One", "del"), plain(" two"));
    check(&body, TextMode::AcceptAll, &["One two"]);
    let cell = format!("{}{}", marked("In", "del"), plain(" cell"));
    check(&table(&[&cell]), TextMode::AcceptAll, &["In cell"]);
    // As-is and the other view keep both paragraphs.
    check(&body, TextMode::AsIs, &["One", " two"]);
    check(&body, TextMode::RejectAll, &["One", " two"]);
}
