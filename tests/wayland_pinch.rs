#![cfg(target_os = "linux")]

mod event {
    pub use winit::event::TouchPhase;
}

#[path = "../src/platform_impl/linux/wayland/seat/pointer/pinch_state.rs"]
mod pinch_state;

use event::TouchPhase;
use pinch_state::PinchState;

#[test]
fn cumulative_scales_produce_multiplicative_zoom_in_both_directions() {
    let mut pinch = PinchState::default();
    let begin = pinch.begin(Some(7), 2).unwrap();
    assert_eq!(begin.phase, TouchPhase::Started);
    assert_eq!(begin.delta, 0.0);
    let mut total = 1.0;
    for scale in [1.25, 1.5, 2.0, 0.75, 1.0] {
        let update = pinch.update(scale).unwrap();
        assert_eq!(update.window, 7);
        assert_eq!(update.phase, TouchPhase::Moved);
        total *= update.delta.exp();
        assert!((total - scale).abs() < 1e-12);
    }
}

#[test]
fn ending_or_cancelling_resets_the_next_gesture() {
    for cancelled in [false, true] {
        let mut pinch = PinchState::default();
        pinch.begin(Some(3), 2).unwrap();
        pinch.update(2.0).unwrap();
        let end = pinch.end(cancelled).unwrap();
        assert_eq!(end.phase, if cancelled { TouchPhase::Cancelled } else { TouchPhase::Ended });
        assert_eq!(end.delta, 0.0);
        assert!(pinch.update(3.0).is_none());
        assert!(pinch.end(cancelled).is_none());
        pinch.begin(Some(4), 2).unwrap();
        let update = pinch.update(1.5).unwrap();
        assert_eq!(update.window, 4);
        assert!((update.delta.exp() - 1.5).abs() < 1e-12);
    }
}

#[test]
fn invalid_scales_do_not_poison_the_baseline() {
    let mut pinch = PinchState::default();
    pinch.begin(Some(1), 2).unwrap();
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, 1e-300] {
        assert!(pinch.update(scale).is_none());
    }
    assert!((pinch.update(2.0).unwrap().delta.exp() - 2.0).abs() < 1e-12);
}

#[test]
fn unsupported_fingers_or_surfaces_clear_the_active_gesture() {
    let mut pinch = PinchState::default();
    assert!(pinch.update(1.5).is_none());
    assert!(pinch.end(false).is_none());
    for (window, fingers) in [(Some(2), 1), (Some(2), 3), (None, 2)] {
        pinch.begin(Some(1), 2).unwrap();
        assert!(pinch.begin(window, fingers).is_none());
        assert!(pinch.update(1.5).is_none());
        assert!(pinch.end(false).is_none());
    }
}
