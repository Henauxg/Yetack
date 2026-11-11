//! An image you zoom with Ctrl+wheel, around the pointer, and drag once zoomed. At first a large
//! image fits its frame and a small one keeps its own size: a double-click or Ctrl+0 shows it so
//! again. A click on it unzoomed runs `on_click`, which the file view uses to open the full view.

use std::rc::Rc;
use std::sync::Arc;

use cockpit_protocol::Image;
use dioxus::html::geometry::{ClientPoint, ElementPoint, PixelsSize, PixelsVector2D, WheelDelta};
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use crate::bridge;

/// The image's first size is 1: fitted to its frame when large, its own size when small. It goes no
/// further than 8 times that.
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
/// moved by `offset`. The layer that holds it always covers the frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zoom {
    scale: f64,
    offset: PixelsVector2D,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            scale: MIN_SCALE,
            offset: PixelsVector2D::zero(),
        }
    }
}

impl Zoom {
    /// Zooms by `factor` and keeps the image's point under `point` there.
    pub fn zoomed_at(self, point: ElementPoint, factor: f64, frame: PixelsSize) -> Self {
        let scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let ratio = scale / self.scale;
        let point: PixelsVector2D = point.to_vector().cast_unit();
        Self {
            scale,
            offset: point - (point - self.offset) * ratio,
        }
        .kept_in(frame)
    }

    /// Moves the image by `by`, as far as it still covers the frame.
    pub fn panned(self, by: PixelsVector2D, frame: PixelsSize) -> Self {
        Self {
            offset: self.offset + by,
            ..self
        }
        .kept_in(frame)
    }

    pub fn is_zoomed(self) -> bool {
        self.scale > MIN_SCALE
    }

    fn kept_in(self, frame: PixelsSize) -> Self {
        let room =
            |offset: f64, length: f64| offset.clamp((length * (1.0 - self.scale)).min(0.0), 0.0);
        Self {
            offset: PixelsVector2D::new(
                room(self.offset.x, frame.width),
                room(self.offset.y, frame.height),
            ),
            ..self
        }
    }

    fn transform(self) -> String {
        format!(
            "translate({:.1}px, {:.1}px) scale({:.4})",
            self.offset.x, self.offset.y, self.scale
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
    start: ClientPoint,
    /// The zoom when it started.
    zoom: Zoom,
    dragged: bool,
}

impl Press {
    /// Whether its release is a click: a press that did not drag, on an image that fits. On a
    /// zoomed image, the first click of a double-click would run `on_click`.
    fn is_click(self) -> bool {
        !self.dragged && !self.zoom.is_zoomed()
    }
}

/// The image, fitted to its frame when large, at its own size when small. The style sheet sizes the
/// frame.
#[component]
pub fn ZoomImage(
    image: Arc<Image>,
    alt: String,
    id: Option<String>,
    on_click: Option<EventHandler<()>>,
) -> Element {
    let mut zoom = use_signal(Zoom::default);
    let mut size = use_signal(PixelsSize::zero);
    let mut press = use_signal(|| None::<Press>);
    let mut frame = use_signal(|| None::<Rc<MountedData>>);
    // A new image starts fitted.
    use_effect(use_reactive(&image, move |_| zoom.set(Zoom::default())));
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
            onmounted: move |event| frame.set(Some(event.data())),
            onresize: move |event: Event<ResizeData>| {
                if let Ok(box_size) = event.get_content_box_size() {
                    size.set(box_size);
                    zoom.set(zoom().kept_in(size()));
                }
            },
            onwheel: move |event: Event<WheelData>| {
                if !event.modifiers().ctrl() {
                    return;
                }
                event.prevent_default();
                let factor = wheel_factor(event.delta());
                zoom.set(zoom().zoomed_at(event.element_coordinates(), factor, size()));
            },
            onmousedown: move |event: Event<MouseData>| {
                if event.trigger_button() != Some(MouseButton::Primary) {
                    return;
                }
                // A press starts a drag, not a text selection. That also keeps the focus off the
                // frame, so the frame takes it here, for Ctrl+0 and Ctrl+=.
                event.prevent_default();
                if let Some(frame) = frame() {
                    spawn(async move {
                        let _ = frame.set_focus(true).await;
                    });
                }
                let start = event.client_coordinates();
                press.set(Some(Press { start, zoom: zoom(), dragged: false }));
            },
            onmousemove: move |event: Event<MouseData>| {
                let Some(mut held) = press() else {
                    return;
                };
                let moved: PixelsVector2D = (event.client_coordinates() - held.start).cast_unit();
                if !held.dragged && moved.length() < DRAG_THRESHOLD {
                    return;
                }
                held.dragged = true;
                press.set(Some(held));
                zoom.set(held.zoom.panned(moved, size()));
            },
            onmouseup: move |_| {
                let released = press.take();
                if released.is_some_and(Press::is_click)
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
                let center = ElementPoint::new(size().width / 2.0, size().height / 2.0);
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
            div { class: "zoom-layer", style: "transform: {current.transform()}",
                Picture { image, alt: alt.clone() }
            }
        }
    }
}

/// The image itself, apart so that a zoom or a drag does not render it again: its address holds
/// the whole file in base64, up to 11 MB.
#[component]
fn Picture(image: Arc<Image>, alt: String) -> Element {
    rsx! {
        img { src: bridge::image_url(&image), alt: "{alt}", draggable: false }
    }
}

#[cfg(test)]
mod tests {
    use dioxus::html::geometry::{LinesVector, PixelsVector3D};

    use super::*;

    fn frame() -> PixelsSize {
        PixelsSize::new(400.0, 300.0)
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let zoomed = Zoom::default().zoomed_at(ElementPoint::new(100.0, 50.0), 2.0, frame());

        // The image's point (100, 50) at scale 2 lands at 2 * 100 + x = 100.
        assert_eq!(
            zoomed,
            Zoom {
                scale: 2.0,
                offset: PixelsVector2D::new(-100.0, -50.0),
            }
        );
    }

    #[test]
    fn the_scale_stays_between_the_fit_and_eight_times() {
        let point = ElementPoint::new(10.0, 10.0);

        let out = Zoom::default().zoomed_at(point, 0.5, frame());
        let far = Zoom::default().zoomed_at(point, 100.0, frame());

        assert_eq!(out, Zoom::default());
        assert!((far.scale - MAX_SCALE).abs() < f64::EPSILON);
    }

    #[test]
    fn the_image_always_covers_its_frame() {
        let zoomed = Zoom::default().zoomed_at(ElementPoint::origin(), 2.0, frame());

        let too_far_right = zoomed.panned(PixelsVector2D::new(50.0, 0.0), frame());
        let too_far_left = zoomed.panned(PixelsVector2D::new(-1000.0, -1000.0), frame());
        let fitted = Zoom::default().panned(PixelsVector2D::new(30.0, 30.0), frame());

        assert!(too_far_right.offset.x.abs() < f64::EPSILON);
        assert!((too_far_left.offset.x + 400.0).abs() < f64::EPSILON);
        assert!((too_far_left.offset.y + 300.0).abs() < f64::EPSILON);
        assert_eq!(fitted, Zoom::default());
    }

    #[test]
    fn a_press_that_does_not_drag_is_a_click_only_on_an_image_that_fits() {
        let zoomed = Zoom::default().zoomed_at(ElementPoint::origin(), 2.0, frame());
        let press = |zoom| Press {
            start: ClientPoint::new(10.0, 10.0),
            zoom,
            dragged: false,
        };

        assert!(press(Zoom::default()).is_click());
        assert!(!press(zoomed).is_click());
    }

    #[test]
    fn a_wheel_turned_up_zooms_in_and_down_zooms_out() {
        let up = wheel_factor(WheelDelta::Pixels(PixelsVector3D::new(0.0, -100.0, 0.0)));
        let down = wheel_factor(WheelDelta::Lines(LinesVector::new(0.0, 3.0, 0.0)));

        assert!(up > 1.0 && up < 1.5, "{up}");
        assert!(down < 1.0 && down > 0.5, "{down}");
    }
}
