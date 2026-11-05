//! The mockups' icons, as SVG markup: 24 by 24 strokes drawn in the text color.

use dioxus::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Search,
    Plus,
    ChevronDown,
    ChevronRight,
    ChevronLeft,
    Close,
    Copy,
    Check,
    FileChanges,
    Tasks,
    Running,
    /// The still counterpart of `Running`, for work that ended.
    Circle,
    Plug,
    Person,
    Terminal,
    Stop,
    StopSmall,
    Send,
    Clock,
    Shield,
    Bolt,
    List,
    Pencil,
    Slash,
    Reconnect,
    Power,
    Eye,
    EyeOff,
    Question,
    Warning,
    Folder,
}

impl IconKind {
    /// The shapes inside the 24 by 24 view box.
    fn shapes(self) -> &'static str {
        match self {
            Self::Search => {
                r#"<circle cx="11" cy="11" r="7"></circle><path d="M20 20l-3.5-3.5"></path>"#
            }
            Self::Plus => r#"<path d="M12 5v14"></path><path d="M5 12h14"></path>"#,
            Self::ChevronDown => r#"<path d="M6 9l6 6 6-6"></path>"#,
            Self::ChevronRight => r#"<path d="M9 6l6 6-6 6"></path>"#,
            Self::ChevronLeft => r#"<path d="M15 6l-6 6 6 6"></path>"#,
            Self::Close => r#"<path d="M6 6l12 12"></path><path d="M18 6L6 18"></path>"#,
            Self::Copy => {
                r#"<rect x="9" y="9" width="11" height="11" rx="2"></rect><path d="M5 15V6a2 2 0 0 1 2-2h9"></path>"#
            }
            Self::Check => r#"<path d="M5 12l5 5 9-10"></path>"#,
            Self::FileChanges => {
                r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"></path><path d="M14 3v5h5"></path><path d="M12 11v6"></path><path d="M9 14h6"></path>"#
            }
            Self::Tasks => {
                r#"<path d="M4 7l2 2 4-4"></path><path d="M13 7h7"></path><path d="M4 16l2 2 4-4"></path><path d="M13 16h7"></path>"#
            }
            Self::Running => r#"<path d="M21 12a9 9 0 1 1-6.2-8.6"></path>"#,
            Self::Circle => r#"<circle cx="12" cy="12" r="9"></circle>"#,
            Self::Plug => {
                r#"<path d="M9 3v5"></path><path d="M15 3v5"></path><path d="M6 8h12v3a6 6 0 0 1-12 0z"></path><path d="M12 17v4"></path>"#
            }
            Self::Person => {
                r#"<circle cx="12" cy="8" r="4"></circle><path d="M5 21a7 7 0 0 1 14 0"></path>"#
            }
            Self::Terminal => r#"<path d="M5 7l5 5-5 5"></path><path d="M12 18h7"></path>"#,
            Self::Stop => {
                r#"<rect x="5" y="5" width="14" height="14" rx="2" fill="currentColor" stroke="none"></rect>"#
            }
            Self::StopSmall => {
                r#"<rect x="4" y="4" width="16" height="16" rx="3" fill="currentColor" stroke="none"></rect>"#
            }
            Self::Send => r#"<path d="M12 19V5"></path><path d="M6 11l6-6 6 6"></path>"#,
            Self::Clock => {
                r#"<circle cx="12" cy="12" r="8"></circle><path d="M12 8v4l3 2"></path>"#
            }
            Self::Shield => r#"<path d="M12 3l7 3v6c0 4-3 7-7 9-4-2-7-5-7-9V6z"></path>"#,
            Self::Bolt => r#"<path d="M13 3L5 14h6l-1 7 8-11h-6z"></path>"#,
            Self::List => {
                r#"<path d="M9 6h11"></path><path d="M9 12h11"></path><path d="M9 18h11"></path><path d="M4 6h.01"></path><path d="M4 12h.01"></path><path d="M4 18h.01"></path>"#
            }
            Self::Pencil => {
                r#"<path d="M4 20h4L19 9l-4-4L4 16z"></path><path d="M13 7l4 4"></path>"#
            }
            Self::Slash => {
                r#"<rect x="4" y="4" width="16" height="16" rx="3"></rect><path d="M14 8l-4 8"></path>"#
            }
            Self::Reconnect => {
                r#"<path d="M20 11a8 8 0 1 0-2.3 5.7"></path><path d="M20 4v7h-7"></path>"#
            }
            Self::Power => {
                r#"<path d="M12 3v8"></path><path d="M6.3 7.3a8 8 0 1 0 11.4 0"></path>"#
            }
            Self::Eye => {
                r#"<path d="M3 12s4-6 9-6 9 6 9 6-4 6-9 6-9-6-9-6z"></path><circle cx="12" cy="12" r="3"></circle>"#
            }
            Self::EyeOff => {
                r#"<path d="M3 3l18 18"></path><path d="M10.6 6.1A9.8 9.8 0 0 1 12 6c5 0 9 6 9 6a17 17 0 0 1-3 3.6"></path><path d="M6.6 7.6C4.4 9.1 3 12 3 12s4 6 9 6a8.6 8.6 0 0 0 4.4-1.2"></path><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"></path>"#
            }
            Self::Question => {
                r#"<circle cx="12" cy="12" r="9"></circle><path d="M9.5 9a2.5 2.5 0 1 1 3.5 2.3c-.6.3-1 .9-1 1.6V14"></path><path d="M12 17h.01"></path>"#
            }
            Self::Warning => {
                r#"<path d="M12 3l9 16H3z"></path><path d="M12 10v4"></path><path d="M12 17h.01"></path>"#
            }
            Self::Folder => {
                r#"<path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"></path>"#
            }
        }
    }
}

/// An icon `size` pixels wide, in the current text color.
#[component]
pub fn Icon(kind: IconKind, size: Option<u16>) -> Element {
    let size = size.unwrap_or(14);
    let markup = format!(
        r#"<svg width="{size}" height="{size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{}</svg>"#,
        kind.shapes()
    );
    rsx! {
        span { class: "icon", dangerous_inner_html: markup }
    }
}
