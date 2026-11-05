mod diff;
mod markdown;
mod syntax;

pub use diff::{DiffLine, DiffRow, LineKind, Segment, diff_lines, plain_lines};
pub use markdown::{Block, FileRef, Inline, parse_markdown, plain_text};
pub use syntax::{Language, Span, TokenClass, highlight};
