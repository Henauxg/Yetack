//! An image you zoom with Ctrl+wheel, around the pointer, and drag once zoomed. A double-click or
//! Ctrl+0 fits it again. A click that does not drag runs `on_click`, which the file view uses to
//! open the full view.

use dioxus::html::geometry::WheelDelta;
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

/// The image fills its frame at 1, and goes no further than 8 times that.
const MIN_SCALE: f64 = 1.0;
const MAX_SCALE: f64 = 8.0;

/// How much a pixel of wheel zooms: a mouse's notch, about 100 pixels, zooms by about a fifth.
const ZOOM_PER_PIXEL: f64 = 0.002;

/// A wheel that counts lines or pages, as most mice under X do, moves this many pixels a step.
const PIXELS_PER_LINE: f64 = 40.0;
const PIXELS_PER_PAGE: f64 = 400.0;

/// The zoom of Ctrl+= and Ctrl+-, around the frame's center.
const KEY_ZOOM: f64 = 1.25;

/// A press that moves less than this, in pixels, is a click and not a drag.
const DRAG_THRESHOLD: f64 = 4.0;

/// Where the image is in its frame: scaled by `scale` from the frame's top left corner, then
/// moved by `x` and `y` pixels. It always covers the frame it fills at 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zoom {
    scale: f64,
    x: f64,
    y: f64,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            scale: MIN_SCALE,
            x: 0.0,
            y: 0.0,
        }
    }
}

impl Zoom {
    /// Zooms by `factor` and keeps the image's point under `point` there. `size` is the frame's.
    pub fn zoomed_at(self, point: (f64, f64), factor: f64, size: (f64, f64)) -> Self {
        let scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let ratio = scale / self.scale;
        Self {
            scale,
            x: point.0 - (point.0 - self.x) * ratio,
            y: point.1 - (point.1 - self.y) * ratio,
        }
        .kept_in(size)
    }

    /// Moves the image by `dx` and `dy`, as far as it still covers the frame.
    pub fn panned(self, dx: f64, dy: f64, size: (f64, f64)) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..self
        }
        .kept_in(size)
    }

    pub fn is_zoomed(self) -> bool {
        self.scale > MIN_SCALE
    }

    fn kept_in(self, (width, height): (f64, f64)) -> Self {
        let room =
            |offset: f64, length: f64| offset.clamp((length * (1.0 - self.scale)).min(0.0), 0.0);
        Self {
            x: room(self.x, width),
            y: room(self.y, height),
            ..self
        }
    }

    fn transform(self) -> String {
        format!(
            "translate({:.1}px, {:.1}px) scale({:.4})",
            self.x, self.y, self.scale
        )
    }
}

/// The zoom a wheel's turn asks for: up zooms in, down zooms out.
fn wheel_factor(delta: WheelDelta) -> f64 {
    let pixels = match delta {
        WheelDelta::Pixels(vector) => vector.y,
        WheelDelta::Lines(vector) => vector.y * PIXELS_PER_LINE,
        WheelDelta::Pages(vector) => vector.y * PIXELS_PER_PAGE,
    };
    (-pixels * ZOOM_PER_PIXEL).exp()
}

/// A press on the image, until it is released.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Press {
    /// Where it started, in the window's pixels.
    start: (f64, f64),
    /// The zoom when it started.
    zoom: Zoom,
    dragged: bool,
}

/// The image `src`, fitted to its frame, which the style sheet sizes.
#[component]
pub fn ZoomImage(
    src: String,
    alt: String,
    id: Option<String>,
    on_click: Option<EventHandler<()>>,
) -> Element {
    let mut zoom = use_signal(Zoom::default);
    let mut size = use_signal(|| (0.0, 0.0));
    let mut press = use_signal(|| None::<Press>);
    // A new image starts fitted.
    use_effect(use_reactive(&src, move |_| zoom.set(Zoom::default())));
    let current = zoom();
    let mut class = vec!["zoom-frame"];
    if current.is_zoomed() {
        class.push("zoomed");
    }
    if press().is_some_and(|press| press.dragged) {
        class.push("dragging");
    }
    let clickable = on_click.is_some();
    if clickable && !current.is_zoomed() {
        class.push("opens");
    }
    rsx! {
        div {
            id,
            class: class.join(" "),
            tabindex: 0,
            role: "img",
            aria_label: "{alt}. Ctrl+wheel zooms, a double-click fits it again.",
            onresize: move |event: Event<ResizeData>| {
                if let Ok(box_size) = event.get_content_box_size() {
                    size.set((box_size.width, box_size.height));
                    zoom.set(zoom().kept_in(size()));
                }
            },
            onwheel: move |event: Event<WheelData>| {
                if !event.modifiers().ctrl() {
                    return;
                }
                event.prevent_default();
                let point = event.element_coordinates();
                let factor = wheel_factor(event.delta());
                zoom.set(zoom().zoomed_at((point.x, point.y), factor, size()));
            },
            onmousedown: move |event: Event<MouseData>| {
                if event.trigger_button() != Some(MouseButton::Primary) {
                    return;
                }
                // A press starts a drag, not a text selection.
                event.prevent_default();
                let point = event.client_coordinates();
                press.set(Some(Press { start: (point.x, point.y), zoom: zoom(), dragged: false }));
            },
            onmousemove: move |event: Event<MouseData>| {
                let Some(mut held) = press() else {
                    return;
                };
                let point = event.client_coordinates();
                let (dx, dy) = (point.x - held.start.0, point.y - held.start.1);
                if !held.dragged && dx.hypot(dy) < DRAG_THRESHOLD {
                    return;
                }
                held.dragged = true;
                press.set(Some(held));
                zoom.set(held.zoom.panned(dx, dy, size()));
            },
            onmouseup: move |_| {
                let released = press.take();
                if released.is_some_and(|released| !released.dragged)
                    && let Some(on_click) = on_click
                {
                    on_click.call(());
                }
            },
            onmouseleave: move |_| press.set(None),
            ondoubleclick: move |_| zoom.set(Zoom::default()),
            onkeydown: move |event: Event<KeyboardData>| {
                if !event.modifiers().ctrl() {
                    return;
                }
                let center = (size().0 / 2.0, size().1 / 2.0);
                let next = match event.key() {
                    Key::Character(key) if key == "0" => Zoom::default(),
                    Key::Character(key) if key == "=" || key == "+" => {
                        zoom().zoomed_at(center, KEY_ZOOM, size())
                    }
                    Key::Character(key) if key == "-" => {
                        zoom().zoomed_at(center, 1.0 / KEY_ZOOM, size())
                    }
                    _ => return,
                };
                event.prevent_default();
                zoom.set(next);
            },
            img { src, alt: "{alt}", style: "transform: {current.transform()}", draggable: false }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: (f64, f64) = (400.0, 300.0);

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let zoomed = Zoom::default().zoomed_at((100.0, 50.0), 2.0, FRAME);

        // The image's point (100, 50) at scale 2 lands at 2 * 100 + x = 100.
        assert_eq!(
            zoomed,
            Zoom {
                scale: 2.0,
                x: -100.0,
                y: -50.0
            }
        );
    }

    #[test]
    fn the_scale_stays_between_the_fit_and_eight_times() {
        let out = Zoom::default().zoomed_at((10.0, 10.0), 0.5, FRAME);
        let far = Zoom::default().zoomed_at((10.0, 10.0), 100.0, FRAME);

        assert_eq!(out, Zoom::default());
        assert!((far.scale - MAX_SCALE).abs() < f64::EPSILON);
    }

    #[test]
    fn the_image_always_covers_its_frame() {
        let zoomed = Zoom::default().zoomed_at((0.0, 0.0), 2.0, FRAME);

        let too_far_right = zoomed.panned(50.0, 0.0, FRAME);
        let too_far_left = zoomed.panned(-1000.0, -1000.0, FRAME);
        let fitted = Zoom::default().panned(30.0, 30.0, FRAME);

        assert!((too_far_right.x).abs() < f64::EPSILON);
        assert!((too_far_left.x + 400.0).abs() < f64::EPSILON);
        assert!((too_far_left.y + 300.0).abs() < f64::EPSILON);
        assert_eq!(fitted, Zoom::default());
    }

    #[test]
    fn a_wheel_turned_up_zooms_in_and_down_zooms_out() {
        use dioxus::html::geometry::{LinesVector, PixelsVector3D};

        let up = wheel_factor(WheelDelta::Pixels(PixelsVector3D::new(0.0, -100.0, 0.0)));
        let down = wheel_factor(WheelDelta::Lines(LinesVector::new(0.0, 3.0, 0.0)));

        assert!(up > 1.0 && up < 1.5, "{up}");
        assert!(down < 1.0 && down > 0.5, "{down}");
    }
}
