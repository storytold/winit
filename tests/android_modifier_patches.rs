//! Guards for the Android modifier fix this branch carries on top of winit 0.30.13.
//!
//! Each test checks that the fix is still in the source, and fails if a rebase drops it. When a
//! winit release carries an equivalent fix, drop the patch together with its test.

use std::path::Path;

fn android() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/platform_impl/android/mod.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// The text after `start`, up to the first `end`.
fn section<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let (_, rest) = text.split_once(start).unwrap_or_else(|| panic!("`{start}` not found"));
    rest.split_once(end).map_or(rest, |(body, _)| body)
}

#[test]
fn key_events_report_the_modifier_state() {
    // winit 0.30 moved modifiers out of `KeyEvent` into `WindowEvent::ModifiersChanged`, and the
    // Android backend never sent it: egui kept an empty modifier state, so Ctrl+C never copied,
    // and a chord key whose printable key reached the app replaced the selection instead of
    // shortcutting it (storytold/effectcraft#2359). The state is read from the event and kept.
    let src = android();
    let imports = section(&src, "use android_activity::input::{", "};");
    assert!(imports.contains("MetaState"), "`MetaState` is not imported");
    assert!(
        src.contains("use crate::keyboard::ModifiersState;"),
        "`ModifiersState` is not imported"
    );
    let helper = section(&src, "fn android_modifiers(meta: MetaState) -> ModifiersState {", "\n}");
    for bit in ["SHIFT", "CONTROL", "ALT", "SUPER"] {
        assert!(helper.contains(bit), "`ModifiersState::{bit}` is not folded in");
    }
    assert!(src.contains("modifiers: ModifiersState,"), "the state is not kept on the loop");
    assert!(src.contains("modifiers: ModifiersState::empty(),"), "the state is not initialised");
}

#[test]
fn the_change_is_dispatched_before_the_key_it_belongs_to() {
    // Consumers stamp a key event with the modifier state they hold at conversion time, so a
    // change dispatched after the key is too late for that key. Android also sends the modifier
    // keys themselves as key events, which is what makes the state change observable here.
    let src = android();
    let arm = section(&src, "keycode => {", "let state = match key.action()");
    assert!(
        arm.contains("event::WindowEvent::ModifiersChanged(modifiers.into())"),
        "the modifier change is not dispatched before the key event"
    );
}

#[test]
fn an_idle_device_reports_no_change() {
    // Dispatching the state on every frame would keep egui's input marked active forever, so a
    // paused Android app never idles and repaints continuously. Only a change is dispatched.
    let src = android();
    let arm = section(&src, "keycode => {", "let state = match key.action()");
    assert!(
        arm.contains("if modifiers != self.modifiers"),
        "the change is dispatched even when nothing changed"
    );
    assert!(arm.contains("self.modifiers = modifiers"), "dispatched changes are not recorded");
    // Volume keys are swallowed before any dispatch, so they cannot report a change either.
    let volume = section(
        &src,
        "Keycode::VolumeUp | Keycode::VolumeDown | Keycode::VolumeMute",
        "keycode => {",
    );
    assert!(
        !volume.contains("ModifiersChanged"),
        "a swallowed volume key reports a modifier change"
    );
}
