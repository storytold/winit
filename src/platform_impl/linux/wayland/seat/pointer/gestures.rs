//! Touchpad pinch and rotation gestures (`zwp_pointer_gestures_v1`).
//!
//! The compositor recognises the gesture and reports it for the focused surface. A pinch is
//! reported with `scale`, the distance between the fingers relative to where the gesture began,
//! and `rotation`, the angle in degrees clockwise since the previous event.
//!
//! winit's [`WindowEvent::PinchGesture`] carries a change since the last event instead, so the
//! scale is turned into the natural log of the ratio to the previous scale. A consumer that
//! applies `exp(delta)` (egui does) then multiplies its zoom by exactly the change the fingers
//! made, and the product over the gesture is the final scale. For small steps this is the same
//! as the proportional change macOS reports.

use std::ops::Deref;
use std::sync::Mutex;

use sctk::globals::GlobalData;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, Proxy, QueueHandle};
use sctk::reexports::protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gesture_pinch_v1::{
    self, ZwpPointerGesturePinchV1,
};
use sctk::reexports::protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gestures_v1::ZwpPointerGesturesV1;

use sctk::compositor::SurfaceData;

use crate::event::{TouchPhase, WindowEvent};
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId, WindowId};

/// Pinches are the two-finger gesture. libinput also reports three and four finger pinches,
/// which compositors use for their own shortcuts.
const PINCH_FINGERS: u32 = 2;

/// Wrapper around the pointer gestures manager.
pub struct PointerGesturesState {
    manager: ZwpPointerGesturesV1,
}

impl PointerGesturesState {
    /// Bind the pointer gestures manager. Pinch is part of the first version of the protocol.
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        let manager = globals.bind(queue_handle, 1..=1, GlobalData)?;
        Ok(Self { manager })
    }
}

impl Deref for PointerGesturesState {
    type Target = ZwpPointerGesturesV1;

    fn deref(&self) -> &Self::Target {
        &self.manager
    }
}

/// What is remembered between the events of one pinch gesture.
#[derive(Debug, Default)]
pub struct PinchGestureData {
    inner: Mutex<PinchGestureInner>,
}

#[derive(Debug, Default)]
struct PinchGestureInner {
    /// The window the gesture began on; `None` when no gesture of ours is in progress.
    window_id: Option<WindowId>,
    /// The `scale` of the previous event, `1.0` when the gesture begins.
    last_scale: f64,
}

/// The change from `previous` to `scale`, as the natural log of their ratio.
///
/// Anything that isn't a positive finite number changes nothing: a zero or negative scale would
/// make the log infinite or NaN, and the consumer would zoom to nothing.
fn pinch_delta(previous: f64, scale: f64) -> f64 {
    if previous.is_finite() && scale.is_finite() && previous > 0.0 && scale > 0.0 {
        (scale / previous).ln()
    } else {
        0.0
    }
}

/// Wayland measures rotation in degrees clockwise; winit's positive values are counterclockwise.
fn rotation_delta(clockwise_degrees: f64) -> f32 {
    if clockwise_degrees.is_finite() {
        -(clockwise_degrees as f32)
    } else {
        0.0
    }
}

impl Dispatch<ZwpPointerGesturesV1, GlobalData, WinitState> for PointerGesturesState {
    fn event(
        _state: &mut WinitState,
        _proxy: &ZwpPointerGesturesV1,
        _event: <ZwpPointerGesturesV1 as Proxy>::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch<ZwpPointerGesturePinchV1, PinchGestureData, WinitState> for PointerGesturesState {
    fn event(
        state: &mut WinitState,
        _proxy: &ZwpPointerGesturePinchV1,
        event: <ZwpPointerGesturePinchV1 as Proxy>::Event,
        data: &PinchGestureData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
        let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));
        let mut inner = data.inner.lock().unwrap();

        match event {
            zwp_pointer_gesture_pinch_v1::Event::Begin { surface, fingers, .. } => {
                inner.window_id = None;
                if fingers != PINCH_FINGERS {
                    return;
                }

                // A gesture on the decorations belongs to the window they are part of.
                let parent_surface = match surface.data::<SurfaceData>() {
                    Some(data) => data.parent_surface().unwrap_or(&surface),
                    None => return,
                };
                let window_id = wayland::make_wid(parent_surface);
                if !state.windows.get_mut().contains_key(&window_id) {
                    return;
                }

                inner.window_id = Some(window_id);
                inner.last_scale = 1.0;
                state.events_sink.push_window_event(
                    WindowEvent::PinchGesture {
                        device_id,
                        delta: 0.0,
                        phase: TouchPhase::Started,
                    },
                    window_id,
                );
                state.events_sink.push_window_event(
                    WindowEvent::RotationGesture {
                        device_id,
                        delta: 0.0,
                        phase: TouchPhase::Started,
                    },
                    window_id,
                );
            },
            zwp_pointer_gesture_pinch_v1::Event::Update { scale, rotation, .. } => {
                let Some(window_id) = inner.window_id else { return };

                let delta = pinch_delta(inner.last_scale, scale);
                if scale.is_finite() && scale > 0.0 {
                    inner.last_scale = scale;
                }
                state.events_sink.push_window_event(
                    WindowEvent::PinchGesture { device_id, delta, phase: TouchPhase::Moved },
                    window_id,
                );

                // Most pinches don't turn, and a stream of zero rotations is only noise.
                let rotation = rotation_delta(rotation);
                if rotation != 0.0 {
                    state.events_sink.push_window_event(
                        WindowEvent::RotationGesture {
                            device_id,
                            delta: rotation,
                            phase: TouchPhase::Moved,
                        },
                        window_id,
                    );
                }
            },
            zwp_pointer_gesture_pinch_v1::Event::End { cancelled, .. } => {
                let Some(window_id) = inner.window_id.take() else { return };

                let phase = if cancelled != 0 { TouchPhase::Cancelled } else { TouchPhase::Ended };
                state.events_sink.push_window_event(
                    WindowEvent::PinchGesture { device_id, delta: 0.0, phase },
                    window_id,
                );
                state.events_sink.push_window_event(
                    WindowEvent::RotationGesture { device_id, delta: 0.0, phase },
                    window_id,
                );
            },
            _ => (),
        }
    }
}

delegate_dispatch!(WinitState: [ZwpPointerGesturesV1: GlobalData] => PointerGesturesState);
delegate_dispatch!(WinitState: [ZwpPointerGesturePinchV1: PinchGestureData] => PointerGesturesState);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_multiply_back_to_the_final_scale() {
        // The scales a compositor reports, relative to the start of the gesture.
        let scales = [1.04, 1.13, 1.31, 1.52, 1.4, 0.9];
        let mut previous = 1.0;
        let mut zoom = 1.0_f64;
        for scale in scales {
            zoom *= pinch_delta(previous, scale).exp();
            previous = scale;
        }
        assert!((zoom - 0.9).abs() < 1e-12, "zoom {zoom}");
    }

    #[test]
    fn spreading_zooms_in_and_pinching_zooms_out() {
        assert!(pinch_delta(1.0, 1.2) > 0.0);
        assert!(pinch_delta(1.2, 1.0) < 0.0);
        assert_eq!(pinch_delta(1.5, 1.5), 0.0);
    }

    #[test]
    fn a_bad_scale_changes_nothing() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(pinch_delta(1.0, bad), 0.0);
            assert_eq!(pinch_delta(bad, 1.0), 0.0);
        }
    }

    #[test]
    fn clockwise_wayland_rotation_is_negative_in_winit() {
        assert_eq!(rotation_delta(10.0), -10.0);
        assert_eq!(rotation_delta(-2.5), 2.5);
        assert_eq!(rotation_delta(f64::NAN), 0.0);
    }
}
