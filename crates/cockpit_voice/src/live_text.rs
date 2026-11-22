/// The text of a recording so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveText {
    /// The segments before the last pause: this text no longer changes.
    pub said: String,
    /// The words since the last pause, which may still change.
    pub hearing: String,
}
