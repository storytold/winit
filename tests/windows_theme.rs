//! W44: a theme set with `Window::set_theme` survives a `WM_SETTINGCHANGE`. Windows sends one
//! whenever a system setting changes, and it used to put the system theme back on the window:
//! a dark app under a light system got a light title bar.

#[cfg(windows)]
#[test]
fn a_theme_set_on_the_window_survives_a_settings_change() {
    use rwh_06::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_SETTINGCHANGE};
    use winit::event_loop::EventLoop;
    use winit::platform::windows::EventLoopBuilderExtWindows;
    use winit::window::{Theme, Window};

    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    #[allow(deprecated)]
    let window =
        event_loop.create_window(Window::default_attributes().with_visible(false)).unwrap();
    let RawWindowHandle::Win32(handle) = window.window_handle().unwrap().as_raw() else {
        panic!("a Windows window has a Win32 handle");
    };
    let hwnd = handle.hwnd.get() as _;
    let area: Vec<u16> = "ImmersiveColorSet\0".encode_utf16().collect();
    // Both themes, so one of them is not the system's, whichever the system uses.
    for theme in [Theme::Dark, Theme::Light] {
        window.set_theme(Some(theme));
        unsafe { SendMessageW(hwnd, WM_SETTINGCHANGE, 0, area.as_ptr() as _) };
        assert_eq!(window.theme(), Some(theme), "the theme set, after a settings change");
    }
}
