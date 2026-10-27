//! Print the projection of a .docx: `cargo run --example project_dump -- FILE [markup] [range] [budget]`.
//! (Development aid; never point it at private documents and keep the output.)

use quillwork_core::anchors::Anchors;
use quillwork_core::project::{self, ReadRequest, View};
use quillwork_core::wml::WordDoc;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("file");
    let mut req = ReadRequest::default();
    for a in args {
        match a.as_str() {
            "markup" => req.view = View::Markup,
            "outline" => req.mode = project::ReadMode::Outline,
            a if a.parse::<usize>().is_ok() => req.budget = a.parse().ok(),
            a => req.range = Some(a.to_owned()),
        }
    }
    req.options.file_name = Some(path.clone());
    let doc = WordDoc::open(std::fs::read(&path).expect("read")).expect("open");
    let anchors = Anchors::mint(&doc);
    let sheet = project::style_sheet(&doc).expect("styles");
    let r = project::read(&doc, &anchors, &sheet, &req).expect("read");
    print!("{}", r.text);
    eprintln!("next={:?} tokens={}", r.next, r.tokens);
}
