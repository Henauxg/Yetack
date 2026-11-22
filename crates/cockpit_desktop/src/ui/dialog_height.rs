//! A dialog's height follows its content with an animation: a list that gets shorter, a tab
//! switch or a note that shows. CSS cannot animate a height that follows the content, so a page
//! script watches each dialog's parts and animates the dialog from its last height to its new one.
//! [`follow_content`] does the same for one element that is no dialog.

use dioxus::prelude::*;
use serde::Serialize;

use crate::ui::dialogs::OVERLAY_CLASS;
use crate::ui::presence::LEAVING;

/// Waits for its settings, then watches every dialog. Field names are serde's names for
/// `Settings`.
const SCRIPT: &str = r#"
const settings = await dioxus.recv();
if (!window.agentsCockpitDialogHeight) {
  window.agentsCockpitDialogHeight = true;
  const token = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  const millis = (name) => {
    const value = token(name);
    const number = parseFloat(value);
    if (Number.isNaN(number)) {
      return 0;
    }
    return value.endsWith("ms") ? number : number * 1000;
  };
  const watches = new Map();
  const scrolls = (element) => ["auto", "scroll"].includes(getComputedStyle(element).overflowY);
  // A dialog's parts whose height is their content's: its children, and the children of a child
  // that scrolls, since the dialog's own height shrinks that one.
  const parts = (dialog) => Array.from(dialog.children).flatMap((child) => {
    return scrolls(child) ? Array.from(child.children) : [child];
  });
  const stop = (watch) => {
    watch.running?.cancel();
    watch.running = null;
    for (const child of watch.squeezed) {
      child.style.scrollbarWidth = "";
    }
    watch.squeezed = [];
  };
  const play = (watch, from, to, elapsed = 0) => {
    const grows = to > from;
    const duration = millis(grows ? settings.grow_duration : settings.shrink_duration);
    if (duration <= 0) {
      return;
    }
    // While the dialog grows, its height squeezes a child that scrolls. A child whose content
    // fits at the new height hides its scrollbar meanwhile, so its content keeps its width.
    for (const child of watch.dialog.children) {
      if (scrolls(child) && child.scrollHeight <= child.clientHeight) {
        child.style.scrollbarWidth = "none";
        watch.squeezed.push(child);
      }
    }
    const animation = watch.dialog.animate(
      [{ height: `${from}px` }, { height: `${to}px` }],
      { duration, easing: token(grows ? settings.grow_easing : settings.shrink_easing) },
    );
    animation.currentTime = elapsed;
    animation.onfinish = () => {
      if (watch.running === animation) {
        stop(watch);
      }
    };
    watch.running = animation;
    watch.from = from;
  };
  const changed = (watch) => {
    const dialog = watch.dialog;
    let from = watch.height;
    const running = watch.running;
    // Read before the stop, which clears it.
    const elapsed = running?.currentTime ?? 0;
    if (running !== null) {
      from = dialog.offsetHeight;
    }
    stop(watch);
    const to = dialog.offsetHeight;
    if (Math.abs(to - watch.height) < 1) {
      // The content kept its height: a running animation goes on where it was.
      if (running !== null) {
        play(watch, watch.from, watch.height, elapsed);
      }
      return;
    }
    watch.height = to;
    play(watch, from, to);
  };
  const observe = (watch) => {
    for (const part of watch.parts) {
      if (!part.isConnected) {
        watch.resize.unobserve(part);
        watch.parts.delete(part);
      }
    }
    for (const part of parts(watch.dialog)) {
      if (!watch.parts.has(part)) {
        watch.parts.add(part);
        watch.resize.observe(part);
      }
    }
  };
  const watch = (dialog) => {
    if (watches.has(dialog)) {
      return;
    }
    const entry = {
      dialog,
      // offsetHeight leaves out the transform of the dialog's entrance.
      height: dialog.offsetHeight,
      from: dialog.offsetHeight,
      running: null,
      squeezed: [],
      parts: new Set(),
    };
    entry.resize = new ResizeObserver(() => changed(entry));
    // A part that comes or goes changes the height too.
    entry.mutations = new MutationObserver(() => {
      observe(entry);
      changed(entry);
    });
    entry.mutations.observe(dialog, { childList: true, subtree: true });
    watches.set(dialog, entry);
    observe(entry);
  };
  // A new window size changes a dialog's fixed or largest height, and no part may report it. The
  // dialog takes its new height without an animation, and the next change animates from there.
  window.addEventListener("resize", () => {
    for (const entry of watches.values()) {
      stop(entry);
      entry.height = entry.dialog.offsetHeight;
    }
  });
  const forget = () => {
    for (const [dialog, entry] of watches) {
      if (!dialog.isConnected) {
        entry.resize.disconnect();
        entry.mutations.disconnect();
        watches.delete(dialog);
      }
    }
  };
  const dialogsIn = (node) => {
    if (!(node instanceof Element)) {
      return [];
    }
    if (node.classList.contains(settings.dialog_class)) {
      return [node];
    }
    return node.classList.contains(settings.overlay_class)
      ? Array.from(node.querySelectorAll(`.${settings.dialog_class}`))
      : [];
  };
  // The page changes often while agents work: only an added overlay or dialog is looked into.
  new MutationObserver((records) => {
    let removed = false;
    for (const record of records) {
      record.addedNodes.forEach((node) => dialogsIn(node).forEach(watch));
      removed ||= record.removedNodes.length > 0;
    }
    if (removed && watches.size > 0) {
      forget();
    }
  }).observe(document.body, { childList: true, subtree: true });
  document.querySelectorAll(`.${settings.dialog_class}`).forEach(watch);
}
"#;

/// The names the scripts read: the motion tokens of the interface rules, a growth's and a
/// shrink's, and, for the dialogs' script, the classes it finds dialogs by.
#[derive(Serialize)]
struct Settings {
    dialog_class: &'static str,
    overlay_class: &'static str,
    grow_duration: &'static str,
    grow_easing: &'static str,
    shrink_duration: &'static str,
    shrink_easing: &'static str,
}

const SETTINGS: Settings = Settings {
    dialog_class: "dialog",
    overlay_class: OVERLAY_CLASS,
    grow_duration: "--motion-in",
    grow_easing: "--ease-out",
    shrink_duration: "--motion-out",
    shrink_easing: "--ease-in",
};

/// What [`follow_content`]'s element sends, bubbling, once its height changed or started to: a
/// script that places it by its height places it again, in the same frame.
pub const HEIGHT_EVENT: &str = "cockpit-height";

/// Animates one element's height as its content grows or shrinks, such as text that wraps onto
/// more lines. Given the element's id, it watches the element's first child, its content, and
/// animates the element from its last height to the new one. The element clips its content. An
/// element in a `leaving` one plays that exit alone. It reads the motion tokens of `Settings`.
const FOLLOW_SCRIPT: &str = r#"
const id = await dioxus.recv();
const heightEvent = await dioxus.recv();
const leavingClass = await dioxus.recv();
const settings = await dioxus.recv();
const clip = document.getElementById(id);
const body = clip?.firstElementChild;
if (body) {
  const token = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  const millis = (name) => {
    const value = token(name);
    const number = parseFloat(value);
    if (Number.isNaN(number)) {
      return 0;
    }
    return value.endsWith("ms") ? number : number * 1000;
  };
  let height = body.offsetHeight;
  let running = null;
  const resize = new ResizeObserver(() => {
    if (!clip.isConnected) {
      resize.disconnect();
      return;
    }
    const to = body.offsetHeight;
    if (Math.abs(to - height) < 1) {
      return;
    }
    // A running animation goes on from where it is.
    const from = running === null ? height : clip.offsetHeight;
    running?.cancel();
    running = null;
    height = to;
    const grows = to > from;
    const duration = millis(grows ? settings.grow_duration : settings.shrink_duration);
    if (duration > 0 && clip.closest(`.${leavingClass}`) === null) {
      const animation = clip.animate(
        [{ height: `${from}px` }, { height: `${to}px` }],
        { duration, easing: token(grows ? settings.grow_easing : settings.shrink_easing) },
      );
      animation.onfinish = () => {
        if (running === animation) {
          running = null;
        }
      };
      running = animation;
    }
    // Sent from here, after the animation starts: an observer of the element's own size sees
    // the new content's full height first in this frame.
    clip.dispatchEvent(new Event(heightEvent, { bubbles: true }));
  });
  resize.observe(body);
}
"#;

/// Animates the height of the element `id` names, which is on the page, as its content changes.
/// Run it when the element mounts.
pub fn follow_content(id: &'static str) {
    let script = document::eval(FOLLOW_SCRIPT);
    let sent = script
        .send(id)
        .and_then(|()| script.send(HEIGHT_EVENT))
        .and_then(|()| script.send(LEAVING))
        .and_then(|()| script.send(SETTINGS));
    if let Err(error) = sent {
        tracing::warn!("cannot animate the height of {id}: {error}");
    }
}

/// Starts the script once, for every dialog the window shows from then on.
pub fn start() {
    let script = document::eval(SCRIPT);
    if let Err(error) = script.send(SETTINGS) {
        tracing::warn!("cannot start the script that animates the dialogs' height: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_reads_each_setting_by_its_serde_name() {
        let value = serde_json::to_value(SETTINGS).unwrap();
        let names: Vec<&String> = value.as_object().unwrap().keys().collect();

        assert_eq!(names.len(), 6);
        for name in names {
            assert!(
                SCRIPT.contains(&format!("settings.{name}")),
                "the script does not read {name}"
            );
        }
    }

    /// A name the script reads that the app does not send is `undefined` there, with no error.
    #[test]
    fn the_follow_script_reads_the_motion_settings_the_app_sends() {
        let value = serde_json::to_value(SETTINGS).unwrap();
        let sent = value.as_object().unwrap();
        let read: Vec<&str> = FOLLOW_SCRIPT
            .split("settings.")
            .skip(1)
            .filter_map(|rest| {
                rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .next()
            })
            .collect();

        assert_eq!(
            read,
            [
                "grow_duration",
                "shrink_duration",
                "grow_easing",
                "shrink_easing"
            ]
        );
        for name in read {
            assert!(sent.contains_key(name), "the app does not send {name}");
        }
    }
}
