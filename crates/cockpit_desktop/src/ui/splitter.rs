//! The line between the chat and the file view, which a drag or the arrow keys move.
//!
//! While the pointer moves, the page's script sets the file view's share of the window in a CSS
//! custom property, and Rust hears of it once, when the button is released: a render of the
//! window per mouse move would make it lag.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection::Cockpit;
use crate::state::actions;

/// The window's root element, which holds `SHARE_PROPERTY`.
pub const APP_ID: &str = "app";

pub const FILE_VIEW_ID: &str = "file-view";

const SPLITTER_ID: &str = "splitter";

/// The CSS custom property that holds the file view's share of the window's width.
pub const SHARE_PROPERTY: &str = "--file-view-share";

/// How far an arrow key moves the splitter, as a share of the window's width.
const KEY_STEP: f64 = 0.02;

/// Waits for its settings, then moves the splitter and sends the file view's share as the layout
/// gave it, which the stylesheet's minimum widths may have changed. Field names are serde's names
/// for `Settings` and `Moved`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
const byId = (id) => document.getElementById(id);
const width = (element) => element.getBoundingClientRect().width;
const place = (share) => {
  const app = byId(settings.app_id);
  if (app !== null) {
    app.style.setProperty(settings.property, String(share));
  }
};
const commit = () => {
  const app = byId(settings.app_id);
  const pane = byId(settings.file_view_id);
  if (app === null || pane === null) {
    return;
  }
  const share = width(pane) / width(app);
  place(share);
  dioxus.send({ share });
};
const isSplitter = (target) => target instanceof Element && target.id === settings.splitter_id;
let drag = null;
let frame = 0;
const follow = () => {
  frame = 0;
  if (drag !== null) {
    place((drag.startWidth + drag.startX - drag.x) / drag.appWidth);
  }
};
document.addEventListener("pointerdown", (event) => {
  const app = byId(settings.app_id);
  const pane = byId(settings.file_view_id);
  if (!isSplitter(event.target) || event.button !== 0 || app === null || pane === null) {
    return;
  }
  event.target.setPointerCapture(event.pointerId);
  drag = {
    pointer: event.pointerId,
    splitter: event.target,
    startX: event.clientX,
    startWidth: width(pane),
    appWidth: width(app),
    x: event.clientX,
    moved: false,
  };
  drag.splitter.classList.add("dragging");
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
  const { splitter, moved } = drag;
  drag = null;
  splitter.classList.remove("dragging");
  document.documentElement.classList.remove("splitting");
  if (moved) {
    commit();
  }
};
document.addEventListener("pointerup", end);
document.addEventListener("pointercancel", end);
// The press that starts a drag would also start a text selection, which the pointer's moves then
// stretch over the chat.
document.addEventListener("selectstart", (event) => {
  if (drag !== null) {
    event.preventDefault();
  }
});
document.addEventListener("keydown", (event) => {
  const direction = { ArrowLeft: 1, ArrowRight: -1 }[event.key];
  const app = byId(settings.app_id);
  const pane = byId(settings.file_view_id);
  const plain = !event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey;
  if (!isSplitter(event.target) || direction === undefined || !plain || app === null || pane === null) {
    return;
  }
  event.preventDefault();
  place(width(pane) / width(app) + direction * settings.key_step);
  commit();
});
await new Promise(() => {});
"#;

#[derive(Debug, Serialize)]
struct Settings {
    splitter_id: &'static str,
    app_id: &'static str,
    file_view_id: &'static str,
    property: &'static str,
    key_step: f64,
}

/// The splitter moved: the file view's share of the window's width.
#[derive(Debug, Deserialize)]
struct Moved {
    share: f64,
}

/// Runs the splitter's script for the window's life. It finds the splitter by its id, so it keeps
/// working as the file view opens and closes.
pub fn use_splitter(cockpit: Cockpit) {
    use_hook(|| {
        let mut script = document::eval(SCRIPT);
        let settings = Settings {
            splitter_id: SPLITTER_ID,
            app_id: APP_ID,
            file_view_id: FILE_VIEW_ID,
            property: SHARE_PROPERTY,
            key_step: KEY_STEP,
        };
        if let Err(error) = script.send(settings) {
            tracing::warn!("cannot start the splitter's script: {error}");
        }
        spawn(async move {
            loop {
                match script.recv::<Moved>().await {
                    Ok(Moved { share }) => {
                        cockpit.update(|state| actions::set_file_view_share(state, share));
                    }
                    Err(error) => {
                        tracing::warn!("the splitter's script stopped: {error}");
                        return;
                    }
                }
            }
        });
    });
}

/// `share` is the file view's share of the window's width.
#[component]
pub fn Splitter(share: f64) -> Element {
    let cockpit: Cockpit = use_context();
    let percent = (share * 100.0).round();
    rsx! {
        div {
            id: SPLITTER_ID,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The script reads these fields by name: a rename in Rust would make it read `undefined`,
    /// with no error.
    #[test]
    fn the_fields_the_script_reads_and_writes_are_pinned() {
        let settings = Settings {
            splitter_id: SPLITTER_ID,
            app_id: APP_ID,
            file_view_id: FILE_VIEW_ID,
            property: SHARE_PROPERTY,
            key_step: KEY_STEP,
        };

        assert_eq!(
            serde_json::to_value(settings).unwrap(),
            json!({
                "splitter_id": "splitter",
                "app_id": "app",
                "file_view_id": "file-view",
                "property": "--file-view-share",
                "key_step": 0.02,
            })
        );
        let moved: Moved = serde_json::from_str(r#"{"share":0.4375}"#).unwrap();
        assert_eq!(moved.share, 0.4375);
    }
}
