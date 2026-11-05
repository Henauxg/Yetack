//! Elements of the XML-like text Claude Code writes in some messages, such as
//! `<summary>…</summary>`.

/// The content of the first `tag` element in `text`, and the text after it.
pub(crate) fn element<'text>(text: &'text str, tag: &str) -> Option<(&'text str, &'text str)> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let (_, from_content) = text.split_once(&open)?;
    from_content.split_once(&close)
}

/// Whether `text` starts with a `tag` element.
pub(crate) fn starts_with(text: &str, tag: &str) -> bool {
    text.starts_with(&format!("<{tag}>"))
}
