//! Style resolution on small inline documents (`wml::style`).

use quillwork_core::wml::style::{
    CondSet, CondType, EffectiveParagraph, EffectiveRun, Script, StyleKind, StyleSheet, TableCtx, TblLook,
};
use quillwork_core::wml::theme::Rgb;
use quillwork_core::xml::{Document, NodeId, ns::W};

const WNS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const ANS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";

fn styles_xml(body: &str) -> Document {
    Document::parse(format!(r#"<w:styles xmlns:w="{WNS}">{body}</w:styles>"#).as_bytes()).expect("styles xml")
}

fn body_xml(body: &str) -> Document {
    Document::parse(format!(r#"<w:document xmlns:w="{WNS}"><w:body>{body}</w:body></w:document>"#).as_bytes())
        .expect("document xml")
}

fn numbering_xml(body: &str) -> Document {
    Document::parse(format!(r#"<w:numbering xmlns:w="{WNS}">{body}</w:numbering>"#).as_bytes()).expect("numbering")
}

fn settings_xml(body: &str) -> Document {
    Document::parse(format!(r#"<w:settings xmlns:w="{WNS}">{body}</w:settings>"#).as_bytes()).expect("settings")
}

fn theme_xml() -> Document {
    let xml = format!(
        r#"<a:theme xmlns:a="{ANS}" name="Office"><a:themeElements>
<a:clrScheme name="Office">
<a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>
<a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
<a:dk2><a:srgbClr val="44546A"/></a:dk2>
<a:lt2><a:srgbClr val="E7E6E6"/></a:lt2>
<a:accent1><a:srgbClr val="4472C4"/></a:accent1>
<a:accent2><a:srgbClr val="ED7D31"/></a:accent2>
<a:accent3><a:srgbClr val="A5A5A5"/></a:accent3>
<a:accent4><a:srgbClr val="FFC000"/></a:accent4>
<a:accent5><a:srgbClr val="5B9BD5"/></a:accent5>
<a:accent6><a:srgbClr val="70AD47"/></a:accent6>
<a:hlink><a:srgbClr val="0563C1"/></a:hlink>
<a:folHlink><a:srgbClr val="954F72"/></a:folHlink>
</a:clrScheme>
<a:fontScheme name="Office">
<a:majorFont><a:latin typeface="Calibri Light"/><a:ea typeface=""/><a:cs typeface=""/>
<a:font script="Jpan" typeface="Yu Gothic Light"/><a:font script="Arab" typeface="Times New Roman"/></a:majorFont>
<a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/>
<a:font script="Jpan" typeface="Yu Mincho"/><a:font script="Arab" typeface="Arial"/></a:minorFont>
</a:fontScheme></a:themeElements></a:theme>"#
    );
    Document::parse(xml.as_bytes()).expect("theme xml")
}

fn sheet(styles: &str) -> StyleSheet {
    StyleSheet::parse(&styles_xml(styles), None).expect("stylesheet")
}

fn sheet_themed(styles: &str) -> StyleSheet {
    StyleSheet::parse(&styles_xml(styles), Some(&theme_xml())).expect("stylesheet")
}

fn paras(d: &Document) -> Vec<NodeId> {
    d.descendants_named(d.root(), W, "p").collect()
}

fn runs(d: &Document, p: NodeId) -> Vec<NodeId> {
    d.descendants_named(p, W, "r").collect()
}

/// Resolve every paragraph (with its table context) and its runs.
fn resolve_all(s: &StyleSheet, d: &Document) -> Vec<(EffectiveParagraph, Vec<EffectiveRun>)> {
    paras(d)
        .into_iter()
        .map(|p| {
            let ep = s.resolve_paragraph_at(d, p);
            let rs = runs(d, p).into_iter().map(|r| s.resolve_run(d, r, &ep)).collect();
            (ep, rs)
        })
        .collect()
}

fn hex_close(got: &str, want: &str) -> bool {
    let (Some(a), Some(b)) = (Rgb::from_hex(got), Rgb::from_hex(want)) else { return false };
    let d = |x: u8, y: u8| (i16::from(x) - i16::from(y)).abs();
    d(a.0, b.0) <= 1 && d(a.1, b.1) <= 1 && d(a.2, b.2) <= 1
}

const NORMAL: &str = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/>
<w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr><w:rPr><w:sz w:val="22"/></w:rPr></w:style>"#;

// ---------------------------------------------------------------------------- docDefaults

#[test]
fn doc_defaults_only() {
    let s = sheet_themed(
        r#"<w:docDefaults>
<w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:hAnsiTheme="minorHAnsi"/><w:sz w:val="24"/><w:lang w:val="nl-NL"/></w:rPr></w:rPrDefault>
<w:pPrDefault><w:pPr><w:spacing w:after="120"/></w:pPr></w:pPrDefault></w:docDefaults>"#,
    );
    assert_eq!(s.default_paragraph_style_id(), None);
    let d = body_xml("<w:p><w:r><w:t>Hello</w:t></w:r></w:p>");
    let all = resolve_all(&s, &d);
    let (p, r) = &all[0];
    assert_eq!(p.style, None);
    assert_eq!(p.spacing.as_ref().and_then(|x| x.after), Some(120));
    assert_eq!(p.justification, None);
    assert_eq!(r[0].size, Some(24));
    assert_eq!(r[0].font.as_deref(), Some("Calibri"));
    assert_eq!(r[0].script, Some(Script::Ascii));
    assert_eq!(r[0].bold, None);
    assert_eq!(r[0].lang.as_ref().and_then(|l| l.val.as_deref()), Some("nl-NL"));
}

#[test]
fn nothing_set_is_none() {
    let s = sheet("");
    let d = body_xml("<w:p><w:r><w:t>x</w:t></w:r></w:p>");
    let all = resolve_all(&s, &d);
    let (p, r) = &all[0];
    assert_eq!(serde_json::to_string(p).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&r[0]).unwrap(), r#"{"script":"ascii"}"#);
}

#[test]
fn rejects_wrong_root() {
    let d = body_xml("");
    assert!(StyleSheet::parse(&d, None).is_err());
}

// ---------------------------------------------------------------------------- basedOn

#[test]
fn based_on_chain() {
    let s = sheet_themed(&format!(
        r#"{NORMAL}
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/>
<w:pPr><w:keepNext/><w:spacing w:before="240" w:after="0"/><w:outlineLvl w:val="0"/></w:pPr>
<w:rPr><w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi"/><w:color w:val="2F5496" w:themeColor="accent1" w:themeShade="BF"/><w:sz w:val="32"/></w:rPr></w:style>
<w:style w:type="paragraph" w:customStyle="1" w:styleId="MyHeading"><w:name w:val="My Heading"/><w:basedOn w:val="Heading1"/>
<w:pPr><w:jc w:val="center"/></w:pPr></w:style>"#
    ));
    let d = body_xml(r#"<w:p><w:pPr><w:pStyle w:val="MyHeading"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#);
    let all = resolve_all(&s, &d);
    let (p, r) = &all[0];
    assert_eq!(p.style.as_deref(), Some("MyHeading"));
    assert_eq!(p.justification.as_deref(), Some("center"));
    assert_eq!(p.keep_next, Some(true));
    assert_eq!(p.outline_level, Some(0));
    let sp = p.spacing.as_ref().unwrap();
    assert_eq!((sp.before, sp.after, sp.line), (Some(240), Some(0), Some(259)));
    assert_eq!(r[0].size, Some(32));
    assert_eq!(r[0].font.as_deref(), Some("Calibri Light"));
    let c = r[0].color.as_deref().unwrap();
    assert!(hex_close(c, "2F5496"), "theme shade gave {c}");

    let chain: Vec<&str> = s.style_chain("MyHeading").iter().map(|x| x.id()).collect();
    assert_eq!(chain, ["MyHeading", "Heading1", "Normal"]);
}

#[test]
fn based_on_cycles_terminate() {
    let s = sheet(
        r#"<w:style w:type="paragraph" w:styleId="A"><w:basedOn w:val="B"/><w:rPr><w:sz w:val="20"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="B"><w:basedOn w:val="A"/><w:rPr><w:sz w:val="30"/><w:i/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Self"><w:basedOn w:val="Self"/><w:rPr><w:b/></w:rPr></w:style>
<w:style w:type="character" w:styleId="C1"><w:basedOn w:val="C2"/><w:rPr><w:b/></w:rPr></w:style>
<w:style w:type="character" w:styleId="C2"><w:basedOn w:val="C1"/><w:rPr><w:caps/></w:rPr></w:style>"#,
    );
    assert!(s.style_chain("A").len() <= 2);
    assert_eq!(s.style_chain("Self").len(), 1);
    let d = body_xml(
        r#"<w:p><w:pPr><w:pStyle w:val="A"/></w:pPr><w:r><w:rPr><w:rStyle w:val="C1"/></w:rPr><w:t>a</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Self"/></w:pPr><w:r><w:t>b</w:t></w:r></w:p>"#,
    );
    let all = resolve_all(&s, &d);
    assert_eq!(all[0].1[0].size, Some(20));
    assert_eq!(all[0].1[0].italic, Some(true));
    assert_eq!(all[0].1[0].bold, Some(true));
    assert_eq!(all[0].1[0].caps, Some(true));
    assert_eq!(all[1].1[0].bold, Some(true));
}

#[test]
fn missing_styles_fall_back() {
    let s = sheet(&format!(
        r#"{NORMAL}<w:style w:type="paragraph" w:styleId="Orphan"><w:basedOn w:val="DoesNotExist"/><w:pPr><w:jc w:val="right"/></w:pPr></w:style>
<w:style w:type="character" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style>"#
    ));
    let d = body_xml(
        r#"<w:p><w:pPr><w:pStyle w:val="Nope"/></w:pPr><w:r><w:rPr><w:rStyle w:val="AlsoNope"/></w:rPr><w:t>a</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Orphan"/></w:pPr><w:r><w:t>b</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Strong"/></w:pPr><w:r><w:t>c</w:t></w:r></w:p>"#,
    );
    let all = resolve_all(&s, &d);
    assert_eq!(all[0].0.style.as_deref(), Some("Normal"));
    assert_eq!(all[0].1[0].style, None);
    assert_eq!(all[0].1[0].size, Some(22));
    assert_eq!(all[1].0.style.as_deref(), Some("Orphan"));
    assert_eq!(all[1].0.justification.as_deref(), Some("end"));
    assert_eq!(all[1].1[0].size, None, "Orphan is not based on Normal");
    // A character style named as a paragraph style is ignored.
    assert_eq!(all[2].0.style.as_deref(), Some("Normal"));
    assert_eq!(all[2].1[0].bold, None);
}

// ---------------------------------------------------------------------------- toggles

const TOGGLE_STYLES: &str = r#"
<w:style w:type="paragraph" w:styleId="BoldPara"><w:name w:val="Bold Para"/><w:rPr><w:b/><w:i/></w:rPr></w:style>
<w:style w:type="character" w:styleId="BoldChar"><w:name w:val="Bold Char"/><w:rPr><w:b/></w:rPr></w:style>
<w:style w:type="character" w:styleId="NotBoldChild"><w:basedOn w:val="BoldChar"/><w:rPr><w:b w:val="0"/></w:rPr></w:style>
<w:style w:type="character" w:styleId="OffChild"><w:basedOn w:val="BoldChar"/><w:rPr><w:b w:val="off"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:link w:val="Heading1Char"/><w:rPr><w:b/></w:rPr></w:style>
<w:style w:type="character" w:styleId="Heading1Char"><w:name w:val="Heading 1 Char"/><w:link w:val="Heading1"/><w:rPr><w:b/></w:rPr></w:style>"#;

#[test]
fn toggles_xor_across_types() {
    let s = sheet(TOGGLE_STYLES);
    let d = body_xml(
        r#"<w:p><w:pPr><w:pStyle w:val="BoldPara"/></w:pPr>
<w:r><w:t>plain</w:t></w:r>
<w:r><w:rPr><w:rStyle w:val="BoldChar"/></w:rPr><w:t>xor</w:t></w:r>
<w:r><w:rPr><w:rStyle w:val="BoldChar"/><w:b/></w:rPr><w:t>direct</w:t></w:r>
<w:r><w:rPr><w:b w:val="false"/></w:rPr><w:t>off</w:t></w:r>
<w:r><w:rPr><w:rStyle w:val="NotBoldChild"/></w:rPr><w:t>chain off</w:t></w:r>
<w:r><w:rPr><w:rStyle w:val="OffChild"/><w:i w:val="0"/></w:rPr><w:t>chain off 2</w:t></w:r>
</w:p>
<w:p><w:r><w:rPr><w:rStyle w:val="BoldChar"/></w:rPr><w:t>outside</w:t></w:r></w:p>"#,
    );
    let all = resolve_all(&s, &d);
    let r = &all[0].1;
    assert_eq!(r[0].bold, Some(true), "paragraph style bold");
    assert_eq!(r[1].bold, Some(false), "bold paragraph style XOR bold character style");
    assert_eq!(r[2].bold, Some(true), "direct formatting wins");
    assert_eq!(r[3].bold, Some(false), "direct val=false");
    assert_eq!(r[4].bold, Some(true), "char chain says not bold, paragraph bold stays");
    assert_eq!(r[5].bold, Some(true));
    assert_eq!(r[5].italic, Some(false), "direct val=0");
    assert_eq!(all[1].1[0].bold, Some(true), "bold char style in a plain paragraph");
}

#[test]
fn linked_character_style_does_not_toggle() {
    let s = sheet(TOGGLE_STYLES);
    let d = body_xml(
        r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:rPr><w:rStyle w:val="Heading1Char"/></w:rPr><w:t>h</w:t></w:r></w:p>"#,
    );
    let all = resolve_all(&s, &d);
    assert_eq!(all[0].1[0].bold, Some(true));
    assert_eq!(all[0].1[0].style.as_deref(), Some("Heading1Char"));
}

// ---------------------------------------------------------------------------- fonts

#[test]
fn theme_fonts_and_scripts() {
    let s = sheet_themed(
        r#"<w:docDefaults><w:rPrDefault><w:rPr>
<w:rFonts w:asciiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia" w:hAnsiTheme="minorHAnsi" w:cstheme="minorBidi"/>
<w:sz w:val="22"/><w:szCs w:val="28"/><w:lang w:val="en-US" w:eastAsia="ja-JP" w:bidi="ar-SA"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="character" w:styleId="Code"><w:rPr><w:rFonts w:ascii="Consolas" w:hAnsi="Consolas"/></w:rPr></w:style>"#,
    );
    let d = body_xml(
        "<w:p><w:r><w:t>Latin</w:t></w:r>\
<w:r><w:t>\u{65e5}\u{672c}</w:t></w:r>\
<w:r><w:rPr><w:rtl/></w:rPr><w:t>abc</w:t></w:r>\
<w:r><w:t>\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}</w:t></w:r>\
<w:r><w:t>\u{00e9}t\u{00e9}</w:t></w:r>\
<w:r><w:rPr><w:rStyle w:val=\"Code\"/></w:rPr><w:t>let x</w:t></w:r>\
<w:r><w:rPr><w:rFonts w:ascii=\"Arial\" w:asciiTheme=\"majorHAnsi\"/></w:rPr><w:t>theme wins</w:t></w:r>\
</w:p>",
    );
    let all = resolve_all(&s, &d);
    let r = &all[0].1;
    assert_eq!((r[0].script, r[0].font.as_deref()), (Some(Script::Ascii), Some("Calibri")));
    assert_eq!((r[1].script, r[1].font.as_deref()), (Some(Script::EastAsia), Some("Yu Mincho")));
    assert_eq!((r[2].script, r[2].font.as_deref()), (Some(Script::ComplexScript), Some("Arial")));
    assert_eq!(r[2].size_for(Script::ComplexScript), Some(28));
    assert_eq!((r[3].script, r[3].font.as_deref()), (Some(Script::ComplexScript), Some("Arial")));
    assert_eq!((r[4].script, r[4].font.as_deref()), (Some(Script::HAnsi), Some("Calibri")));
    assert_eq!(r[5].font.as_deref(), Some("Consolas"));
    assert_eq!(r[5].font_for(Script::EastAsia), Some("Yu Mincho"));
    assert_eq!(r[6].font.as_deref(), Some("Calibri Light"));
}

#[test]
fn theme_font_lang_from_settings() {
    let mut s = sheet_themed(
        r#"<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:eastAsiaTheme="minorEastAsia"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
    );
    let d = body_xml("<w:p><w:r><w:t>\u{65e5}</w:t></w:r></w:p>");
    assert_eq!(resolve_all(&s, &d)[0].1[0].font, None, "ea typeface is empty and no language");
    s.set_settings(&settings_xml(
        r#"<w:themeFontLang w:val="en-US" w:eastAsia="ja-JP"/><w:defaultTabStop w:val="708"/>"#,
    ))
    .unwrap();
    assert_eq!(resolve_all(&s, &d)[0].1[0].font.as_deref(), Some("Yu Mincho"));
    assert_eq!(s.default_tab_stop(), Some(708));
}

#[test]
fn east_asia_hint_moves_shared_characters() {
    let s = sheet(
        r#"<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="A" w:hAnsi="H" w:eastAsia="E" w:cs="C"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
    );
    let d = body_xml(
        "<w:p><w:r><w:t>\u{201c}</w:t></w:r><w:r><w:rPr><w:rFonts w:hint=\"eastAsia\"/></w:rPr><w:t>\u{201c}</w:t></w:r>\
<w:r><w:rPr><w:cs/></w:rPr><w:t>x</w:t></w:r></w:p>",
    );
    let all = resolve_all(&s, &d);
    let r = &all[0].1;
    assert_eq!(r[0].font.as_deref(), Some("H"));
    assert_eq!(r[1].font.as_deref(), Some("E"));
    assert_eq!(r[2].font.as_deref(), Some("C"));
}

// ---------------------------------------------------------------------------- colours

#[test]
fn theme_colours_tint_shade_auto_highlight_shading() {
    let s = sheet_themed("");
    let d = body_xml(
        r#"<w:p><w:pPr><w:shd w:val="clear" w:color="auto" w:fill="E7E6E6" w:themeFill="background2"/></w:pPr>
<w:r><w:rPr><w:color w:val="D9E2F3" w:themeColor="accent1" w:themeTint="33"/></w:rPr><w:t>a</w:t></w:r>
<w:r><w:rPr><w:color w:val="FF0000" w:themeColor="text1"/></w:rPr><w:t>b</w:t></w:r>
<w:r><w:rPr><w:color w:val="auto"/><w:highlight w:val="yellow"/></w:rPr><w:t>c</w:t></w:r>
<w:r><w:rPr><w:color w:val="1F3864" w:themeColor="accent1" w:themeShade="80"/>
<w:shd w:val="clear" w:color="auto" w:fill="FFFFFF" w:themeFill="accent6" w:themeFillTint="66"/>
<w:u w:val="double" w:color="FF0000"/></w:rPr><w:t>d</w:t></w:r>
<w:r><w:rPr><w:color w:val="123456"/></w:rPr><w:t>e</w:t></w:r>
</w:p>"#,
    );
    let all = resolve_all(&s, &d);
    let (p, r) = &all[0];
    let shd = p.shading.as_ref().unwrap();
    assert_eq!(
        (shd.val.as_deref(), shd.color.as_deref(), shd.fill.as_deref()),
        (Some("clear"), Some("auto"), Some("E7E6E6"))
    );
    let tint = r[0].color.as_deref().unwrap();
    assert!(hex_close(tint, "D9E2F3"), "tint gave {tint}");
    assert_eq!(r[1].color.as_deref(), Some("000000"), "theme colour beats w:val");
    assert_eq!(r[2].color.as_deref(), Some("auto"));
    assert_eq!(r[2].highlight.as_deref(), Some("yellow"));
    let shade = r[3].color.as_deref().unwrap();
    assert!(hex_close(shade, "1F3864"), "shade gave {shade}");
    let fill = r[3].shading.as_ref().and_then(|x| x.fill.as_deref()).unwrap();
    assert!(hex_close(fill, "C5E0B3"), "fill tint gave {fill}");
    let u = r[3].underline.as_ref().unwrap();
    assert_eq!((u.val.as_deref(), u.color.as_deref()), (Some("double"), Some("FF0000")));
    assert_eq!(r[4].color.as_deref(), Some("123456"));
}

#[test]
fn colour_scheme_mapping_from_settings() {
    let mut s = sheet_themed("");
    s.set_settings(&settings_xml(r#"<w:clrSchemeMapping w:bg1="dark1" w:t1="light1" w:bg2="dark2" w:t2="light2"/>"#))
        .unwrap();
    let d =
        body_xml(r#"<w:p><w:r><w:rPr><w:color w:val="000000" w:themeColor="text1"/></w:rPr><w:t>a</w:t></w:r></w:p>"#);
    assert_eq!(resolve_all(&s, &d)[0].1[0].color.as_deref(), Some("FFFFFF"));
}

#[test]
fn colours_without_theme_use_the_stored_value() {
    let s = sheet("");
    let d = body_xml(
        r#"<w:p><w:r><w:rPr><w:color w:val="2F5496" w:themeColor="accent1" w:themeShade="BF"/></w:rPr><w:t>a</w:t></w:r></w:p>"#,
    );
    assert_eq!(resolve_all(&s, &d)[0].1[0].color.as_deref(), Some("2F5496"));
}

// ---------------------------------------------------------------------------- run properties

#[test]
fn misc_run_properties() {
    let s = sheet("");
    let d = body_xml(
        r#"<w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/><w:position w:val="-4"/><w:kern w:val="28"/>
<w:spacing w:val="20"/><w:w w:val="90"/><w:caps/><w:smallCaps w:val="0"/><w:strike/><w:dstrike w:val="1"/>
<w:vanish/><w:lang w:val="nl-NL" w:eastAsia="en-US" w:bidi="ar-SA"/><w:bdr w:val="single" w:sz="4" w:space="0" w:color="auto"/>
<w:em w:val="dot"/><w:noProof/></w:rPr><w:t>x</w:t></w:r></w:p>"#,
    );
    let r = &resolve_all(&s, &d)[0].1[0];
    assert_eq!(r.vert_align.as_deref(), Some("superscript"));
    assert_eq!(r.position, Some(-4));
    assert_eq!(r.kern, Some(28));
    assert_eq!(r.spacing, Some(20));
    assert_eq!(r.scale, Some(90));
    assert_eq!(
        (r.caps, r.small_caps, r.strike, r.dstrike, r.vanish),
        (Some(true), Some(false), Some(true), Some(true), Some(true))
    );
    let l = r.lang.as_ref().unwrap();
    assert_eq!(
        (l.val.as_deref(), l.east_asia.as_deref(), l.bidi.as_deref()),
        (Some("nl-NL"), Some("en-US"), Some("ar-SA"))
    );
    let b = r.border.as_ref().unwrap();
    assert_eq!((b.val.as_deref(), b.sz, b.color.as_deref()), (Some("single"), Some(4), Some("auto")));
    assert_eq!(r.emphasis_mark.as_deref(), Some("dot"));
    assert_eq!(r.no_proof, Some(true));
}

#[test]
fn run_serialises_camel_case() {
    let s = sheet("");
    let d = body_xml(r#"<w:p><w:r><w:rPr><w:smallCaps/><w:sz w:val="20"/></w:rPr><w:t>x</w:t></w:r></w:p>"#);
    let r = &resolve_all(&s, &d)[0].1[0];
    let v: serde_json::Value = serde_json::to_value(r).unwrap();
    assert_eq!(v["smallCaps"], true);
    assert_eq!(v["size"], 20);
    assert!(v.get("bold").is_none());
    let back: EffectiveRun = serde_json::from_value(v).unwrap();
    assert_eq!(&back, r);
}

// ---------------------------------------------------------------------------- paragraph properties

#[test]
fn indentation_justification_tabs_borders() {
    let s = sheet_themed(&format!(
        r#"{NORMAL}<w:style w:type="paragraph" w:styleId="Body"><w:basedOn w:val="Normal"/>
<w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/><w:tab w:val="center" w:pos="4536"/></w:tabs>
<w:ind w:left="567" w:firstLine="284"/><w:jc w:val="left"/><w:keepLines/><w:widowControl w:val="0"/>
<w:pBdr><w:bottom w:val="single" w:sz="6" w:space="1" w:color="4472C4" w:themeColor="accent1"/></w:pBdr></w:pPr></w:style>"#
    ));
    let d = body_xml(
        r#"<w:p><w:pPr><w:pStyle w:val="Body"/><w:tabs><w:tab w:val="clear" w:pos="720"/><w:tab w:val="right" w:leader="dot" w:pos="9072"/></w:tabs>
<w:ind w:hanging="360"/><w:jc w:val="both"/></w:pPr></w:p>
<w:p><w:pPr><w:pStyle w:val="Body"/><w:ind w:end="100"/><w:jc w:val="right"/><w:bidi/></w:pPr></w:p>"#,
    );
    let all = resolve_all(&s, &d);
    let p = &all[0].0;
    let tabs: Vec<(&str, i32, Option<&str>)> =
        p.tabs.iter().map(|t| (t.val.as_str(), t.pos, t.leader.as_deref())).collect();
    assert_eq!(tabs, [("center", 4536, None), ("right", 9072, Some("dot"))]);
    let ind = p.indent.as_ref().unwrap();
    assert_eq!((ind.start, ind.hanging, ind.first_line), (Some(567), Some(360), None));
    assert_eq!(p.justification.as_deref(), Some("both"));
    assert_eq!(p.keep_lines, Some(true));
    assert_eq!(p.widow_control, Some(false));
    let b = p.borders.as_ref().and_then(|b| b.bottom.as_ref()).unwrap();
    assert_eq!(
        (b.val.as_deref(), b.sz, b.space, b.color.as_deref()),
        (Some("single"), Some(6), Some(1), Some("4472C4"))
    );

    let q = &all[1].0;
    assert_eq!(q.tabs.len(), 2);
    let ind = q.indent.as_ref().unwrap();
    assert_eq!((ind.start, ind.end, ind.first_line), (Some(567), Some(100), Some(284)));
    assert_eq!(q.justification.as_deref(), Some("end"));
    assert_eq!(q.bidi, Some(true));
}

// ---------------------------------------------------------------------------- numbering

const NUMBERING: &str = r#"
<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr><w:rPr><w:b/><w:color w:val="FF0000"/></w:rPr></w:lvl>
<w:lvl w:ilvl="1"><w:pPr><w:ind w:left="1440" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum>
<w:abstractNum w:abstractNumId="1"><w:numStyleLink w:val="ListStyle"/></w:abstractNum>
<w:abstractNum w:abstractNumId="2"><w:styleLink w:val="ListStyle"/><w:lvl w:ilvl="0"><w:pPr><w:ind w:left="360" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:num w:numId="2"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:lvl w:ilvl="0"><w:pPr><w:ind w:left="2000" w:hanging="500"/></w:pPr></w:lvl></w:lvlOverride></w:num>
<w:num w:numId="3"><w:abstractNumId w:val="1"/></w:num>
<w:num w:numId="4"><w:abstractNumId w:val="2"/></w:num>"#;

#[test]
fn numbering_level_indentation() {
    let mut s = sheet(&format!(
        r#"{NORMAL}<w:style w:type="paragraph" w:styleId="ListParagraph"><w:name w:val="List Paragraph"/><w:basedOn w:val="Normal"/>
<w:pPr><w:numPr><w:numId w:val="1"/></w:numPr><w:ind w:left="1080"/><w:contextualSpacing/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Indented"><w:basedOn w:val="Normal"/><w:pPr><w:ind w:left="1080"/></w:pPr></w:style>
<w:style w:type="numbering" w:styleId="ListStyle"><w:pPr><w:numPr><w:numId w:val="4"/></w:numPr></w:pPr></w:style>"#
    ));
    s.set_numbering(&numbering_xml(NUMBERING)).unwrap();
    let d = body_xml(
        r#"<w:p><w:pPr><w:pStyle w:val="ListParagraph"/></w:pPr><w:r><w:t>style numbering</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>direct</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Indented"/><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr></w:p>
<w:p><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr><w:ind w:left="100"/></w:pPr></w:p>
<w:p><w:pPr><w:numPr><w:numId w:val="2"/></w:numPr></w:pPr></w:p>
<w:p><w:pPr><w:numPr><w:numId w:val="3"/></w:numPr></w:pPr></w:p>
<w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:numId w:val="0"/></w:numPr></w:pPr></w:p>
<w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:ilvl w:val="1"/></w:numPr></w:pPr></w:p>"#,
    );
    let all = resolve_all(&s, &d);
    let ind = |i: usize| all[i].0.indent.as_ref().map(|x| (x.start, x.hanging));
    // Style numbering sits below the style: the style's own indent wins, hanging comes from the level.
    assert_eq!(ind(0), Some((Some(1080), Some(360))));
    assert_eq!(all[0].0.numbering.map(|n| (n.num_id, n.ilvl)), Some((1, 0)));
    assert_eq!(all[0].0.contextual_spacing, Some(true));
    // The level's run properties do not apply to the text.
    assert_eq!(all[0].1[0].bold, None);
    // Direct numbering sits above the style.
    assert_eq!(ind(1), Some((Some(720), Some(360))));
    assert_eq!(ind(2), Some((Some(1440), Some(360))));
    // Direct indentation wins over everything.
    assert_eq!(ind(3), Some((Some(100), Some(360))));
    // lvlOverride.
    assert_eq!(ind(4), Some((Some(2000), Some(500))));
    // numStyleLink through a numbering style.
    assert_eq!(ind(5), Some((Some(360), Some(360))));
    // numId 0 turns numbering off.
    assert_eq!(all[6].0.numbering, None);
    assert_eq!(ind(6), Some((Some(1080), None)));
    // ilvl from the paragraph, numId from the style.
    assert_eq!(all[7].0.numbering.map(|n| (n.num_id, n.ilvl)), Some((1, 1)));
    assert_eq!(ind(7), Some((Some(1080), Some(360))));

    // The list label gets the level's rPr.
    let p0 = paras(&d)[0];
    let label = s.resolve_list_label(&d, p0, &all[0].0).unwrap();
    assert_eq!((label.bold, label.color.as_deref(), label.size), (Some(true), Some("FF0000"), Some(22)));
    assert!(s.resolve_list_label(&d, paras(&d)[6], &all[6].0).is_none());
}

// ---------------------------------------------------------------------------- tables

const TABLE_STYLES: &str = r#"
<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/>
<w:tblPr><w:tblInd w:w="0" w:type="dxa"/></w:tblPr></w:style>
<w:style w:type="table" w:styleId="Grid"><w:name w:val="Grid Table"/>
<w:pPr><w:spacing w:after="0"/></w:pPr><w:rPr><w:sz w:val="20"/></w:rPr>
<w:tblPr><w:tblStyleRowBandSize w:val="1"/><w:tblStyleColBandSize w:val="1"/></w:tblPr>
<w:tcPr><w:tcBorders><w:top w:val="single" w:sz="4" w:color="auto"/></w:tcBorders></w:tcPr>
<w:tblStylePr w:type="firstRow"><w:rPr><w:b/><w:color w:val="FFFFFF" w:themeColor="background1"/></w:rPr>
<w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="4472C4" w:themeFill="accent1"/></w:tcPr></w:tblStylePr>
<w:tblStylePr w:type="lastCol"><w:rPr><w:i/></w:rPr></w:tblStylePr>
<w:tblStylePr w:type="band1Horz"><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="D9E2F3"/></w:tcPr></w:tblStylePr>
<w:tblStylePr w:type="band2Horz"><w:pPr><w:jc w:val="center"/></w:pPr></w:tblStylePr>
<w:tblStylePr w:type="neCell"><w:rPr><w:u w:val="single"/></w:rPr></w:tblStylePr>
</w:style>
<w:style w:type="table" w:styleId="Wide"><w:basedOn w:val="Grid"/><w:tblPr><w:tblStyleRowBandSize w:val="2"/></w:tblPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="240"/></w:pPr><w:rPr><w:b/></w:rPr></w:style>"#;

fn table(style: &str, look: &str, rows: usize, cols: usize) -> String {
    let mut t = format!(r#"<w:tbl><w:tblPr>{style}{look}</w:tblPr>"#);
    for r in 0..rows {
        t.push_str("<w:tr>");
        for c in 0..cols {
            t.push_str(&format!("<w:tc><w:p><w:r><w:t>{r}{c}</w:t></w:r></w:p></w:tc>"));
        }
        t.push_str("</w:tr>");
    }
    t.push_str("</w:tbl>");
    t
}

#[test]
fn table_conditional_formatting() {
    let s = sheet_themed(&format!("{NORMAL}{TABLE_STYLES}"));
    let d = body_xml(&table(
        r#"<w:tblStyle w:val="Grid"/>"#,
        r#"<w:tblLook w:val="0520" w:firstRow="1" w:lastRow="0" w:firstColumn="0" w:lastColumn="1" w:noHBand="0" w:noVBand="1"/>"#,
        4,
        3,
    ));
    let all = resolve_all(&s, &d);
    let cell = |r: usize, c: usize| &all[r * 3 + c];
    // Whole table: size and spacing override Normal.
    assert_eq!(cell(1, 0).1[0].size, Some(20));
    assert_eq!(cell(1, 0).0.spacing.as_ref().and_then(|x| x.after), Some(0));
    assert_eq!(cell(1, 0).0.spacing.as_ref().and_then(|x| x.line), Some(259), "Normal's other spacing stays");
    // firstRow.
    assert_eq!(cell(0, 0).1[0].bold, Some(true));
    assert_eq!(cell(0, 0).1[0].color.as_deref(), Some("FFFFFF"));
    assert_eq!(cell(1, 0).1[0].bold, None);
    // lastCol.
    assert_eq!(cell(2, 2).1[0].italic, Some(true));
    assert_eq!(cell(2, 1).1[0].italic, None);
    // neCell = firstRow ∩ lastCol.
    assert!(cell(0, 2).1[0].underline.is_some());
    assert!(cell(0, 1).1[0].underline.is_none());
    // Row banding starts after the header row: rows 1 and 3 are band1, row 2 band2.
    assert_eq!(cell(1, 0).0.justification, None);
    assert_eq!(cell(2, 0).0.justification.as_deref(), Some("center"));
    assert_eq!(cell(3, 0).0.justification, None);

    // Cells.
    let tcs: Vec<NodeId> = d.descendants_named(d.root(), W, "tc").collect();
    let c = |r: usize, col: usize| {
        let tc = tcs[r * 3 + col];
        s.resolve_cell(&d, tc, &TableCtx::of(&s, &d, tc))
    };
    assert_eq!(c(0, 0).shading.and_then(|x| x.fill).as_deref(), Some("4472C4"));
    assert_eq!(c(1, 0).shading.and_then(|x| x.fill).as_deref(), Some("D9E2F3"));
    assert_eq!(c(2, 0).shading, None);
    assert_eq!(c(2, 0).borders.and_then(|b| b.top).and_then(|t| t.val).as_deref(), Some("single"));
}

#[test]
fn table_style_vs_paragraph_styles() {
    let s = sheet(&format!("{NORMAL}{TABLE_STYLES}"));
    let mut xml = table(r#"<w:tblStyle w:val="Grid"/>"#, r#"<w:tblLook w:val="04A0"/>"#, 2, 1);
    xml = xml.replacen("<w:p><w:r>", r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r>"#, 1);
    let d = body_xml(&xml);
    let all = resolve_all(&s, &d);
    // A non-default paragraph style beats the table style.
    assert_eq!(all[0].0.spacing.as_ref().and_then(|x| x.after), Some(240));
    // Heading1 bold XOR firstRow bold.
    assert_eq!(all[0].1[0].bold, Some(false));
    assert_eq!(all[1].0.spacing.as_ref().and_then(|x| x.after), Some(0));
}

#[test]
fn table_banding_sizes_and_defaults() {
    let s = sheet(&format!("{NORMAL}{TABLE_STYLES}"));
    // Wide: band size 2 from the style; no tblLook (all off, banding on).
    let d = body_xml(&table(r#"<w:tblStyle w:val="Wide"/>"#, "", 6, 1));
    let all = resolve_all(&s, &d);
    let centered: Vec<bool> = all.iter().map(|(p, _)| p.justification.is_some()).collect();
    assert_eq!(centered, [false, false, true, true, false, false]);
    assert!(all.iter().all(|(_, r)| r[0].bold.is_none()), "no firstRow without tblLook");

    // No tblStyle → default table style; unknown tblStyle → default table style.
    let d = body_xml(&table(r#"<w:tblStyle w:val="Missing"/>"#, "", 1, 1));
    let p = paras(&d)[0];
    assert_eq!(TableCtx::of(&s, &d, p).style.as_deref(), Some("TableNormal"));
    assert_eq!(TableCtx::of(&s, &body_xml("<w:p/>"), paras(&body_xml("<w:p/>"))[0]), TableCtx::none());
}

#[test]
fn cond_set_from_look() {
    let look = TblLook::WORD_DEFAULT;
    let set = |r, c| TableCtx::cond_set(look, r, 4, c, 3, 1, 1);
    let names = |s: CondSet| s.iter().map(CondType::as_str).collect::<Vec<_>>();
    assert_eq!(names(set(0, 0)), ["wholeTable", "firstCol", "firstRow", "nwCell"]);
    assert_eq!(names(set(1, 1)), ["wholeTable", "band1Horz"]);
    assert_eq!(names(set(2, 1)), ["wholeTable", "band2Horz"]);
    assert_eq!(names(set(3, 2)), ["wholeTable", "band1Horz"]);
    let all_on = TblLook {
        first_row: true,
        last_row: true,
        first_column: true,
        last_column: true,
        no_h_band: false,
        no_v_band: false,
    };
    assert_eq!(names(TableCtx::cond_set(all_on, 3, 4, 2, 3, 1, 1)), ["wholeTable", "lastCol", "lastRow", "seCell"]);
    assert_eq!(names(TableCtx::cond_set(all_on, 1, 4, 1, 3, 1, 1)), ["wholeTable", "band1Vert", "band1Horz"]);
    assert_eq!(names(TableCtx::cond_set(all_on, 2, 6, 2, 6, 2, 2)), ["wholeTable", "band1Vert", "band1Horz"]);
    assert_eq!(names(TableCtx::cond_set(all_on, 3, 6, 3, 6, 2, 2)), ["wholeTable", "band2Vert", "band2Horz"]);
}

#[test]
fn tbl_look_hex_mask() {
    let d = body_xml(r#"<w:tbl><w:tblPr><w:tblLook w:val="04A0"/></w:tblPr></w:tbl>"#);
    let look = d.descendants_named(d.root(), W, "tblLook").next().unwrap();
    assert_eq!(TblLook::parse(&d, look), TblLook::WORD_DEFAULT);
}

// ---------------------------------------------------------------------------- lookup

#[test]
fn style_lookup_by_name() {
    let s = sheet(&format!(
        r#"{NORMAL}<w:style w:type="paragraph" w:styleId="Kop1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:aliases w:val="H2,Sub"/></w:style>
<w:style w:type="character" w:styleId="Kop1Char"><w:name w:val="Kop 1 Char"/><w:link w:val="Kop1"/></w:style>
<w:style w:type="paragraph" w:customStyle="1" w:styleId="Clausetitel"><w:name w:val="Clause titel"/></w:style>"#
    ));
    let id = |n: &str| s.style_by_name(n).map(|x| x.id());
    assert_eq!(id("heading 1"), Some("Kop1"));
    assert_eq!(id("Heading 1"), Some("Kop1"));
    assert_eq!(id("HEADING1"), Some("Kop1"));
    assert_eq!(id("Kop 1"), Some("Kop1"));
    assert_eq!(id("kop1"), Some("Kop1"));
    assert_eq!(id("Überschrift 1"), Some("Kop1"));
    assert_eq!(id("Standaard"), Some("Normal"));
    assert_eq!(id("sub"), Some("Heading2"));
    assert_eq!(id("Clause Titel"), Some("Clausetitel"));
    assert_eq!(id("Heading 3"), None);
    assert_eq!(id(""), None);
    assert_eq!(s.style_by_name_of_kind("Kop 1 Char", StyleKind::Paragraph).map(|x| x.id()), None);
    assert_eq!(s.style_by_name_of_kind("kop 1 char", StyleKind::Character).map(|x| x.id()), Some("Kop1Char"));
    let k = s.style_by_id("Kop1").unwrap();
    assert_eq!((k.kind(), k.based_on(), k.name()), (StyleKind::Paragraph, Some("Normal"), Some("heading 1")));
    assert_eq!(s.style_by_id("Kop1Char").and_then(|x| x.linked()), Some("Kop1"));
    assert_eq!(s.default_paragraph_style_id(), Some("Normal"));
    assert_eq!(s.style_by_id("Heading2").unwrap().aliases(), ["H2", "Sub"]);
}

// ---------------------------------------------------------------------------- caching

#[test]
fn cache_fills_and_invalidates() {
    let s = sheet(TOGGLE_STYLES);
    let d = body_xml(
        r#"<w:p><w:pPr><w:pStyle w:val="BoldPara"/></w:pPr><w:r><w:t>a</w:t></w:r><w:r><w:rPr><w:rStyle w:val="BoldChar"/></w:rPr><w:t>b</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="BoldPara"/></w:pPr><w:r><w:t>c</w:t></w:r></w:p>"#,
    );
    let first = resolve_all(&s, &d);
    assert_eq!(s.cache_len(), (1, 2));
    s.invalidate();
    assert_eq!(s.cache_len(), (0, 0));
    assert_eq!(resolve_all(&s, &d), first);
}

#[test]
fn paragraph_mark() {
    let s = sheet(TOGGLE_STYLES);
    let d = body_xml(r#"<w:p><w:pPr><w:pStyle w:val="BoldPara"/><w:rPr><w:sz w:val="40"/></w:rPr></w:pPr></w:p>"#);
    let p = paras(&d)[0];
    let ep = s.resolve_paragraph(&d, p, &TableCtx::none());
    let mark = s.resolve_paragraph_mark(&d, p, &ep);
    assert_eq!((mark.bold, mark.size), (Some(true), Some(40)));
}
