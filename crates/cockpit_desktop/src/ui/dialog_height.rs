//! A dialog's height follows its content with an animation: a list that gets shorter, a tab
//! switch or a note that shows. CSS cannot animate a height that follows the content, so a page
//! script watches each dialog's parts and animates the dialog from its last height to its new one.

use dioxus::prelude::*;
use serde::Serialize;

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
  // A dialog's parts whose height is their content's: its children, and the children of a child
  // that scrolls, since the dialog's own height shrinks that one.
  const parts = (dialog) => Array.from(dialog.children).flatMap((child) => {
    const scrolls = ["auto", "scroll"].includes(getComputedStyle(child).overflowY);
    return scrolls ? Array.from(child.children) : [child];
  });
  const play = (watch, from, to, elapsed = 0) => {
    const grows = to > from;
    const duration = millis(grows ? settings.grow_duration : settings.shrink_duration);
    if (duration <= 0) {
      return;
    }
    const animation = watch.dialog.animate(
      [{ height: `${from}px` }, { height: `${to}px` }],
      { duration, easing: token(grows ? settings.grow_easing : settings.shrink_easing) },
    );
    animation.currentTime = elapsed;
    animation.onfinish = () => {
      if (watch.running === animation) {
        watch.running = null;
      }
    };
    watch.running = animation;
    watch.from = from;
  };
  const changed = (watch) => {
    const dialog = watch.dialog;
    let from = watch.height;
    const running = watch.running;
    // Read before the cancel, which clears it.
    const elapsed = running?.currentTime ?? 0;
    if (running !== null) {
      from = dialog.offsetHeight;
      running.cancel();
      watch.running = null;
    }
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

/// The names the script reads: the motion tokens of the interface rules, a growth's and a
/// shrink's, and the classes it finds dialogs by.
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
    overlay_class: "overlay",
    grow_duration: "--motion-in",
    grow_easing: "--ease-out",
    shrink_duration: "--motion-out",
    shrink_easing: "--ease-in",
};

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
}
