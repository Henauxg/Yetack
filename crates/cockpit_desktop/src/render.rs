mod capped;
mod diff;
mod markdown;
mod syntax;

pub use capped::MAX_RENDERED_LINES;
pub use diff::{Diff, DiffLine, DiffRow, LineKind, diff_lines, plain_lines};
pub use markdown::{Block, FileRef, Inline, parse_markdown};
pub use syntax::{Language, highlight_capped};
