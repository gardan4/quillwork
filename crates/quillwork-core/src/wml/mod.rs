//! The Word model (DESIGN §4): a `.docx` as a set of parsed parts, its stories, typed views
//! over the XML, list numbering and fields.

pub(crate) mod tree;

pub mod doc;
pub mod field;
pub mod numbering;
pub mod parts;
pub mod story;
pub mod style;
pub mod text;
pub mod theme;
pub mod view;

pub use doc::{Loc, PartIdx, PartKind, WordDoc};
pub use field::{Field, FieldCode, FieldKind, fields};
pub use numbering::{EffectiveNum, ListLabel, Numbering};
pub use parts::{MemParts, Parts};
pub use story::{HfRef, HfType, Section, Story, StoryKind};
pub use text::{Segment, TextMode};
pub use view::{
    Block, Bookmark, Cell, CommentMarker, CommentMarkerKind, Drawing, DrawingKind, Hyperlink, NumPr, Paragraph,
    Revision, RevisionKind, Row, Run, Sdt, Table, VMerge,
};
