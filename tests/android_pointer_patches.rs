//! Guards for the Android pointer fixes this branch carries on top of winit 0.30.13.
//!
//! Each test checks that a fix is still in the source, and fails if a rebase drops it. When a
//! winit release carries an equivalent fix, drop the patch together with its test.

use std::path::Path;

fn source(path: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/platform_impl").join(path);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// The text after `start`, up to the first `end`.
fn section<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let (_, rest) = text.split_once(start).unwrap_or_else(|| panic!("`{start}` not found"));
    rest.split_once(end).map_or(rest, |(body, _)| body)
}

fn android() -> String {
    source("android/mod.rs")
}

#[test]
fn mouse_and_touchpad_gestures_leave_the_touch_path() {
    // winit 0.30.13 maps every Android motion event to `Touch`, leaving a `// TODO mouse events`
    // where the button and scroll actions belong, so a mouse reports no right-click, no wheel and
    // no hover (storytold/effectcraft#2359). The gesture is split by its source *before* the touch
    // mapping, and touchscreen and touch-dragged stylus gestures keep the old path.
    let src = android();
    let imports = section(&src, "use android_activity::input::{", "};");
    for name in ["Axis", "MotionEvent", "Source"] {
        assert!(imports.contains(name), "`{name}` is not imported");
    }
    let route = section(
        &src,
        "if matches!(motion_event.source(), Source::Mouse | Source::Touchpad)",
        "let phase = match motion_event.action()",
    );
    assert!(
        route.contains("self.dispatch_mouse_input(window_id, device_id, motion_event, callback)"),
        "mouse and touchpad gestures are not routed to `dispatch_mouse_input`"
    );
    assert!(src.contains("fn dispatch_mouse_input<F>"), "`dispatch_mouse_input` is missing");
    // The touch mapping stays intact for non-mouse pointers.
    assert!(src.contains("None // TODO mouse events"), "the touch path's mapping was rewritten");
}

#[test]
fn wheel_notches_report_line_deltas() {
    // A scroll gesture arrives as `MotionAction::Scroll` carrying AXIS_HSCROLL/AXIS_VSCROLL, which
    // the touch mapping drops. Android's vertical axis matches winit's convention; its horizontal
    // one is the other way round, so it must be negated or scrolling right moves content right.
    let src = android();
    let scroll = section(&src, "MotionAction::Scroll => {", "dispatch(WindowEvent::MouseWheel");
    assert!(scroll.contains("Axis::Hscroll"), "the horizontal scroll axis is not read");
    assert!(scroll.contains("Axis::Vscroll"), "the vertical scroll axis is not read");
    assert!(scroll.contains("-p.axis_value(Axis::Hscroll)"), "the horizontal axis is not negated");
    assert!(scroll.contains("MouseScrollDelta::LineDelta(h, v)"), "no line delta is built");
}

#[test]
fn the_api_33_button_symbol_is_never_referenced() {
    // `AMotionEvent_getActionButton` exists only from API 33, and merely referencing it stops the
    // library loading on older devices (`dlopen failed: cannot locate symbol`, seen as an Android
    // 12 tablet failing with `UnsatisfiedLinkError`). The changed button is inferred from the
    // button state instead, which every Android reports.
    let src = android();
    assert!(
        !src.contains("getActionButton("),
        "`AMotionEvent_getActionButton` is called and breaks API < 33"
    );
    assert!(src.contains("fn changed_button_in_state("), "the button-state inference is missing");
    assert!(
        src.contains("fn mouse_button_bit(button: event::MouseButton) -> u8"),
        "the button bitmask helper is missing"
    );
    assert!(src.contains("mouse_buttons: Cell<u8>,"), "the reported button state is not kept");
    assert!(
        src.contains("mouse_buttons: Cell::new(0),"),
        "the reported button state is not initialised"
    );
    let press = section(
        &src,
        "MotionAction::ButtonPress | MotionAction::ButtonRelease => {",
        "MotionAction::Down | MotionAction::Up => {",
    );
    assert!(
        press.contains("changed_button_in_state(self.mouse_buttons.get(), pressed, motion_event)"),
        "press and release actions do not consult the reported button state"
    );
}

#[test]
fn gesture_endings_release_every_reported_button() {
    // Android reports a gesture's *first* button as an ordinary `Down` carrying the button in the
    // event's state, and the matching `Up` has already cleared that state. A `Cancel` (home key,
    // notification shade) and losing the window end a gesture with no `Up` at all. Leaving a
    // button recorded down means a later press dispatches nothing, so a stuck right-button would
    // swallow clicks (storytold/effectcraft#2359).
    let src = android();
    let down = section(
        &src,
        "MotionAction::Down | MotionAction::Up => {",
        "MotionAction::Move | MotionAction::HoverMove => {",
    );
    assert!(
        down.contains("button_in_state(motion_event)"),
        "`Down` does not read the event's button state"
    );
    assert!(down.contains("for button in self.take_mouse_buttons()"), "`Up` releases nothing");
    let cancel = section(&src, "MotionAction::Cancel => {", "InputStatus::Handled");
    assert!(cancel.contains("take_mouse_buttons"), "`Cancel` leaves buttons reported down");
    let focus = section(&src, "event::WindowEvent::Focused(false)", "InputStatus");
    assert!(focus.contains("take_mouse_buttons"), "losing the window leaves buttons reported down");
    assert!(src.contains("fn take_mouse_buttons(&self)"), "the button set is not readable");
    assert!(src.contains("fn set_mouse_button(&self"), "the button state is not updated");
}

#[test]
fn hover_moves_report_cursor_motion() {
    // An unpressed mouse move arrives as `HoverMove`/`HoverEnter`/`HoverExit`, which the touch
    // mapping drops: without them egui never updates hover, so menus and scroll bars never react
    // until a button is pressed.
    let src = android();
    let hover = section(
        &src,
        "MotionAction::Move | MotionAction::HoverMove => {",
        "MotionAction::HoverEnter => {",
    );
    assert!(hover.contains("dispatch(WindowEvent::CursorMoved"), "moves report no position");
    let enter = section(&src, "MotionAction::HoverEnter => {", "MotionAction::HoverExit => {");
    assert!(enter.contains("CursorEntered"), "entering the window is not reported");
    assert!(
        src.contains("MotionAction::HoverExit => dispatch(WindowEvent::CursorLeft"),
        "leaving the window is not reported"
    );
}
