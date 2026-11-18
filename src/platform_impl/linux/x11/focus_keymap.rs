//! Repeat provenance from the event-ordered keymap following a core FocusIn.
//!
//! X11 sends KeymapNotify immediately after FocusIn for KeymapState subscribers.
//! Unlike a later QueryKeymap reply, that event still describes the focus boundary
//! when a repeat and its release are already queued by the time dispatch resumes.
//! See https://www.x.org/releases/X11R7.5/doc/libX11/libX11.html, section 10.8.

#[derive(Default)]
pub(super) struct FocusKeymap {
    pending: Option<u32>,
    window: Option<u32>,
    keys: [u8; 32],
}

impl FocusKeymap {
    pub fn focus_in(&mut self, window: u32) {
        self.pending = Some(window);
        self.window = None;
        self.keys = [0; 32];
    }

    pub fn focus_out(&mut self, window: u32) {
        if self.pending == Some(window) {
            self.pending = None;
        }
        if self.window == Some(window) {
            self.window = None;
            self.keys = [0; 32];
        }
    }

    pub fn cancel_pending(&mut self) {
        self.pending = None;
    }

    pub fn keymap_notify(&mut self, keys: [u8; 32]) -> Option<u32> {
        let window = self.pending.take()?;
        self.window = Some(window);
        self.keys = keys;
        Some(window)
    }

    pub fn key_event(&mut self, window: u32, keycode: u32, pressed: bool) -> bool {
        if self.window != Some(window) || !(8..=255).contains(&keycode) {
            return false;
        }
        let Some(byte) = self.keys.get_mut(keycode as usize / 8) else { return false };
        let mask = 1 << (keycode % 8);
        let was_held = *byte & mask != 0;
        if !pressed {
            *byte &= !mask;
        }
        pressed && was_held
    }

    pub fn key_event_from(
        &mut self,
        active: u32,
        mut source: u32,
        keycode: u32,
        pressed: bool,
        window_count: usize,
        mut parent: impl FnMut(u32) -> Option<u32>,
    ) -> Option<bool> {
        // A native child can receive keys while its ancestor keeps keyboard focus.
        // Bound the walk by tracked windows so a stale/cyclic relationship is inert.
        for _ in 0..window_count {
            if source == active {
                break;
            }
            source = parent(source)?;
        }
        if active != source {
            return None;
        }
        Some(self.key_event(active, keycode, pressed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u32 = 10;
    const B: u32 = 20;
    const N: u32 = 57;
    const Y: u32 = 29;

    fn down(key: u32) -> [u8; 32] {
        let mut keys = [0; 32];
        keys[key as usize / 8] |= 1 << (key % 8);
        keys
    }

    fn late_query_does_not_erase_the_focus_event_state(key: u32) {
        let mut state = FocusKeymap::default();
        state.focus_in(B);
        // Server queue: FocusIn, KeymapNotify(held), auto-repeat, physical release.
        // By the time client dispatch starts, a fresh QueryKeymap would report all keys up.
        let event_ordered_keys = down(key);
        let late_query_keys = [0; 32];
        assert_ne!(event_ordered_keys, late_query_keys);
        assert_eq!(state.keymap_notify(event_ordered_keys), Some(B));
        assert!(state.key_event(B, key, true), "the queued auto-repeat belongs to the old hold");
        assert!(!state.key_event(B, key, false));
        assert!(!state.key_event(B, key, true), "a later deliberate gesture stays fresh");
    }

    #[test]
    fn delayed_n_release_before_query_keeps_the_queued_repeat_identity() {
        late_query_does_not_erase_the_focus_event_state(N);
    }

    #[test]
    fn delayed_y_release_before_query_keeps_the_queued_repeat_identity() {
        late_query_does_not_erase_the_focus_event_state(Y);
    }

    #[test]
    fn fresh_press_after_an_unseen_release_is_not_a_repeat() {
        let mut state = FocusKeymap::default();
        state.focus_in(A);
        state.keymap_notify(down(N));
        state.focus_out(A);
        state.focus_in(B);
        state.keymap_notify([0; 32]);
        assert!(!state.key_event(B, N, true));
    }

    #[test]
    fn press_focus_out_focus_in_repeat_release_then_fresh_press() {
        for key in [N, Y] {
            for window in [A, B] {
                let mut state = FocusKeymap::default();
                state.focus_in(A);
                assert_eq!(state.keymap_notify([0; 32]), Some(A));
                assert!(!state.key_event(A, key, true));
                state.focus_out(A);
                state.focus_in(window);
                assert_eq!(state.keymap_notify(down(key)), Some(window));
                assert!(state.key_event(window, key, true), "the held key remains a repeat");
                assert!(!state.key_event(window, key, false));
                assert!(!state.key_event(window, key, true), "release ends the old hold");
            }
        }
    }

    #[test]
    fn press_focus_out_release_outside_then_return_has_no_stale_repeat() {
        for key in [N, Y] {
            let mut state = FocusKeymap::default();
            state.focus_in(A);
            assert_eq!(state.keymap_notify([0; 32]), Some(A));
            assert!(!state.key_event(A, key, true));
            state.focus_out(A);
            // The outside release is not delivered to A. Its next focus snapshot is up.
            state.focus_in(A);
            assert_eq!(state.keymap_notify([0; 32]), Some(A));
            assert!(!state.key_event(A, key, true));
        }
    }

    #[test]
    fn two_held_keys_keep_independent_repeat_and_release_state() {
        // Separate bytes and separate bits in one byte, releasing in either order.
        for keys in [[N, Y], [Y, N], [N, N + 1], [N + 1, N]] {
            let mut state = FocusKeymap::default();
            state.focus_in(A);
            assert_eq!(state.keymap_notify([0; 32]), Some(A));
            for key in keys {
                assert!(!state.key_event(A, key, true));
            }
            state.focus_out(A);
            state.focus_in(B);
            let mut held = down(keys[0]);
            held[keys[1] as usize / 8] |= 1 << (keys[1] % 8);
            assert_eq!(state.keymap_notify(held), Some(B));
            assert!(state.key_event(B, keys[0], true));
            assert!(state.key_event(B, keys[1], true));
            assert!(!state.key_event(B, keys[0], false));
            assert!(!state.key_event(B, keys[0], true));
            assert!(state.key_event(B, keys[1], true), "releasing one key keeps the other held");
            assert!(!state.key_event(B, keys[1], false));
            assert!(!state.key_event(B, keys[1], true));
        }
    }

    #[test]
    fn release_without_any_repeat_allows_the_next_fresh_press() {
        for key in [N, Y] {
            let mut state = FocusKeymap::default();
            state.focus_in(B);
            state.keymap_notify(down(key));
            assert!(!state.key_event(B, key, false));
            assert!(!state.key_event(B, key, true));
        }
    }

    #[test]
    fn unrelated_window_and_pointer_keymaps_do_not_retarget_provenance() {
        let mut state = FocusKeymap::default();
        assert_eq!(state.keymap_notify(down(N)), None, "pointer-enter keymaps are not focus state");
        state.focus_in(A);
        assert_eq!(state.keymap_notify(down(N)), Some(A));
        assert_eq!(state.keymap_notify([0; 32]), None);
        assert!(!state.key_event(B, N, true));
        assert!(!state.key_event(B, N, false));
        state.focus_out(B);
        assert!(state.key_event(A, N, true));
        state.focus_out(A);
        assert!(!state.key_event(A, N, true));
    }

    #[test]
    fn losing_focus_before_the_snapshot_cancels_it() {
        let mut state = FocusKeymap::default();
        state.focus_in(A);
        state.focus_out(A);
        assert_eq!(state.keymap_notify(down(N)), None);
        assert!(!state.key_event(A, N, true));
    }

    #[test]
    fn an_untracked_focus_event_cancels_pending_snapshot_authority() {
        let mut state = FocusKeymap::default();
        state.focus_in(A);
        state.cancel_pending();
        assert_eq!(state.keymap_notify(down(N)), None);
        assert!(!state.key_event(A, N, true));
    }

    #[test]
    fn pointer_entry_cancels_stale_pending_but_does_not_erase_an_owned_hold() {
        let mut state = FocusKeymap::default();
        state.focus_in(A);
        state.cancel_pending();
        assert_eq!(state.keymap_notify(down(Y)), None);

        state.focus_in(A);
        assert_eq!(state.keymap_notify(down(N)), Some(A));
        state.cancel_pending();
        assert_eq!(state.keymap_notify([0; 32]), None);
        assert!(state.key_event(A, N, true));
    }

    #[test]
    fn an_unrelated_windows_release_cannot_end_the_active_windows_hold() {
        let mut state = FocusKeymap::default();
        state.focus_in(A);
        state.keymap_notify(down(N));
        assert_eq!(state.key_event_from(A, B, N, false, 0, |_| None), None);
        assert_eq!(state.key_event_from(A, A, N, true, 0, |_| None), Some(true));
    }

    #[test]
    fn a_delayed_old_windows_key_cannot_become_a_fresh_press_in_the_new_window() {
        let mut state = FocusKeymap::default();
        state.focus_in(A);
        state.keymap_notify(down(N));
        state.focus_out(A);
        state.focus_in(B);
        state.keymap_notify([0; 32]);
        assert_eq!(state.key_event_from(B, A, N, true, 0, |_| None), None);
        assert_eq!(state.key_event_from(B, A, N, false, 0, |_| None), None);
        assert_eq!(state.key_event_from(B, B, N, true, 0, |_| None), Some(false));
    }

    #[test]
    fn a_childs_keys_are_delivered_to_its_focused_parent() {
        const CHILD: u32 = 30;
        const GRANDCHILD: u32 = 40;
        let parent = |window| match window {
            CHILD => Some(A),
            GRANDCHILD => Some(CHILD),
            _ => None,
        };
        for source in [CHILD, GRANDCHILD] {
            for key in [N, Y] {
                let mut state = FocusKeymap::default();
                state.focus_in(A);
                state.keymap_notify(down(key));
                // The native child selects KeyPress, while XI2 focus remains on A.
                assert_eq!(state.key_event_from(A, source, key, true, 3, parent), Some(true));
                // An unrelated release still cannot clear the parent's held provenance.
                assert_eq!(state.key_event_from(A, B, key, false, 3, parent), None);
                assert_eq!(state.key_event_from(A, source, key, true, 3, parent), Some(true));
                assert_eq!(state.key_event_from(A, source, key, false, 3, parent), Some(false));
                assert_eq!(state.key_event_from(A, source, key, true, 3, parent), Some(false));
            }
        }
    }

    #[test]
    fn stale_parent_cycles_cannot_retarget_or_hang_key_delivery() {
        let mut state = FocusKeymap::default();
        state.focus_in(A);
        state.keymap_notify(down(N));
        assert_eq!(state.key_event_from(A, B, N, false, 2, |_| Some(B)), None);
        assert_eq!(state.key_event_from(A, A, N, true, 2, |_| None), Some(true));
    }

    #[test]
    fn out_of_range_keycodes_are_not_repeats() {
        let mut state = FocusKeymap::default();
        state.focus_in(A);
        state.keymap_notify([255; 32]);
        assert!(!state.key_event(A, u32::MAX, true));
    }
}
