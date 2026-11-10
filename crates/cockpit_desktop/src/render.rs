mod capped;
mod diff;
mod markdown;
mod syntax;

pub use capped::{MAX_RENDERED_LINES, split_lines};
pub use diff::{Diff, DiffLine, DiffRow, LineKind, changed_lines, diff_lines, plain_lines};
pub use markdown::{
    Block, FileRef, Inline, blocks_plain_text, parse_markdown, parse_markdown_file, plain_text,
};
pub use syntax::{Language, highlight_capped};
