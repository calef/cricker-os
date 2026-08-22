//! **From a keyboard's scancodes to the bytes a terminal receives** (milestone 29's input).
//!
//! A virtio-input device does not send characters. It sends `virtio_input_event` records carrying
//! **Linux evdev codes**: a physical key going down or coming up, in a numbering that has nothing to
//! do with what is printed on the keycap. Turning those into the wire bytes a terminal understands is
//! a table plus one bit of state, which makes it exactly the kind of thing that belongs on the host
//! (DECISIONS §7) rather than in a driver where a wrong row costs a QEMU boot to find.
//!
//! It lives in this crate rather than in `line_editor` because it is the *display* terminal's input
//! half, and because the driver that uses it (`user/src/kbd.rs`) already depends on nothing else
//! here. The bytes it produces are the terminal contract's driver half either way
//! (notes/terminal-contract.md, `OP_BYTES`), so a keystroke from this device and a keystroke from the
//! UART are the same thing by the time anything downstream sees it.
//!
//! # What it covers, and the limits that are real
//!
//! A **US layout's main block**: the alphanumerics, the symbol keys, space, enter, tab, backspace,
//! and escape, shifted and unshifted. That is what a terminal needs to be usable and it is where the
//! honest line is: no keypad, no function keys, no arrow keys (a terminal sends escape sequences for
//! those, and this device's codes for them are the *other* half of a mapping `line_editor` already has
//! on the receiving side), no compose, no dead keys, no other layout, and no key repeat beyond what
//! the device itself sends. Recorded in notes/glyphs.md rather than half-built.

/// `EV_SYN`: an event-batch separator. Carries no key and is ignored here.
pub const EV_SYN: u16 = 0;
/// `EV_KEY`: a key changed state. The only event type this cares about.
pub const EV_KEY: u16 = 1;

/// `KEY_LEFTSHIFT`.
pub const KEY_LEFTSHIFT: u16 = 42;
/// `KEY_RIGHTSHIFT`.
pub const KEY_RIGHTSHIFT: u16 = 54;

/// The highest evdev code this table knows. Above it, [`byte`] answers `None`.
pub const MAX_CODE: u16 = 57;

/// What each key sends with no modifier, indexed by evdev code. `0` means "this key produces no
/// byte", which covers both the unmapped codes and the modifiers themselves.
///
/// Written as one flat table rather than a match, because that is what it is: a keyboard layout is
/// data, and the failure mode of a 58-arm match is a row silently missing.
static UNSHIFTED: [u8; MAX_CODE as usize + 1] =
    *b"\0\x1b1234567890-=\x08\tqwertyuiop[]\r\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";

/// What each key sends with shift held. Same indexing as [`UNSHIFTED`].
static SHIFTED: [u8; MAX_CODE as usize + 1] =
    *b"\0\x1b!@#$%^&*()_+\x08\tQWERTYUIOP{}\r\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";

/// Is this key a shift?
pub const fn is_shift(code: u16) -> bool {
    code == KEY_LEFTSHIFT || code == KEY_RIGHTSHIFT
}

/// **The byte evdev key `code` sends**, or `None` if this layout has nothing for it.
///
/// `None` rather than a placeholder byte: a key with no mapping must produce *nothing*, because a
/// terminal that inserted a substitute character for every unmapped function key would corrupt every
/// line the user typed while reaching for one.
pub fn byte(code: u16, shift: bool) -> Option<u8> {
    let table = if shift { &SHIFTED } else { &UNSHIFTED };
    match table.get(code as usize) {
        Some(&0) | None => None,
        Some(&b) => Some(b),
    }
}

/// **A keyboard's state**: which shift keys are down, and nothing else.
///
/// One bit, and that is the whole reason this is a struct rather than a free function: shift is
/// *held*, so it arrives as its own press and release events and the driver has to remember it
/// between them. Everything else about a key is in the event.
#[derive(Debug, Clone, Copy, Default)]
pub struct Keyboard {
    left: bool,
    right: bool,
}

impl Keyboard {
    /// Neither shift key held.
    pub const fn new() -> Keyboard {
        Keyboard {
            left: false,
            right: false,
        }
    }

    /// Is a shift key held?
    pub const fn shift(&self) -> bool {
        self.left || self.right
    }

    /// **Feed one `virtio_input_event`**, and get back the byte it produces, if any.
    ///
    /// `value` is 0 for a release, 1 for a press, and 2 for the device's own auto-repeat. **Repeats
    /// count as presses**, which is what makes holding a key type: a driver that only honoured
    /// `value == 1` would look correct in every test and be maddening to use.
    pub fn event(&mut self, kind: u16, code: u16, value: u32) -> Option<u8> {
        if kind != EV_KEY {
            return None;
        }
        if is_shift(code) {
            let down = value != 0;
            if code == KEY_LEFTSHIFT {
                self.left = down;
            } else {
                self.right = down;
            }
            return None;
        }
        if value == 0 {
            return None; // a release types nothing
        }
        byte(code, self.shift())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// The letters, the digits, and the symbols land where the keycaps say, shifted and unshifted.
    /// A table off by one row would put `s` on the `a` key and nothing else would notice.
    #[test]
    fn the_keycaps_say_what_the_table_says() {
        for (code, want, want_shift) in [
            (30u16, b'a', b'A'), // KEY_A: the first row's left-hand home key
            (16, b'q', b'Q'),    // KEY_Q
            (44, b'z', b'Z'),    // KEY_Z
            (2, b'1', b'!'),     // KEY_1
            (11, b'0', b')'),    // KEY_0
            (12, b'-', b'_'),    // KEY_MINUS
            (39, b';', b':'),    // KEY_SEMICOLON
            (53, b'/', b'?'),    // KEY_SLASH
            (57, b' ', b' '),    // KEY_SPACE
            (28, b'\r', b'\r'),  // KEY_ENTER
            (15, b'\t', b'\t'),  // KEY_TAB
            (14, 0x08, 0x08),    // KEY_BACKSPACE
            (1, 0x1b, 0x1b),     // KEY_ESC
        ] {
            assert_eq!(byte(code, false), Some(want), "code {code} unshifted");
            assert_eq!(byte(code, true), Some(want_shift), "code {code} shifted");
        }
    }

    /// **Enter sends CR, not LF.** The line discipline treats either as end-of-line, but a real
    /// terminal sends CR and the VT engine's `\r\n` handling is written for that; a keyboard that
    /// sent LF would move the cursor down a row without returning it, on a screen, visibly.
    #[test]
    fn enter_sends_carriage_return() {
        assert_eq!(byte(28, false), Some(b'\r'));
        assert_ne!(byte(28, false), Some(b'\n'));
    }

    /// Nothing has a mapping it should not: the modifiers type nothing, and a code past the table is
    /// `None` rather than an index panic or a stray byte.
    #[test]
    fn unmapped_keys_type_nothing() {
        assert_eq!(byte(KEY_LEFTSHIFT, false), None);
        assert_eq!(byte(KEY_RIGHTSHIFT, false), None);
        assert_eq!(
            byte(29, false),
            None,
            "left control types nothing on its own"
        );
        assert_eq!(byte(56, false), None, "left alt likewise");
        assert_eq!(byte(0, false), None, "KEY_RESERVED");
        assert_eq!(byte(MAX_CODE + 1, false), None, "past the table");
        assert_eq!(byte(59, false), None, "F1");
        assert_eq!(byte(u16::MAX, true), None, "far past the table");
    }

    /// No two keys send the same byte, so a wrong keycode is a wrong character rather than a
    /// coincidence. (`*` is the exception: the keypad asterisk and shift-8 are the same character on
    /// a real keyboard too, which is why it is named here instead of quietly allowed.)
    #[test]
    fn two_keys_never_send_the_same_byte() {
        for shift in [false, true] {
            let mut seen: Vec<(u16, u8)> = Vec::new();
            for code in 0..=MAX_CODE {
                let Some(b) = byte(code, shift) else { continue };
                if let Some((other, _)) = seen.iter().find(|(_, o)| *o == b) {
                    assert!(
                        b == b'*',
                        "codes {other} and {code} both send {:?}: the table is shifted",
                        b as char,
                    );
                }
                seen.push((code, b));
            }
            assert!(seen.len() > 45, "the table lost most of its keys");
        }
    }

    /// **Shift is held, so it is state.** Press shift, type, release shift, type: the same key sends
    /// two different bytes, and the shift key itself sends none.
    #[test]
    fn shift_is_remembered_between_events() {
        let mut kb = Keyboard::new();
        assert_eq!(kb.event(EV_KEY, 30, 1), Some(b'a'));
        assert_eq!(kb.event(EV_KEY, KEY_LEFTSHIFT, 1), None);
        assert!(kb.shift());
        assert_eq!(kb.event(EV_KEY, 30, 1), Some(b'A'));
        assert_eq!(kb.event(EV_KEY, KEY_LEFTSHIFT, 0), None);
        assert!(!kb.shift());
        assert_eq!(kb.event(EV_KEY, 30, 1), Some(b'a'));

        // Two shifts, released independently: releasing one while the other is held stays shifted.
        kb.event(EV_KEY, KEY_LEFTSHIFT, 1);
        kb.event(EV_KEY, KEY_RIGHTSHIFT, 1);
        kb.event(EV_KEY, KEY_LEFTSHIFT, 0);
        assert_eq!(
            kb.event(EV_KEY, 30, 1),
            Some(b'A'),
            "right shift is still down"
        );
        kb.event(EV_KEY, KEY_RIGHTSHIFT, 0);
        assert_eq!(kb.event(EV_KEY, 30, 1), Some(b'a'));

        // Which hand is down is only ever read as the OR of the two, so a keyboard that filed the
        // left key under the right one types identically and debugs as the wrong hand. The state
        // dump is the only witness, and a driver being debugged is what it is for.
        kb.event(EV_KEY, KEY_LEFTSHIFT, 1);
        assert!(
            std::format!("{kb:?}").contains("left: true, right: false"),
            "the left shift key must be recorded as the left one",
        );
    }

    /// A **release types nothing**, an auto-repeat types the same as a press, and a non-key event is
    /// ignored. The release rule is the one that matters: without it every keystroke would arrive
    /// twice, which is the first bug an evdev driver has.
    #[test]
    fn releases_type_nothing_and_repeats_type_again() {
        let mut kb = Keyboard::new();
        assert_eq!(kb.event(EV_KEY, 30, 1), Some(b'a'), "press");
        assert_eq!(kb.event(EV_KEY, 30, 0), None, "release");
        assert_eq!(kb.event(EV_KEY, 30, 2), Some(b'a'), "auto-repeat");
        assert_eq!(kb.event(EV_SYN, 0, 0), None, "a batch separator");
        assert_eq!(kb.event(7, 30, 1), None, "some other event type");
    }

    /// A whole word typed the way the device sends it: press and release per key, shift for the
    /// capital. The byte stream is what a terminal would have received from a UART.
    #[test]
    fn typing_a_word_produces_the_bytes_a_terminal_would_have_seen() {
        let mut kb = Keyboard::new();
        let mut out = Vec::new();
        // Shift+h, e, l, l, o, Enter.
        for (code, value) in [
            (KEY_LEFTSHIFT, 1),
            (35, 1),
            (35, 0),
            (KEY_LEFTSHIFT, 0),
            (18, 1),
            (18, 0),
            (38, 1),
            (38, 0),
            (38, 1),
            (38, 0),
            (24, 1),
            (24, 0),
            (28, 1),
            (28, 0),
        ] {
            if let Some(b) = kb.event(EV_KEY, code, value) {
                out.push(b);
            }
        }
        assert_eq!(out, b"Hello\r");
    }
}
