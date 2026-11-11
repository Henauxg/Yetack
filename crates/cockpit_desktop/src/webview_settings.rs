//! Turns off WebKitGTK's accelerated compositing. With it on, WebKitGTK 2.54 draws an animation's
//! first frame once more after its last, at random: a popover, a dialog or a card flashes as it
//! opens or closes. Without it, the page draws on the main thread, and the window's animations,
//! short and small, still run at the screen's rate.
//!
//! It goes through the environment, which WebKitGTK reads as it creates the web view. The web
//! view's hardware acceleration setting, changed once the view exists, stops the view's drawing at
//! its first resize to full screen: the window stays blank until the app restarts.

use std::ffi::OsStr;

/// The variable that turns accelerated compositing off before the web view exists.
const COMPOSITING_VAR: &str = "WEBKIT_DISABLE_COMPOSITING_MODE";

/// Sets the variable, unless the user set it, to keep compositing on with `0` for example.
///
/// Must run before the program starts any thread: `main` calls it first.
pub fn turn_off_accelerated_compositing() {
    if !needs_setting(std::env::var_os(COMPOSITING_VAR).as_deref()) {
        return;
    }
    // SAFETY: `main` calls this before it starts any thread, so nothing reads or writes the
    // environment at the same time. Safe code cannot do it: WebKitGTK reads the variable from the
    // process's environment, and a restart of the app with the variable set would be the only
    // other way.
    unsafe { std::env::set_var(COMPOSITING_VAR, "1") };
}

/// Whether the app sets the variable, given its value at the start.
fn needs_setting(current: Option<&OsStr>) -> bool {
    current.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variable_is_set_only_when_the_user_did_not_set_it() {
        assert!(needs_setting(None));
        assert!(!needs_setting(Some(OsStr::new("0"))));
        assert!(!needs_setting(Some(OsStr::new("1"))));
        assert!(!needs_setting(Some(OsStr::new(""))));
    }
}
