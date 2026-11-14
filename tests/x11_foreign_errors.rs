//! Window setters on X11 must not panic on an X error that an earlier request left behind.
//!
//! Xlib reports errors asynchronously through winit's error handler, which keeps the latest one.
//! The display connection is shared with other libraries (Mesa's Vulkan and GLX drivers), so the
//! error a setter finds after flushing its own requests usually came from someone else's. A
//! setter used to panic on it while holding the window's state lock, and the window's `Drop`
//! then aborted the process on the poisoned lock (storytold/deckcraft#2086, a DRI3
//! `BadDrawable`; rust-windowing/winit#4615, a GLX `BadIDChoice`).
//!
//! Needs an X server: skipped when `DISPLAY` is unset.
#![cfg(all(
    feature = "x11",
    feature = "rwh_06",
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android", target_os = "redox"))
))]

use rwh_06::{HasDisplayHandle, RawDisplayHandle};
use winit::event_loop::EventLoop;
use winit::platform::x11::EventLoopBuilderExtX11;
use winit::window::{Theme, Window, WindowLevel};

#[test]
fn window_setters_survive_an_error_from_another_request() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("skipped: DISPLAY is unset");
        return;
    }
    let mut builder = EventLoop::builder();
    builder.with_x11().with_any_thread(true);
    let event_loop = builder.build().expect("X11 event loop");
    #[allow(deprecated)]
    let window =
        event_loop.create_window(Window::default_attributes().with_visible(false)).expect("window");
    let RawDisplayHandle::Xlib(handle) = window.display_handle().expect("display handle").as_raw()
    else {
        panic!("not an Xlib display");
    };
    let display = handle.display.expect("Xlib display").as_ptr().cast();
    let xlib = x11_dl::xlib::Xlib::open().expect("libX11");
    // A request winit didn't make fails on the shared connection: freeing a pixmap that doesn't
    // exist gives `BadPixmap`, which the sync hands to winit's error handler.
    let leave_an_error = || unsafe {
        (xlib.XFreePixmap)(display, 1);
        (xlib.XSync)(display, x11_dl::xlib::False);
    };

    leave_an_error();
    window.set_visible(true);
    leave_an_error();
    window.set_title("foreign X error");
    leave_an_error();
    let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(320, 240));
    leave_an_error();
    window.set_decorations(false);
    leave_an_error();
    window.set_window_level(WindowLevel::AlwaysOnTop);
    leave_an_error();
    window.set_window_icon(None);
    leave_an_error();
    window.set_theme(Some(Theme::Dark));
    leave_an_error();
    window.set_maximized(true);
    leave_an_error();
    window.set_minimized(true);
    leave_an_error();
    window.set_visible(false);
    // Dropping the window takes the state lock again; a poisoned lock made it abort.
    drop(window);
}
