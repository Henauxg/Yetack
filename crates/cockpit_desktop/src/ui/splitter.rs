//! The lines that size the panes beside the chat: the sidebar's on its right edge, and the file
//! view's and the files pane's on their left edges. A drag or the arrow keys move them.
//!
//! While the pointer moves, the page's script sets the pane's size in a CSS custom property on the
//! window's root, and Rust hears of it once, when the button is released: a render of the window
//! per mouse move would make it lag.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection::Cockpit;
use crate::prefs::{MAX_FILES_WIDTH, MAX_SIDEBAR_WIDTH, MIN_FILES_WIDTH, MIN_SIDEBAR_WIDTH};
use crate::state::{actions, file_tree};

/// The window's root element, which holds each pane's property.
pub const APP_ID: &str = "app";

pub const FILE_VIEW_ID: &str = "file-view";

pub const SIDEBAR_ID: &str = "sidebar";

pub const FILES_PANE_ID: &str = "files-pane";

/// The CSS custom property that holds the file view's share of the window's width.
pub const SHARE_PROPERTY: &str = "--file-view-share";

/// The CSS custom property that holds the sidebar's width.
pub const SIDEBAR_WIDTH_PROPERTY: &str = "--sidebar-width";

/// The CSS custom property that holds the files pane's width.
pub const FILES_WIDTH_PROPERTY: &str = "--files-width";

/// How far an arrow key moves the file view's splitter, as a share of the window's width.
const SHARE_KEY_STEP: f64 = 0.02;

/// How far an arrow key moves the sidebar's and the files pane's splitters, in pixels.
const PIXELS_KEY_STEP: f64 = 16.0;

/// Waits for its settings, then moves the splitters and sends a pane's size as the layout gave it,
/// which the style sheet's bounds may have changed. Field names are serde's names for `Settings`,
/// `SplitterSettings` and `Moved`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
const byId = (id) => document.getElementById(id);
const width = (element) => element.getBoundingClientRect().width;
// The pane's size in the splitter's unit, from its width in pixels.
const size = (splitter, paneWidth) => {
  const app = byId(settings.app_id);
  return splitter.unit === "share" ? paneWidth / width(app) : paneWidth;
};
const place = (splitter, value) => {
  const app = byId(settings.app_id);
  if (app !== null) {
    app.style.setProperty(splitter.property, splitter.unit === "share" ? String(value) : `${value}px`);
  }
};
const commit = (splitter) => {
  const pane = byId(splitter.pane_id);
  if (byId(settings.app_id) === null || pane === null) {
    return;
  }
  const value = size(splitter, width(pane));
  place(splitter, value);
  dioxus.send({ pane: splitter.pane, value });
};
const splitterOf = (target) => target instanceof Element
  ? settings.splitters.find((splitter) => splitter.splitter_id === target.id)
  : undefined;
// +1 when a move of the pointer to the right widens the pane.
const growth = (splitter) => splitter.pane_side === "left" ? 1 : -1;
let drag = null;
let frame = 0;
const follow = () => {
  frame = 0;
  if (drag !== null) {
    const paneWidth = drag.startWidth + growth(drag.splitter) * (drag.x - drag.startX);
    place(drag.splitter, size(drag.splitter, paneWidth));
  }
};
document.addEventListener("pointerdown", (event) => {
  const splitter = splitterOf(event.target);
  const pane = splitter === undefined ? null : byId(splitter.pane_id);
  if (splitter === undefined || event.button !== 0 || byId(settings.app_id) === null || pane === null) {
    return;
  }
  // Without this, the drag selects the chat's text. The focus it would give is given here, so
  // that the arrow keys move the splitter after a click.
  event.preventDefault();
  event.target.focus({ preventScroll: true });
  event.target.setPointerCapture(event.pointerId);
  drag = {
    pointer: event.pointerId,
    element: event.target,
    splitter,
    startX: event.clientX,
    startWidth: width(pane),
    x: event.clientX,
    moved: false,
  };
  drag.element.classList.add("dragging");
  document.documentElement.classList.add("splitting");
});
document.addEventListener("pointermove", (event) => {
  if (drag === null || event.pointerId !== drag.pointer) {
    return;
  }
  drag.x = event.clientX;
  drag.moved = true;
  if (frame === 0) {
    frame = requestAnimationFrame(follow);
  }
});
const end = (event) => {
  if (drag === null || event.pointerId !== drag.pointer) {
    return;
  }
  cancelAnimationFrame(frame);
  follow();
  const { element, splitter, moved } = drag;
  drag = null;
  // Measured while `splitting` still turns the transitions off: with them on, the last move would
  // start one and the pane would measure its width before the move.
  if (moved) {
    commit(splitter);
  }
  element.classList.remove("dragging");
  document.documentElement.classList.remove("splitting");
};
document.addEventListener("pointerup", end);
document.addEventListener("pointercancel", end);
document.addEventListener("keydown", (event) => {
  const splitter = splitterOf(event.target);
  const direction = { ArrowLeft: -1, ArrowRight: 1 }[event.key];
  const pane = splitter === undefined ? null : byId(splitter.pane_id);
  const plain = !event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey;
  if (splitter === undefined || direction === undefined || !plain || byId(settings.app_id) === null || pane === null) {
    return;
  }
  event.preventDefault();
  // A step is immediate, as a drag is: the commit measures the pane, and a transition would give
  // its width before the step.
  const root = document.documentElement;
  root.classList.add("splitting");
  place(splitter, size(splitter, width(pane)) + growth(splitter) * direction * splitter.key_step);
  commit(splitter);
  requestAnimationFrame(() => {
    if (drag === null) {
      root.classList.remove("splitting");
    }
  });
});
await new Promise(() => {});
"#;

/// A pane a splitter sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pane {
    Sidebar,
    FileView,
    Files,
}

/// What a pane's size counts.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Unit {
    /// A share of the window's width, between 0 and 1.
    Share,
    Pixels,
}

/// Which side of its splitter a pane is on.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Side {
    Left,
    Right,
}

#[derive(Debug, Serialize)]
struct SplitterSettings {
    pane: Pane,
    splitter_id: &'static str,
    pane_id: &'static str,
    property: &'static str,
    unit: Unit,
    pane_side: Side,
    /// In the pane's unit.
    key_step: f64,
}

#[derive(Debug, Serialize)]
struct Settings {
    app_id: &'static str,
    splitters: [SplitterSettings; 3],
}

/// A splitter moved: its pane's size in the pane's unit.
#[derive(Debug, Deserialize)]
struct Moved {
    pane: Pane,
    value: f64,
}

impl Pane {
    fn splitter_id(self) -> &'static str {
        match self {
            Self::Sidebar => "sidebar-splitter",
            Self::FileView => "splitter",
            Self::Files => "files-splitter",
        }
    }

    fn settings(self) -> SplitterSettings {
        let (pane_id, property, unit, pane_side, key_step) = match self {
            Self::Sidebar => (
                SIDEBAR_ID,
                SIDEBAR_WIDTH_PROPERTY,
                Unit::Pixels,
                Side::Left,
                PIXELS_KEY_STEP,
            ),
            Self::FileView => (
                FILE_VIEW_ID,
                SHARE_PROPERTY,
                Unit::Share,
                Side::Right,
                SHARE_KEY_STEP,
            ),
            Self::Files => (
                FILES_PANE_ID,
                FILES_WIDTH_PROPERTY,
                Unit::Pixels,
                Side::Right,
                PIXELS_KEY_STEP,
            ),
        };
        SplitterSettings {
            pane: self,
            splitter_id: self.splitter_id(),
            pane_id,
            property,
            unit,
            pane_side,
            key_step,
        }
    }
}

fn settings() -> Settings {
    Settings {
        app_id: APP_ID,
        splitters: [
            Pane::Sidebar.settings(),
            Pane::FileView.settings(),
            Pane::Files.settings(),
        ],
    }
}

/// Runs the splitters' script for the window's life. It finds a splitter by its id, so it keeps
/// working as the file view opens and closes.
pub fn use_splitter(cockpit: Cockpit) {
    use_hook(|| {
        let mut script = document::eval(SCRIPT);
        if let Err(error) = script.send(settings()) {
            tracing::warn!("cannot start the splitters' script: {error}");
        }
        spawn(async move {
            loop {
                match script.recv::<Moved>().await {
                    Ok(Moved { pane, value }) => cockpit.update(|state| match pane {
                        Pane::Sidebar => actions::set_sidebar_width(state, value),
                        Pane::FileView => actions::set_file_view_share(state, value),
                        Pane::Files => file_tree::set_width(state, value),
                    }),
                    Err(error) => {
                        tracing::warn!("the splitters' script stopped: {error}");
                        return;
                    }
                }
            }
        });
    });
}

/// The file view's splitter. `share` is the file view's share of the window's width.
#[component]
pub fn Splitter(share: f64) -> Element {
    let cockpit: Cockpit = use_context();
    let percent = (share * 100.0).round();
    rsx! {
        div {
            id: Pane::FileView.splitter_id(),
            class: "splitter",
            role: "separator",
            tabindex: 0,
            aria_orientation: "vertical",
            aria_label: "File view width",
            aria_valuemin: 0,
            aria_valuemax: 100,
            aria_valuenow: percent,
            title: "Drag to resize the file view, double-click for its default width",
            ondoubleclick: move |_| cockpit.update(actions::reset_file_view_share),
        }
    }
}

/// The files pane's splitter, on its left edge. `width` is the pane's width in pixels.
#[component]
pub fn FilesSplitter(width: u16) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        div {
            id: Pane::Files.splitter_id(),
            class: "splitter",
            role: "separator",
            tabindex: 0,
            aria_orientation: "vertical",
            aria_label: "Files pane width",
            aria_valuemin: MIN_FILES_WIDTH,
            aria_valuemax: MAX_FILES_WIDTH,
            aria_valuenow: width,
            title: "Drag to resize the files pane, double-click for its default width",
            ondoubleclick: move |_| cockpit.update(file_tree::reset_width),
        }
    }
}

/// The sidebar's splitter, on its right edge. `width` is the sidebar's width in pixels.
#[component]
pub fn SidebarSplitter(width: u16) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        div {
            id: Pane::Sidebar.splitter_id(),
            class: "splitter sidebar-splitter",
            role: "separator",
            tabindex: 0,
            aria_orientation: "vertical",
            aria_label: "Sidebar width",
            aria_valuemin: MIN_SIDEBAR_WIDTH,
            aria_valuemax: MAX_SIDEBAR_WIDTH,
            aria_valuenow: width,
            title: "Drag to resize the sidebar, double-click for its default width",
            ondoubleclick: move |_| cockpit.update(actions::reset_sidebar_width),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The script reads these fields by name: a rename in Rust would make it read `undefined`,
    /// with no error.
    #[test]
    fn the_fields_the_script_reads_and_writes_are_pinned() {
        assert_eq!(
            serde_json::to_value(settings()).unwrap(),
            json!({
                "app_id": "app",
                "splitters": [
                    {
                        "pane": "sidebar",
                        "splitter_id": "sidebar-splitter",
                        "pane_id": "sidebar",
                        "property": "--sidebar-width",
                        "unit": "pixels",
                        "pane_side": "left",
                        "key_step": 16.0,
                    },
                    {
                        "pane": "file_view",
                        "splitter_id": "splitter",
                        "pane_id": "file-view",
                        "property": "--file-view-share",
                        "unit": "share",
                        "pane_side": "right",
                        "key_step": 0.02,
                    },
                    {
                        "pane": "files",
                        "splitter_id": "files-splitter",
                        "pane_id": "files-pane",
                        "property": "--files-width",
                        "unit": "pixels",
                        "pane_side": "right",
                        "key_step": 16.0,
                    },
                ],
            })
        );
        let moved: Moved = serde_json::from_str(r#"{"pane":"file_view","value":0.4375}"#).unwrap();
        assert_eq!(moved.pane, Pane::FileView);
        assert_eq!(moved.value, 0.4375);
    }
}
