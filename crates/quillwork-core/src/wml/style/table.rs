//! Table-style conditional formatting: which `w:tblStylePr` types apply to a cell.

use super::StyleSheet;
use super::props::{parse_num, parse_on_off, w_children, wattr, wval};
use crate::xml::{Document, NodeId, ns::W};

/// `ST_TblStyleOverrideType`, in the order Word applies them (later wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CondType {
    WholeTable,
    Band1Vert,
    Band2Vert,
    Band1Horz,
    Band2Horz,
    FirstCol,
    LastCol,
    FirstRow,
    LastRow,
    NeCell,
    NwCell,
    SeCell,
    SwCell,
}

impl CondType {
    /// All types in application order.
    pub const ALL: [CondType; 13] = [
        CondType::WholeTable,
        CondType::Band1Vert,
        CondType::Band2Vert,
        CondType::Band1Horz,
        CondType::Band2Horz,
        CondType::FirstCol,
        CondType::LastCol,
        CondType::FirstRow,
        CondType::LastRow,
        CondType::NeCell,
        CondType::NwCell,
        CondType::SeCell,
        CondType::SwCell,
    ];

    pub fn parse(s: &str) -> Option<CondType> {
        Some(match s {
            "wholeTable" => CondType::WholeTable,
            "band1Vert" => CondType::Band1Vert,
            "band2Vert" => CondType::Band2Vert,
            "band1Horz" => CondType::Band1Horz,
            "band2Horz" => CondType::Band2Horz,
            "firstCol" => CondType::FirstCol,
            "lastCol" => CondType::LastCol,
            "firstRow" => CondType::FirstRow,
            "lastRow" => CondType::LastRow,
            "neCell" => CondType::NeCell,
            "nwCell" => CondType::NwCell,
            "seCell" => CondType::SeCell,
            "swCell" => CondType::SwCell,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CondType::WholeTable => "wholeTable",
            CondType::Band1Vert => "band1Vert",
            CondType::Band2Vert => "band2Vert",
            CondType::Band1Horz => "band1Horz",
            CondType::Band2Horz => "band2Horz",
            CondType::FirstCol => "firstCol",
            CondType::LastCol => "lastCol",
            CondType::FirstRow => "firstRow",
            CondType::LastRow => "lastRow",
            CondType::NeCell => "neCell",
            CondType::NwCell => "nwCell",
            CondType::SeCell => "seCell",
            CondType::SwCell => "swCell",
        }
    }

    fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// A set of conditional-format types (always includes `wholeTable` when built by
/// [`TableCtx::cond_set`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CondSet(u16);

impl CondSet {
    pub const EMPTY: CondSet = CondSet(0);

    pub fn contains(self, c: CondType) -> bool {
        self.0 & c.bit() != 0
    }
    pub fn insert(&mut self, c: CondType) {
        self.0 |= c.bit();
    }
    pub fn with(mut self, c: CondType) -> CondSet {
        self.insert(c);
        self
    }
    /// Members in application order.
    pub fn iter(self) -> impl Iterator<Item = CondType> {
        CondType::ALL.into_iter().filter(move |c| self.contains(*c))
    }
    pub fn bits(self) -> u16 {
        self.0
    }
}

/// `w:tblLook`: which conditional formats a table turns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TblLook {
    pub first_row: bool,
    pub last_row: bool,
    pub first_column: bool,
    pub last_column: bool,
    pub no_h_band: bool,
    pub no_v_band: bool,
}

impl TblLook {
    /// Word's default for new tables (`w:val="04A0"`): first row, first column, row banding.
    pub const WORD_DEFAULT: TblLook = TblLook {
        first_row: true,
        last_row: false,
        first_column: true,
        last_column: false,
        no_h_band: false,
        no_v_band: true,
    };

    /// Read a `w:tblLook` element: the 2010 attributes, else the 2006 `w:val` bitmask.
    pub fn parse(doc: &Document, look: NodeId) -> TblLook {
        let mask = wval(doc, look).and_then(|v| u16::from_str_radix(v.trim(), 16).ok()).unwrap_or(0);
        let flag = |name: &str, bit: u16| match wattr(doc, look, name) {
            Some(v) => parse_on_off(&v),
            None => mask & bit != 0,
        };
        TblLook {
            first_row: flag("firstRow", 0x0020),
            last_row: flag("lastRow", 0x0040),
            first_column: flag("firstColumn", 0x0080),
            last_column: flag("lastColumn", 0x0100),
            no_h_band: flag("noHBand", 0x0200),
            no_v_band: flag("noVBand", 0x0400),
        }
    }
}

/// Where a paragraph or cell sits in a table: the table style in effect and the conditional
/// formats that apply. [`TableCtx::default()`] is "not in a table".
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TableCtx {
    /// Table style id (`w:tblStyle`, else the default table style).
    pub style: Option<String>,
    pub cond: CondSet,
}

impl TableCtx {
    /// Not in a table.
    pub fn none() -> TableCtx {
        TableCtx::default()
    }

    /// Conditional formats for the cell at (`row`, `col`) of a `rows` × `cols` table.
    /// `row_band` / `col_band` are the banding sizes (`w:tblStyleRowBandSize`, default 1).
    pub fn cond_set(
        look: TblLook,
        row: usize,
        rows: usize,
        col: usize,
        cols: usize,
        row_band: u32,
        col_band: u32,
    ) -> CondSet {
        let mut s = CondSet::EMPTY.with(CondType::WholeTable);
        let first_row = look.first_row && row == 0;
        let last_row = look.last_row && rows > 0 && row + 1 == rows;
        let first_col = look.first_column && col == 0;
        let last_col = look.last_column && cols > 0 && col + 1 == cols;
        if !look.no_v_band && !first_col && !last_col {
            let c = col - usize::from(look.first_column);
            let band = (c / col_band.max(1) as usize) % 2;
            s.insert(if band == 0 { CondType::Band1Vert } else { CondType::Band2Vert });
        }
        if !look.no_h_band && !first_row && !last_row {
            let r = row - usize::from(look.first_row);
            let band = (r / row_band.max(1) as usize) % 2;
            s.insert(if band == 0 { CondType::Band1Horz } else { CondType::Band2Horz });
        }
        for (on, c) in [
            (first_col, CondType::FirstCol),
            (last_col, CondType::LastCol),
            (first_row, CondType::FirstRow),
            (last_row, CondType::LastRow),
            (first_row && last_col, CondType::NeCell),
            (first_row && first_col, CondType::NwCell),
            (last_row && last_col, CondType::SeCell),
            (last_row && first_col, CondType::SwCell),
        ] {
            if on {
                s.insert(c);
            }
        }
        s
    }

    /// The context of `node` (a paragraph, run, cell or anything inside a cell): found from
    /// its nearest enclosing `w:tc`. Outside a table this is [`TableCtx::none`].
    pub fn of(sheet: &StyleSheet, doc: &Document, node: NodeId) -> TableCtx {
        let tc = if doc.is(node, W, "tc") { Some(node) } else { doc.ancestors(node).find(|&a| doc.is(a, W, "tc")) };
        let Some(tc) = tc else { return TableCtx::none() };
        let Some(tr) = doc.ancestors(tc).find(|&a| doc.is(a, W, "tr")) else { return TableCtx::none() };
        let Some(tbl) = doc.ancestors(tr).find(|&a| doc.is(a, W, "tbl")) else { return TableCtx::none() };

        let tbl_pr = doc.find_child(tbl, W, "tblPr");
        let style = tbl_pr
            .and_then(|p| doc.find_child(p, W, "tblStyle"))
            .and_then(|s| wval(doc, s))
            .map(|s| s.into_owned())
            .filter(|id| sheet.style_by_id(id).is_some())
            .or_else(|| sheet.default_table_style_id().map(str::to_owned));
        let look =
            tbl_pr.and_then(|p| doc.find_child(p, W, "tblLook")).map(|l| TblLook::parse(doc, l)).unwrap_or_default();
        let band = |name: &str| {
            tbl_pr
                .and_then(|p| doc.find_child(p, W, name))
                .and_then(|n| wval(doc, n))
                .and_then(|v| parse_num(&v))
                .map(|v| v.clamp(1, 1 << 16) as u32)
        };
        let (style_row_band, style_col_band) = style.as_deref().map(|id| sheet.band_sizes(id)).unwrap_or((None, None));
        let row_band = band("tblStyleRowBandSize").or(style_row_band).unwrap_or(1);
        let col_band = band("tblStyleColBandSize").or(style_col_band).unwrap_or(1);

        let rows = structural_children(doc, tbl, "tr");
        let row = rows.iter().position(|&r| r == tr).unwrap_or(0);
        let cells = structural_children(doc, tr, "tc");
        let col = cells.iter().position(|&c| c == tc).unwrap_or(0);
        TableCtx { style, cond: TableCtx::cond_set(look, row, rows.len(), col, cells.len(), row_band, col_band) }
    }
}

/// `w:tr` children of a table (or `w:tc` children of a row), looking through content
/// wrappers (`w:sdt`/`w:sdtContent`, `w:customXml`) but not into nested tables.
fn structural_children(doc: &Document, parent: NodeId, local: &str) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = doc.child_elements(parent).collect();
    stack.reverse();
    while let Some(n) = stack.pop() {
        if doc.namespace(n) != Some(W) {
            continue;
        }
        match doc.local_name(n) {
            Some(l) if l == local => out.push(n),
            Some("sdt" | "sdtContent" | "customXml") => {
                let mut kids: Vec<NodeId> = w_children(doc, n).map(|(c, _)| c).collect();
                kids.reverse();
                stack.extend(kids);
            }
            _ => {}
        }
    }
    out
}
