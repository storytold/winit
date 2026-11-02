//! The text that `WinitView` shows input methods through `NSTextInputClient`.
//!
//! When the application publishes surrounding text (`WindowExtMacOS::set_ime_surrounding_text`),
//! `ImeDocument` exposes a coherent virtual UTF-16 buffer combining committed text before the
//! caret/selection, the active `marked_text` (or committed selection when no composition is
//! active), and committed text after the caret/selection. When no surrounding text is published,
//! it falls back to tracking the text inserted since the caret last moved (`typed`).
//!
//! Offsets are UTF-16 code units, as in `NSRange`. They keep growing across resets and external
//! jumps, so a range an input method computed before a reset never matches text inserted after it.

// `tests/ime_patches.rs` includes this file and provides its own `ImeDeleteSurrounding`
// and `crate::platform::macos::ImeTextChangeCause`.
use super::ImeDeleteSurrounding;
use crate::platform::macos::ImeTextChangeCause;

/// Text kept for replacements in unsynced mode, in characters.
const KEPT_CHARS: usize = 256;

/// Safety cap for synced prefix growth between frame syncs, in characters.
const SYNCED_MAX_BEFORE_CHARS: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SyncedSurrounding {
    before: String,
    selected: String,
    after: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ImeDocument {
    /// The text inserted since the last reset when surrounding text is not synced;
    /// the caret is at its end.
    typed: String,
    /// Optional surrounding context synchronized from the application.
    synced: Option<SyncedSurrounding>,
    /// The UTF-16 offset of the start of the known text in the virtual document.
    start: usize,
}

impl ImeDocument {
    fn before_text(&self) -> &str {
        match &self.synced {
            Some(s) => &s.before,
            None => &self.typed,
        }
    }

    /// The caret's UTF-16 offset, which is also where marked text (or selection) starts.
    pub fn caret(&self) -> usize {
        self.start.saturating_add(utf16_len(self.before_text()))
    }

    /// Total end UTF-16 offset of the known virtual buffer given `marked_utf16_len`.
    pub fn virtual_end_utf16(&self, marked_utf16_len: usize) -> usize {
        let caret = self.caret();
        match &self.synced {
            Some(s) => {
                let middle =
                    if marked_utf16_len > 0 { marked_utf16_len } else { utf16_len(&s.selected) };
                caret.saturating_add(middle).saturating_add(utf16_len(&s.after))
            },
            None => caret.saturating_add(marked_utf16_len),
        }
    }

    /// Forget local unsynced text and advance the virtual UTF-16 origin so ranges computed before
    /// a click, command, or IME reset cannot match text after it. Synchronized surrounding text is
    /// retained at the new origin until updated or explicitly cleared via
    /// [`Self::clear_surrounding_text`].
    pub fn reset(&mut self) {
        self.start = self.virtual_end_utf16(0);
        self.typed.clear();
    }

    /// Synchronize surrounding committed text and UTF-8 byte selection offsets `(cursor, anchor)`.
    ///
    /// `cursor` and `anchor` are clamped safely to valid UTF-8 character boundaries in `text`.
    #[allow(dead_code)]
    pub fn set_surrounding_text(
        &mut self,
        text: &str,
        cursor: usize,
        anchor: usize,
        cause: ImeTextChangeCause,
    ) {
        self.set_surrounding_text_with_marked(text, cursor, anchor, cause, false);
    }

    /// Like [`Self::set_surrounding_text`], preserving offset continuity if `has_marked_text` is
    /// currently active during a native IME composition pass.
    pub fn set_surrounding_text_with_marked(
        &mut self,
        text: &str,
        cursor: usize,
        anchor: usize,
        cause: ImeTextChangeCause,
        has_marked_text: bool,
    ) {
        let c = clamp_char_boundary(text, cursor);
        let a = clamp_char_boundary(text, anchor);
        let sel_start = c.min(a);
        let sel_end = c.max(a);
        let before = &text[..sel_start];
        let selected = &text[sel_start..sel_end];
        let after = &text[sel_end..];

        if let Some(cur) = &self.synced {
            if cur.before == before && cur.selected == selected && cur.after == after {
                return;
            }
            if cause == ImeTextChangeCause::Other && !has_marked_text {
                self.start = self.virtual_end_utf16(0);
            } else if cur.before.ends_with(before) && cur.selected == selected && cur.after == after
            {
                let dropped_utf16 = utf16_len(&cur.before).saturating_sub(utf16_len(before));
                self.start = self.start.saturating_add(dropped_utf16);
            }
        } else if !self.typed.is_empty() {
            let matches_typed_tail = before == self.typed && selected.is_empty();
            if !matches_typed_tail && !has_marked_text {
                self.start = self.virtual_end_utf16(0);
            }
            self.typed.clear();
        }

        let mut synced = SyncedSurrounding {
            before: before.to_owned(),
            selected: selected.to_owned(),
            after: after.to_owned(),
        };
        trim_front_chars(&mut synced.before, &mut self.start, SYNCED_MAX_BEFORE_CHARS);
        self.synced = Some(synced);
    }

    /// Forget synced surrounding text and return to local typed-prefix mode.
    ///
    /// Returns `true` if synchronized surrounding text was present and cleared.
    pub fn clear_surrounding_text(&mut self) -> bool {
        if self.synced.is_some() {
            self.start = self.virtual_end_utf16(0);
            self.synced = None;
            self.typed.clear();
            true
        } else {
            false
        }
    }

    /// Text reached the app at the caret.
    pub fn insert(&mut self, text: &str) {
        match &mut self.synced {
            Some(s) => {
                s.selected.clear();
                s.before.push_str(text);
                trim_front_chars(&mut s.before, &mut self.start, SYNCED_MAX_BEFORE_CHARS);
            },
            None => {
                self.typed.push_str(text);
                trim_front_chars(&mut self.typed, &mut self.start, KEPT_CHARS);
            },
        }
    }

    /// The selection to report: the selection within marked text, or the committed caret/selection.
    pub fn selected_range(&self, marked_selection: Option<(usize, usize)>) -> (usize, usize) {
        let caret = self.caret();
        match marked_selection {
            Some((location, length)) => (caret.saturating_add(location), length),
            None => {
                let sel_len = self.synced.as_ref().map_or(0, |s| utf16_len(&s.selected));
                (caret, sel_len)
            },
        }
    }

    /// The known committed text in `range`, with the UTF-16 range it actually covers.
    #[allow(dead_code)]
    pub fn substring(&self, range: (usize, usize)) -> Option<(String, (usize, usize))> {
        self.substring_with_marked(range, "")
    }

    /// The known text in `range` across `[before] + [marked (or selection)] + [after]`, with the
    /// UTF-16 range it actually covers.
    pub fn substring_with_marked(
        &self,
        (location, length): (usize, usize),
        marked: &str,
    ) -> Option<(String, (usize, usize))> {
        let before = self.before_text();
        let middle = if !marked.is_empty() {
            marked
        } else {
            self.synced.as_ref().map_or("", |s| s.selected.as_str())
        };
        let after = self.synced.as_ref().map_or("", |s| s.after.as_str());

        let doc_end = self
            .start
            .saturating_add(utf16_len(before))
            .saturating_add(utf16_len(middle))
            .saturating_add(utf16_len(after));
        let from_req = location.max(self.start);
        let to_req = location.saturating_add(length).min(doc_end);
        if from_req >= to_req {
            return None;
        }

        let mut seg_start = self.start;
        let mut out = String::new();
        let mut actual_from = None;

        for seg in [before, middle, after] {
            let seg_units = utf16_len(seg);
            let seg_end = seg_start.saturating_add(seg_units);
            if to_req > seg_start && from_req < seg_end {
                let local_from = from_req.saturating_sub(seg_start);
                let local_to = to_req.saturating_sub(seg_start).min(seg_units);
                let (from_byte, from_units) = byte_offset_floor(seg, local_from);
                let to_byte = byte_offset_ceil(seg, local_to);
                if from_byte < to_byte {
                    if actual_from.is_none() {
                        actual_from = Some(seg_start.saturating_add(from_units));
                    }
                    out.push_str(&seg[from_byte..to_byte]);
                }
            }
            seg_start = seg_end;
        }

        let actual_from = actual_from?;
        let actual_len = utf16_len(&out);
        Some((out, (actual_from, actual_len)))
    }

    /// Remove the committed text an input method replaces, and return the deletion the app has to
    /// make.
    #[allow(dead_code)]
    pub fn replace(&mut self, range: Option<(usize, usize)>) -> Option<ImeDeleteSurrounding> {
        self.replace_with_marked(range, 0)
    }

    /// Like [`Self::replace`], accounting for the active `marked_utf16_len` at `[caret .. caret +
    /// marked_utf16_len]` so replacing the marked text itself never deletes committed text after
    /// the caret.
    pub fn replace_with_marked(
        &mut self,
        range: Option<(usize, usize)>,
        marked_utf16_len: usize,
    ) -> Option<ImeDeleteSurrounding> {
        let (location, length) = range?;
        let slot_start = self.caret();
        let active_middle_utf16 = if marked_utf16_len > 0 {
            marked_utf16_len
        } else {
            self.synced.as_ref().map_or(0, |s| utf16_len(&s.selected))
        };
        let slot_end = slot_start.saturating_add(active_middle_utf16);
        let req_start = location;
        let req_end = location.saturating_add(length);

        if req_start < self.start || req_start > slot_end || req_end < slot_start {
            return None;
        }

        match &mut self.synced {
            Some(s) => {
                let before_bytes = if req_start < slot_start {
                    let local_from = req_start - self.start;
                    let (from_byte, _) = byte_offset_floor(&s.before, local_from);
                    let replaced = s.before.split_off(from_byte);
                    replaced.len()
                } else {
                    0
                };

                let after_bytes = if req_end > slot_end && !s.after.is_empty() {
                    let wanted_after_utf16 = req_end - slot_end;
                    let to_byte = byte_offset_ceil(&s.after, wanted_after_utf16);
                    s.after.drain(..to_byte);
                    to_byte
                } else {
                    0
                };

                if before_bytes == 0 && after_bytes == 0 {
                    return None;
                }
                s.selected.clear();
                Some(ImeDeleteSurrounding { before_bytes, after_bytes })
            },
            None => {
                if req_start >= slot_start {
                    return None;
                }
                let local_from = req_start - self.start;
                let (from_byte, _) = byte_offset_floor(&self.typed, local_from);
                let replaced = self.typed.split_off(from_byte);
                if replaced.is_empty() {
                    return None;
                }
                Some(ImeDeleteSurrounding { before_bytes: replaced.len(), after_bytes: 0 })
            },
        }
    }

    /// Clamp a requested UTF-16 `(location, length)` to the valid virtual document bounds for
    /// `firstRectForCharacterRange:actualRange:`.
    pub fn clamp_character_range(
        &self,
        range: Option<(usize, usize)>,
        marked_utf16_len: usize,
    ) -> (usize, usize) {
        let doc_start = self.start;
        let doc_end = self.virtual_end_utf16(marked_utf16_len);
        match range {
            None => (self.caret(), 0),
            Some((loc, len)) => {
                let clamped_loc = loc.clamp(doc_start, doc_end);
                let clamped_end = loc.saturating_add(len).clamp(clamped_loc, doc_end);
                (clamped_loc, clamped_end - clamped_loc)
            },
        }
    }
}

/// Estimate `(dx, min_width)` in view points for a sub-range `(loc, len)` inside `marked` when
/// `ime_position` is anchored at `marked_sel_loc` (the active cursor inside `marked`).
///
/// This keeps Hanja (`Opt+Return`) and multi-clause candidate popups aligned with the start of the
/// queried syllable/clause rather than drifting to the trailing edge of the composition.
pub(super) fn marked_subrange_rect_offset(
    marked: &str,
    caret: usize,
    marked_sel_loc: usize,
    (loc, len): (usize, usize),
    line_height: f64,
) -> (f64, f64) {
    let marked_len = utf16_len(marked);
    if marked_len == 0
        || line_height <= 0.0
        || !line_height.is_finite()
        || loc < caret
        || loc > caret.saturating_add(marked_len)
    {
        return (0.0, 0.0);
    }
    let req_start = (loc - caret).min(marked_len);
    let req_end = loc.saturating_add(len).saturating_sub(caret).min(marked_len);
    let sel_pos = marked_sel_loc.min(marked_len);

    let dx = if req_start < sel_pos {
        -utf16_span_em(marked, req_start, sel_pos) * line_height
    } else if req_start > sel_pos {
        utf16_span_em(marked, sel_pos, req_start) * line_height
    } else {
        0.0
    };
    let width = utf16_span_em(marked, req_start, req_end) * line_height;
    (dx, width)
}

fn utf16_span_em(text: &str, from_u16: usize, to_u16: usize) -> f64 {
    if from_u16 >= to_u16 {
        return 0.0;
    }
    let mut pos = 0;
    let mut em = 0.0;
    for c in text.chars() {
        let next = pos + c.len_utf16();
        if next > from_u16 && pos < to_u16 {
            em += if is_wide_cjk_or_hangul(c) { 0.8 } else { 0.45 };
        }
        pos = next;
        if pos >= to_u16 {
            break;
        }
    }
    em
}

fn is_wide_cjk_or_hangul(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x11FF
            | 0x2E80..=0x9FFF
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7FF
            | 0xF900..=0xFAFF
            | 0xFF01..=0xFF60
            | 0xFFE0..=0xFFE6
    )
}

fn trim_front_chars(buf: &mut String, start_utf16: &mut usize, max_chars: usize) {
    let excess = buf.chars().count().saturating_sub(max_chars);
    if excess > 0 {
        let cut = buf.char_indices().nth(excess).map_or(buf.len(), |(i, _)| i);
        *start_utf16 = start_utf16.saturating_add(utf16_len(&buf[..cut]));
        buf.drain(..cut);
    }
}

fn clamp_char_boundary(text: &str, offset: usize) -> usize {
    let mut index = offset.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Convert a relative UTF-16 offset into `(byte_offset, utf16_offset_at_char_start)`, moving back
/// to the start of a character when `utf16_offset` lands inside a surrogate pair.
fn byte_offset_floor(text: &str, utf16_offset: usize) -> (usize, usize) {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        let next = units + c.len_utf16();
        if next > utf16_offset {
            return (index, units);
        }
        units = next;
    }
    (text.len(), units)
}

/// Convert a relative UTF-16 offset into a byte offset, moving forward to the end of a character
/// when `utf16_offset` lands inside a surrogate pair.
fn byte_offset_ceil(text: &str, utf16_offset: usize) -> usize {
    if utf16_offset == 0 {
        return 0;
    }
    let mut units = 0;
    for (index, c) in text.char_indices() {
        units += c.len_utf16();
        if units >= utf16_offset {
            return index + c.len_utf8();
        }
    }
    text.len()
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Decode arbitrary UTF-16 code units lossily into valid UTF-8, replacing any unpaired surrogate
/// with `U+FFFD` instead of panicking or dereferencing a null `UTF8String` pointer.
pub(crate) fn utf16_to_lossy_utf8(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Decode marked UTF-16 `units` lossily while clamping `(raw_loc, raw_len)` (including
/// `NSNotFound` and `usize::MAX`) without overflow, snapping valid surrogate-pair interiors to
/// character boundaries, and returning `(marked_utf8, (snapped_loc_u16, snapped_len_u16),
/// cursor_utf8_range)` in a single pass.
pub(crate) fn utf16_marked_selection_and_utf8_range(
    units: &[u16],
    raw_loc: usize,
    raw_len: usize,
    not_found: usize,
) -> (String, (usize, usize), Option<(usize, usize)>) {
    if units.is_empty() {
        return (String::new(), (0, 0), None);
    }
    let len = units.len();
    let (loc, end) = if raw_loc >= not_found {
        (len, len)
    } else {
        let loc = raw_loc.min(len);
        let end = raw_loc.saturating_add(raw_len).min(len);
        (loc, end)
    };
    let is_pair_interior = |idx: usize| -> bool {
        idx > 0
            && idx < len
            && (0xd800..=0xdbff).contains(&units[idx - 1])
            && (0xdc00..=0xdfff).contains(&units[idx])
    };
    let snapped_loc = if is_pair_interior(loc) { loc - 1 } else { loc };
    let snapped_end = if end == loc {
        snapped_loc
    } else if is_pair_interior(end) {
        end + 1
    } else {
        end
    };

    let mut out = String::with_capacity(len);
    let mut lower_utf8 = None;
    let mut upper_utf8 = None;
    let mut i = 0;
    while i < len {
        if i == snapped_loc {
            lower_utf8 = Some(out.len());
        }
        if i == snapped_end {
            upper_utf8 = Some(out.len());
        }
        let u = units[i];
        let (ch, step) = if (0xd800..=0xdbff).contains(&u)
            && i + 1 < len
            && (0xdc00..=0xdfff).contains(&units[i + 1])
        {
            let hi = u32::from(u) - 0xd800;
            let lo = u32::from(units[i + 1]) - 0xdc00;
            let code = 0x10000 + ((hi << 10) | lo);
            (char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER), 2)
        } else if (0xd800..=0xdfff).contains(&u) {
            (char::REPLACEMENT_CHARACTER, 1)
        } else {
            (char::from_u32(u32::from(u)).unwrap_or(char::REPLACEMENT_CHARACTER), 1)
        };
        out.push(ch);
        i += step;
    }
    let lower = lower_utf8.unwrap_or(out.len());
    let upper = upper_utf8.unwrap_or(out.len()).max(lower);
    (out, (snapped_loc, snapped_end.saturating_sub(snapped_loc)), Some((lower, upper)))
}

/// What to do with text an input method inserts while there's no composition to commit.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PlainInsert {
    /// The key press being handled reports the text.
    KeyText,
    /// Commit the text: no `KeyboardInput` event carries it.
    Commit,
    /// Drop it.
    Ignore,
}

/// `key_characters` is `Some` while `keyDown:` handles a key, with the characters it types.
pub(super) fn plain_insert(text: &str, key_characters: Option<&str>) -> PlainInsert {
    if text.is_empty() || text.chars().next().is_some_and(char::is_control) {
        return PlainInsert::Ignore;
    }
    match key_characters {
        // An input method that types other text than the key, such as Chinese punctuation (`，`
        // for `,`) or an ideographic space for Space, without a composition.
        Some(characters) if characters != text => PlainInsert::Commit,
        Some(_) => PlainInsert::KeyText,
        // The emoji picker or dictation, outside any key press.
        None => PlainInsert::Commit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> ImeDocument {
        let mut document = ImeDocument::default();
        document.insert(text);
        document
    }

    /// The deletion of `text` before the caret.
    fn deleting(text: &str) -> Option<ImeDeleteSurrounding> {
        Some(ImeDeleteSurrounding { before_bytes: text.len(), after_bytes: 0 })
    }

    #[test]
    fn press_and_hold_replaces_the_letter_before_the_caret() {
        let mut document = typed("café");
        assert_eq!(document.selected_range(None), (4, 0));
        assert_eq!(document.substring((3, 1)), Some(("é".to_owned(), (3, 1))));
        assert_eq!(document.replace(Some((3, 1))), deleting("é"));
        document.insert("è");
        assert_eq!(document.substring((0, 10)), Some(("cafè".to_owned(), (0, 4))));
    }

    #[test]
    fn a_range_into_the_marked_text_replaces_up_to_the_caret() {
        let mut document = typed("e");
        assert_eq!(document.selected_range(Some((1, 0))), (2, 0));
        assert_eq!(document.replace(Some((0, 2))), deleting("e"));
        assert_eq!(document.caret(), 0);
    }

    #[test]
    fn ranges_the_document_cannot_answer_are_ignored() {
        let mut document = typed("abc");
        for range in [None, Some((3, 0)), Some((3, 2)), Some((0, 1)), Some((1, 1))] {
            assert_eq!(document.replace(range), None, "{range:?}");
        }
        document.reset();
        document.insert("x");
        // A range from before the reset.
        assert_eq!(document.replace(Some((2, 1))), None);
        assert_eq!(document.substring((0, 3)), None);
        assert_eq!(document.replace(Some((3, 1))), deleting("x"));
    }

    #[test]
    fn offsets_count_utf16_units() {
        let mut document = typed("a😀b");
        assert_eq!(document.caret(), 4);
        // A range starting inside the surrogate pair takes the whole character.
        assert_eq!(document.replace(Some((2, 2))), deleting("😀b"));
        assert_eq!(document.caret(), 1);
    }

    #[test]
    fn old_text_is_dropped_but_offsets_stay() {
        let mut document = typed(&"a".repeat(KEPT_CHARS));
        document.insert("bc");
        assert_eq!(document.caret(), KEPT_CHARS + 2);
        assert_eq!(document.substring((0, 2)), None);
        assert_eq!(document.replace(Some((KEPT_CHARS, 2))), deleting("bc"));
    }

    #[test]
    fn replacements_delete_exact_bytes_after_accents_flags_and_emoji_sequences() {
        // Toolkits delete a whole grapheme cluster per Backspace, so the deletion is an exact
        // byte count: the text before the replaced range must survive.
        let prefix = "e\u{301}é🇯🇵👨\u{200d}👩\u{200d}👧";
        let prefix_units = utf16_len(prefix);
        assert_eq!(prefix_units, 2 + 1 + 4 + 8);

        // Press-and-hold replaces the letter after the ZWJ sequence.
        let mut document = typed(&format!("{prefix}e"));
        let deletion = document.replace(Some((prefix_units, 1)));
        assert_eq!(deletion, Some(ImeDeleteSurrounding { before_bytes: 1, after_bytes: 0 }));
        assert_eq!(document.substring((0, 100)), Some((prefix.to_owned(), (0, prefix_units))));

        // Replacing the emoji sequence itself: 18 bytes, 8 UTF-16 units.
        let mut document = typed(prefix);
        let deletion = document.replace(Some((7, 8)));
        assert_eq!(deletion, Some(ImeDeleteSurrounding { before_bytes: 18, after_bytes: 0 }));
        assert_eq!(document.substring((0, 100)), Some(("e\u{301}é🇯🇵".to_owned(), (0, 7))));

        // One regional indicator of the flag, and the combining accent without its letter.
        let mut document = typed("a🇯🇵");
        assert_eq!(document.replace(Some((3, 2))), deleting("🇵"));
        assert_eq!(document.substring((0, 10)), Some(("a🇯".to_owned(), (0, 3))));
        let mut document = typed("e\u{301}");
        assert_eq!(document.replace(Some((1, 1))), deleting("\u{301}"));
        assert_eq!(document.substring((0, 10)), Some(("e".to_owned(), (0, 1))));
    }

    #[test]
    fn text_that_differs_from_the_key_is_committed() {
        assert_eq!(plain_insert("，", Some(",")), PlainInsert::Commit);
        assert_eq!(plain_insert("\u{3000}", Some(" ")), PlainInsert::Commit);
        assert_eq!(plain_insert("a", Some("a")), PlainInsert::KeyText);
        assert_eq!(plain_insert("😀", None), PlainInsert::Commit);
        assert_eq!(plain_insert("\r", Some("\r")), PlainInsert::Ignore);
        assert_eq!(plain_insert("", None), PlainInsert::Ignore);
    }

    #[test]
    fn cold_jamo_retry_snapshot_rollback_preserves_preceding_ascii() {
        // W-02: after typing ASCII "abc", a cold first Korean jamo "ㅎ" inserts into `ImeDocument`
        // on pass #1 via `PlainInsert::KeyText` before `ImeStartup::finish` triggers retry.
        // Restoring the snapshot ensures pass #2 composes "한" at caret 3 and a replacement of
        // "한" deletes only "한" (3 bytes), NEVER eating "abc"!
        let mut document = typed("abc");
        assert_eq!(document.caret(), 3);

        let snapshot = document.clone();
        // Pass #1 (cold fallback): inserts compatibility jamo "ㅎ" (3 UTF-8 bytes, 1 UTF-16 unit).
        document.insert("ㅎ");
        assert_eq!(document.caret(), 4);

        // Retry triggered: restore snapshot before pass #2.
        document = snapshot;
        assert_eq!(document.caret(), 3);
        assert_eq!(document.substring((0, 10)), Some(("abc".to_owned(), (0, 3))));

        // Pass #2 + subsequent jamo commits "한", then replaces "한".
        document.insert("한");
        assert_eq!(document.caret(), 4);
        assert_eq!(
            document.replace(Some((3, 1))),
            Some(ImeDeleteSurrounding { before_bytes: "한".len(), after_bytes: 0 })
        );
        assert_eq!(document.substring((0, 10)), Some(("abc".to_owned(), (0, 3))));
    }

    #[test]
    fn virtual_buffer_combines_synced_prefix_marked_text_and_suffix() {
        let mut doc = ImeDocument::default();
        let text = "안녕 세상";
        let cursor = "안녕 ".len();
        doc.set_surrounding_text(text, cursor, cursor, ImeTextChangeCause::Other);
        assert_eq!(doc.caret(), 3);
        assert_eq!(doc.selected_range(None), (3, 0));

        // While composing "한" at caret 3: virtual buffer is "안녕 " (0..3) + "한" (3..4) + "세상" (4..6).
        assert_eq!(doc.selected_range(Some((1, 0))), (4, 0));
        assert_eq!(doc.substring_with_marked((3, 1), "한"), Some(("한".to_owned(), (3, 1))));
        assert_eq!(doc.substring_with_marked((1, 4), "한"), Some(("녕 한세".to_owned(), (1, 4))));
        assert_eq!(
            doc.substring_with_marked((0, 100), "한"),
            Some(("안녕 한세상".to_owned(), (0, 6)))
        );

        // Replacing only the marked range `(3, 1)` must NOT delete committed suffix "세"!
        assert_eq!(doc.replace_with_marked(Some((3, 1)), 1), None);

        // Commit "한" + " ": synced buffer stays coherent before next frame sync.
        doc.insert("한");
        assert_eq!(doc.caret(), 4);
        assert_eq!(
            doc.substring_with_marked((0, 100), ""),
            Some(("안녕 한세상".to_owned(), (0, 6)))
        );
        doc.insert(" ");
        assert_eq!(doc.caret(), 5);
        assert_eq!(
            doc.substring_with_marked((0, 100), ""),
            Some(("안녕 한 세상".to_owned(), (0, 7)))
        );
    }

    #[test]
    fn surrounding_text_clamps_non_boundary_and_reversed_selection_and_replaces_both_sides() {
        let mut doc = ImeDocument::default();
        // "가나다라마": each Hangul syllable is 3 UTF-8 bytes (total 15 bytes).
        // Pass mid-character byte offsets (4 -> clamped to 3, 11 -> clamped to 9, reversed!).
        doc.set_surrounding_text("가나다라마", 11, 4, ImeTextChangeCause::Other);
        // Clamped selection is bytes 3..9 ("나다"), so before = "가" (1), selected = "나다" (2), after = "라마" (2).
        assert_eq!(doc.caret(), 1);
        assert_eq!(doc.selected_range(None), (1, 2));
        assert_eq!(doc.substring((0, 10)), Some(("가나다라마".to_owned(), (0, 5))));

        // Replace spanning "가" (before: 3 bytes), selection "나다" (excluded!), and "라" (after: 3 bytes):
        // Virtual UTF-16 range: `0..4` ("가" at 0..1, "나다" at 1..3, "라" at 3..4).
        assert_eq!(
            doc.replace_with_marked(Some((0, 4)), 0),
            Some(ImeDeleteSurrounding { before_bytes: 3, after_bytes: 3 })
        );
        doc.insert("하");
        assert_eq!(doc.substring((0, 10)), Some(("하마".to_owned(), (0, 2))));

        // Hostile / NSNotFound / overflowing ranges never panic and return None.
        assert_eq!(doc.replace_with_marked(Some((usize::MAX, 1)), 0), None);
        assert_eq!(doc.replace_with_marked(Some((isize::MAX as usize, usize::MAX)), 1), None);
        assert_eq!(doc.substring_with_marked((usize::MAX, usize::MAX), "한"), None);

        // Out-of-bounds cursor beyond `text.len()` clamps cleanly.
        doc.set_surrounding_text("한글", usize::MAX, usize::MAX, ImeTextChangeCause::Other);
        assert_eq!(doc.substring((0, 100)), Some(("한글".to_owned(), (2, 2))));
    }

    #[test]
    fn sliding_window_trim_and_clear_surrounding_text_keep_offsets_monotonic() {
        let mut doc = ImeDocument::default();
        doc.set_surrounding_text("가나다라마", 15, 15, ImeTextChangeCause::Other);
        assert_eq!(doc.caret(), 5);

        // Simulate 4000-byte sliding window trimming "가나" (2 UTF-16 units) from the front:
        // caret must stay at 5 so an in-flight markedRange `(5, 1)` does not shift!
        doc.set_surrounding_text_with_marked("다라마", 9, 9, ImeTextChangeCause::InputMethod, true);
        assert_eq!(doc.caret(), 5);
        assert_eq!(doc.substring_with_marked((2, 4), "한"), Some(("다라마한".to_owned(), (2, 4))));

        // Clearing surrounding text advances epoch past `5` and returns to local typed mode.
        assert!(doc.clear_surrounding_text());
        assert!(!doc.clear_surrounding_text());
        assert_eq!(doc.caret(), 5);
        assert_eq!(doc.substring((0, 10)), None);
        doc.insert("아");
        assert_eq!(doc.caret(), 6);
        assert_eq!(doc.substring((5, 1)), Some(("아".to_owned(), (5, 1))));
    }

    #[test]
    fn reset_fences_stale_ranges_while_retaining_synced_context() {
        let mut doc = ImeDocument::default();
        doc.set_surrounding_text("가나다", 3, 3, ImeTextChangeCause::Other);
        assert_eq!(doc.caret(), 1);

        // A no-op shortcut (`Cmd+S`, `Cmd+C`) or click that does not change the editor's snapshot
        // triggers `reset()`. Stale ranges before the reset are fenced (`< doc.start`), while new
        // queries at the new caret still see the synchronized context even if `egui-winit`
        // deduplicates the unchanged snapshot.
        doc.reset();
        assert_eq!(doc.replace(Some((0, 1))), None);
        assert_eq!(doc.caret(), 4);
        assert_eq!(doc.substring((3, 3)), Some(("가나다".to_owned(), (3, 3))));
    }

    #[test]
    fn marked_subrange_offset_anchors_hanja_popup_at_queried_syllable() {
        let mut doc = ImeDocument::default();
        doc.set_surrounding_text("안녕 ", 7, 7, ImeTextChangeCause::Other);
        assert_eq!(doc.clamp_character_range(Some((3, 1)), 1), (3, 1));
        assert_eq!(doc.clamp_character_range(Some((0, 99)), 1), (0, 4));
        assert_eq!(doc.clamp_character_range(None, 1), (3, 0));

        // Composing "한" at caret 3 with cursor at trailing edge `marked_sel_loc = 1`:
        // querying `(3, 1)` shifts left by one wide Hangul cell (`-0.8 * 20.0 = -16.0`) with width 16.0.
        let (dx, width) = marked_subrange_rect_offset("한", 3, 1, (3, 1), 20.0);
        assert!((dx - (-16.0)).abs() < 1e-6);
        assert!((width - 16.0).abs() < 1e-6);

        // Querying at the active cursor `(4, 0)` has zero X shift.
        let (dx_cursor, width_cursor) = marked_subrange_rect_offset("한", 3, 1, (4, 0), 20.0);
        assert_eq!(dx_cursor, 0.0);
        assert_eq!(width_cursor, 0.0);
    }

    #[test]
    fn external_other_edit_suffix_shrink_fences_stale_utf16_ranges() {
        // F-CHAL-1: external `Other` edit shrinking `"가한"` -> `"한"` without active marked text
        // must NOT be misclassified as a sliding-window prefix trim; stale UTF-16 range `(1, 1)`
        // from before the edit must be fenced and return `None`.
        let mut doc = ImeDocument::default();
        doc.set_surrounding_text("가한", "가한".len(), "가한".len(), ImeTextChangeCause::Other);
        assert_eq!(doc.caret(), 2);

        doc.set_surrounding_text("한", "한".len(), "한".len(), ImeTextChangeCause::Other);
        assert_eq!(doc.caret(), 3);
        assert_eq!(doc.replace_with_marked(Some((1, 1)), 0), None);
        assert_eq!(
            doc.replace_with_marked(Some((2, 1)), 0),
            Some(ImeDeleteSurrounding { before_bytes: "한".len(), after_bytes: 0 })
        );
    }

    #[test]
    fn utf16_helpers_handle_not_found_overflow_split_surrogates_and_unpaired_surrogates() {
        let not_found = isize::MAX as usize;

        // Empty marked units return `(String::new(), (0, 0), None)`.
        assert_eq!(
            utf16_marked_selection_and_utf8_range(&[], not_found, 1, not_found),
            (String::new(), (0, 0), None)
        );

        // NSNotFound (`isize::MAX as usize`) with `length = 1`, `(1, usize::MAX)`, and
        // `(usize::MAX, usize::MAX)` never overflow or underflow.
        let ko: Vec<u16> = "한글".encode_utf16().collect();
        assert_eq!(
            utf16_marked_selection_and_utf8_range(&ko, not_found, 1, not_found),
            ("한글".to_owned(), (2, 0), Some((6, 6)))
        );
        assert_eq!(
            utf16_marked_selection_and_utf8_range(&ko, 1, usize::MAX, not_found),
            ("한글".to_owned(), (1, 1), Some((3, 6)))
        );
        assert_eq!(
            utf16_marked_selection_and_utf8_range(&ko, usize::MAX, usize::MAX, not_found),
            ("한글".to_owned(), (2, 0), Some((6, 6)))
        );

        // Surrogate-interior `selectedRange = (1, 0)` and `(1, 1)` on `"😀한"` snap to valid
        // UTF-8 char boundaries (`"😀"` = 4 UTF-8 bytes, 2 UTF-16 units; `"한"` = 3 bytes, 1 unit).
        let emoji_ko: Vec<u16> = "😀한".encode_utf16().collect();
        let (s0, sel0, r0) = utf16_marked_selection_and_utf8_range(&emoji_ko, 1, 0, not_found);
        assert_eq!(s0, "😀한");
        assert_eq!(sel0, (0, 0));
        assert_eq!(r0, Some((0, 0)));
        assert!(s0.is_char_boundary(0));

        let (s1, sel1, r1) = utf16_marked_selection_and_utf8_range(&emoji_ko, 1, 1, not_found);
        assert_eq!(s1, "😀한");
        assert_eq!(sel1, (0, 2));
        assert_eq!(r1, Some((0, 4)));
        assert!(s1.is_char_boundary(0) && s1.is_char_boundary(4));

        // Unpaired high and low surrogates decode lossily to U+FFFD without panicking or UB.
        assert_eq!(utf16_to_lossy_utf8(&[0xd83d, 0xd55c]), "\u{FFFD}한");
        assert_eq!(utf16_to_lossy_utf8(&[0xdc00]), "\u{FFFD}");
        for raw_loc in 0..=3 {
            for raw_len in [0, 1, 2, usize::MAX] {
                for units in [&[0xd83d_u16, 0xd55c][..], &[0xdc00_u16][..], &emoji_ko[..]] {
                    let (decoded, (_, _), range) =
                        utf16_marked_selection_and_utf8_range(units, raw_loc, raw_len, not_found);
                    let (lo, hi) = range.expect("non-empty units produce Some range");
                    assert!(lo <= hi && hi <= decoded.len());
                    assert!(decoded.is_char_boundary(lo) && decoded.is_char_boundary(hi));
                }
            }
        }
    }
}
