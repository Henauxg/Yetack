mod diff;
mod markdown;
mod syntax;

pub use diff::{DiffLine, DiffRow, LineKind, diff_lines, plain_lines};
pub use markdown::{Block, FileRef, Inline, parse_markdown};
pub use syntax::{Language, highlight};
