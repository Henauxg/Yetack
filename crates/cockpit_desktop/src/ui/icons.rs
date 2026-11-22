//! The mockups' icons, as SVG markup: 24 by 24 strokes drawn in the text color.

use dioxus::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Search,
    /// Dictation.
    Mic,
    /// The speech model to download.
    Download,
    Plus,
    ChevronDown,
    /// The find's previous match.
    ChevronUp,
    ChevronRight,
    ChevronLeft,
    Close,
    /// The terminal's "Move to" button: a window with a panel at its bottom.
    PanelBottom,
    Copy,
    Check,
    /// A target, to show the file view's file in the tree.
    Target,
    /// Two arrows that meet, to close every folder.
    Collapse,
    Tasks,
    Running,
    /// The still counterpart of `Running`, for work that ended.
    Circle,
    Plug,
    Person,
    Terminal,
    Send,
    ArrowDown,
    /// From one version to the next.
    ArrowRight,
    Clock,
    Shield,
    Bolt,
    List,
    Pencil,
    /// A smiling face, for the title's emoji.
    Smile,
    Slash,
    Reconnect,
    Power,
    Eye,
    EyeOff,
    Question,
    /// A note that explains, before its text.
    Info,
    Warning,
    Folder,
    File,
    /// The agent map: an agent above the subagents it started.
    Map,
    /// A server, in the sidebar's machine headers.
    Server,
    /// This computer, in the sidebar's machine headers.
    Laptop,
    /// More actions, in a menu.
    Dots,
    /// An agent marked to handle.
    Flag,
    /// Long lines wrapped, for the file view's wrap toggle.
    Wrap,
    /// Notifications, of the app or of an agent.
    Bell,
    /// Notifications turned off.
    BellOff,
    /// The app's settings.
    Gear,
    /// Reply to a passage of the chat.
    Reply,
    /// The keyboard shortcuts.
    Keyboard,
    /// Back to the default.
    Undo,
    /// A setting that does not change.
    Lock,
    /// A speech bubble, for the messages the user sent.
    Message,
    /// A git repository: a branch leaving its trunk.
    Repository,
    /// A link that opens in the browser.
    External,
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
            Self::ChevronUp => r#"<path d="M6 15l6-6 6 6"></path>"#,
            Self::ChevronRight => r#"<path d="M9 6l6 6-6 6"></path>"#,
            Self::ChevronLeft => r#"<path d="M15 6l-6 6 6 6"></path>"#,
            Self::Close => r#"<path d="M6 6l12 12"></path><path d="M18 6L6 18"></path>"#,
            Self::PanelBottom => {
                r#"<rect x="3" y="4" width="18" height="16" rx="2"></rect><path d="M3 14h18"></path>"#
            }
            Self::Copy => {
                r#"<rect x="9" y="9" width="11" height="11" rx="2"></rect><path d="M5 15V6a2 2 0 0 1 2-2h9"></path>"#
            }
            Self::Check => r#"<path d="M5 12l5 5 9-10"></path>"#,
            Self::Target => {
                r#"<circle cx="12" cy="12" r="7"></circle><circle cx="12" cy="12" r="2"></circle>"#
            }
            Self::Collapse => r#"<path d="M7 4l5 5 5-5"></path><path d="M7 20l5-5 5 5"></path>"#,
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
            Self::Send => r#"<path d="M12 19V5"></path><path d="M6 11l6-6 6 6"></path>"#,
            Self::Mic => {
                r#"<path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"></path><path d="M19 10v2a7 7 0 0 1-14 0v-2"></path><path d="M12 19v3"></path>"#
            }
            Self::Download => {
                r#"<path d="M12 15V3"></path><path d="m7 10 5 5 5-5"></path><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>"#
            }
            Self::ArrowDown => r#"<path d="M12 5v14"></path><path d="M6 13l6 6 6-6"></path>"#,
            Self::ArrowRight => r#"<path d="M5 12h14"></path><path d="M13 6l6 6-6 6"></path>"#,
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
            Self::Smile => {
                r#"<circle cx="12" cy="12" r="9"></circle><path d="M8.5 14a4.5 4.5 0 0 0 7 0"></path><path d="M9 9.5h.01"></path><path d="M15 9.5h.01"></path>"#
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
            Self::Message => {
                r#"<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12z"></path>"#
            }
            Self::External => {
                r#"<path d="M14 4h6v6"></path><path d="M20 4l-9 9"></path><path d="M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"></path>"#
            }
            Self::Repository => {
                r#"<circle cx="6" cy="5" r="2"></circle><circle cx="6" cy="19" r="2"></circle><circle cx="18" cy="7" r="2"></circle><path d="M6 7v10"></path><path d="M18 9c0 5-6 4-11.5 8.5"></path>"#
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
            Self::Info => {
                r#"<circle cx="12" cy="12" r="9"></circle><path d="M12 11v5"></path><path d="M12 8h.01"></path>"#
            }
            Self::Warning => {
                r#"<path d="M12 3l9 16H3z"></path><path d="M12 10v4"></path><path d="M12 17h.01"></path>"#
            }
            Self::File => {
                r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"></path><path d="M14 3v5h5"></path>"#
            }
            Self::Map => {
                r#"<rect x="9" y="3" width="6" height="5" rx="1"></rect><rect x="3" y="16" width="6" height="5" rx="1"></rect><rect x="15" y="16" width="6" height="5" rx="1"></rect><path d="M12 8v4"></path><path d="M6 16v-2a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v2"></path>"#
            }
            Self::Server => {
                r#"<rect x="3" y="4" width="18" height="7" rx="2"></rect><rect x="3" y="13" width="18" height="7" rx="2"></rect><path d="M7 7.5h.01M7 16.5h.01"></path>"#
            }
            Self::Dots => {
                r#"<circle cx="5" cy="12" r="1.2"></circle><circle cx="12" cy="12" r="1.2"></circle><circle cx="19" cy="12" r="1.2"></circle>"#
            }
            Self::Laptop => {
                r#"<rect x="4" y="5" width="16" height="11" rx="2"></rect><path d="M2 19h20"></path>"#
            }
            Self::Flag => {
                r#"<path d="M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z"></path><path d="M4 22v-7"></path>"#
            }
            Self::Folder => {
                r#"<path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"></path>"#
            }
            Self::Wrap => {
                r#"<path d="M3 6h18"></path><path d="M3 12h15a3 3 0 0 1 0 6h-4"></path><path d="M16 16l-2 2 2 2"></path><path d="M3 18h7"></path>"#
            }
            Self::Bell => {
                r#"<path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9"></path><path d="M13.73 21a2 2 0 0 1-3.46 0"></path>"#
            }
            Self::BellOff => {
                r#"<path d="M13.73 21a2 2 0 0 1-3.46 0"></path><path d="M18.63 13A17.89 17.89 0 0 1 18 8"></path><path d="M6.26 6.26A5.86 5.86 0 0 0 6 8c0 7-3 9-3 9h14"></path><path d="M18 8a6 6 0 0 0-9.33-5"></path><path d="M3 3l18 18"></path>"#
            }
            Self::Gear => {
                r#"<path d="M9.8 4.7 L10.4 2.1 L13.6 2.1 L14.2 4.7 L15.6 5.3 L17.9 3.9 L20.1 6.1 L18.7 8.4 L19.3 9.8 L21.9 10.4 L21.9 13.6 L19.3 14.2 L18.7 15.6 L20.1 17.9 L17.9 20.1 L15.6 18.7 L14.2 19.3 L13.6 21.9 L10.4 21.9 L9.8 19.3 L8.4 18.7 L6.1 20.1 L3.9 17.9 L5.3 15.6 L4.7 14.2 L2.1 13.6 L2.1 10.4 L4.7 9.8 L5.3 8.4 L3.9 6.1 L6.1 3.9 L8.4 5.3Z"></path><circle cx="12" cy="12" r="3"></circle>"#
            }
            Self::Reply => {
                r#"<path d="M10 7l-5 5 5 5"></path><path d="M5 12h10a4 4 0 0 1 4 4v2"></path>"#
            }
            Self::Keyboard => {
                r#"<rect x="2" y="6" width="20" height="12" rx="2"></rect><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"></path>"#
            }
            Self::Undo => {
                r#"<path d="M3 12a9 9 0 1 0 3-6.7L3 8"></path><path d="M3 3v5h5"></path>"#
            }
            Self::Lock => {
                r#"<rect x="5" y="11" width="14" height="10" rx="2"></rect><path d="M8 11V7a4 4 0 0 1 8 0v4"></path>"#
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
