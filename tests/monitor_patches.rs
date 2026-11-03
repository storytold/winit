//! Guards for the monitor fixes this branch carries on top of winit 0.30.13.
//!
//! Each test checks that a fix is still in the source and fails if a rebase drops it. When a winit
//! release carries an equivalent fix, drop the patch together with its test.

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

#[test]
fn wayland_lists_each_output_once() {
    // `WinitState::new` seeds the window's monitor list from every output bound at startup, and
    // SCTK then calls `new_output` once more for each of them, on its first complete `done`.
    // winit 0.30.13 pushed it again, so `Window::available_monitors()` listed one display twice,
    // and the stale copy outlived the output's removal (storytold/winit PR #22).
    let state = source("linux/wayland/state.rs");
    let new_output = section(&state, "fn new_output(", "fn update_output(");
    let guard = new_output
        .find("if !monitors.contains(&monitor)")
        .expect("new_output doesn't skip an output that is already listed");
    let push =
        new_output.find("monitors.push(monitor)").expect("new_output doesn't list the output");
    assert!(guard < push, "new_output pushes the output before checking whether it is listed");
}
