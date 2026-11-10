//! Turns off WebKitGTK's accelerated compositing. With it on, WebKitGTK 2.54 draws an animation's
//! first frame once more after its last, at random: a popover, a dialog or a card flashes as it
//! opens or closes. Without it, the page draws on the main thread, and the window's animations,
//! short and small, still run at the screen's rate.

use dioxus::desktop::wry::{WebView, WebViewExtUnix};
use webkit2gtk::{HardwareAccelerationPolicy, SettingsExt, WebViewExt};

pub fn turn_off_accelerated_compositing(webview: &WebView) {
    match webview.webview().settings() {
        Some(settings) => {
            settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Never)
        }
        None => tracing::warn!("the web view has no settings: its animations may flash"),
    }
}
