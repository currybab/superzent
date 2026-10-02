use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, Div, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Task, Window, div,
};

/// A self-contained pulsing wrapper for small attention dots.
///
/// `gpui::with_animation` calls `request_animation_frame` on every frame, which
/// drives a full-window relayout at the display refresh rate (120Hz on ProMotion)
/// for as long as the animated element is on screen. A tiny attention dot only
/// needs a low frame rate to read as a pulse, so this element schedules its own
/// redraw on a fixed interval instead, keeping the whole window from re-laying
/// out at the display rate while the dot is visible.
pub struct PulsingDot {
    id: ElementId,
    period: Duration,
    interval: Duration,
    element: Option<Div>,
    animator: Box<dyn Fn(Div, f32) -> Div + 'static>,
}

impl PulsingDot {
    pub fn new(
        id: impl Into<ElementId>,
        period: Duration,
        element: Div,
        animator: impl Fn(Div, f32) -> Div + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            period,
            interval: Duration::from_millis(50),
            element: Some(element),
            animator: Box::new(animator),
        }
    }

    /// How often the dot redraws. A stepped animator only needs one redraw per
    /// step, so it can use an interval far longer than a smooth pulse.
    pub fn redraw_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}

struct PulsingDotState {
    start: Instant,
    // Holds the scheduled redraw alive; dropping it cancels the wake-up, so the
    // pulse stops automatically once the element leaves the tree.
    _redraw: Option<Task<()>>,
}

impl IntoElement for PulsingDot {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PulsingDot {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let Some(global_id) = global_id else {
            let mut element = self.element.take().unwrap_or_else(div).into_any_element();
            let layout_id = element.request_layout(window, cx);
            return (layout_id, element);
        };
        window.with_element_state(global_id, |state, window| {
            let state = state.unwrap_or_else(|| PulsingDotState {
                start: Instant::now(),
                _redraw: None,
            });
            let elapsed = state.start.elapsed();
            let delta = (elapsed.as_secs_f32() / self.period.as_secs_f32()).fract();

            let element = self.element.take().unwrap_or_else(div);
            let mut element = (self.animator)(element, delta).into_any_element();

            // Wake this view at the next interval boundary rather than on the next
            // frame, decoupling the pulse cadence from the display refresh rate.
            // Aligning to the boundary keeps stepped animators from drifting.
            let view = window.current_view();
            let interval_nanos = self.interval.as_nanos().max(1);
            let remaining_nanos = interval_nanos - elapsed.as_nanos() % interval_nanos;
            let delay = Duration::from_nanos(u64::try_from(remaining_nanos).unwrap_or(u64::MAX));
            let redraw = cx.spawn(async move |cx| {
                cx.background_executor().timer(delay).await;
                cx.update(|cx| cx.notify(view));
            });

            let layout_id = element.request_layout(window, cx);
            (
                (layout_id, element),
                PulsingDotState {
                    start: state.start,
                    _redraw: Some(redraw),
                },
            )
        })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}
