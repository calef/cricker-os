//! **What the milestone-29 tests put on the screen**, in one place so every party agrees.
//!
//! Text on a screen can only be *proved* if the picture is a value more than one independent
//! witness can compute. These constants are that value's input. The kernel-side test sends these
//! bytes to the terminal and then predicts the framebuffer by running the engine itself; the
//! host-side scanout check runs the same engine over the same bytes and grades what QEMU is
//! actually displaying. Neither reads the other's answer, and the terminal component that draws
//! holds none of this: it renders whatever arrives.
//!
//! This is the same move `gfx_proto::pixel` makes for rung one's test pattern and `compositor::SCENE`
//! for rung two's scene: the test's content lives in the contract crate, not in the test.

/// The columns of a terminal that owns the whole scanout: `gfx_proto::WIDTH / bitfont::GLYPH_W`.
///
/// Spelled as a literal here rather than computed, because this crate deliberately does not depend
/// on the framebuffer contract (a VT engine that knew about a scanout would be the wrong shape).
/// `user/src/display_terminal.rs` and the kernel wiring both assert the two agree at **compile time**, so a
/// screen that changed size is a build error rather than a terminal quietly missing its last column.
///
/// **Eighteen, and the division has a remainder**: 128 / 7 is 18 with two pixels left over, which
/// the terminal paints as background once and no cell ever owns. It was 16 while the font was 8
/// wide.
pub const COLS: u32 = 18;
/// The rows of a terminal that owns the whole scanout. See [`COLS`].
pub const ROWS: u32 = 8;

/// **What the application prints.** Delivered as `OP_WRITE`, the terminal contract's application
/// half (notes/terminal-contract.md), exactly as a program printing to a serial console would.
///
/// Chosen so the picture is hard to produce by accident:
///
/// - **four rows of text**, so a stride error or a one-row shift is visible (one row would not be);
/// - **three renditions**: default, a green foreground, and a reversed block. A terminal that
///   ignored SGR would draw every glyph correctly and still fail;
/// - **a `\r\n` pair**, which is what `line_editor::expand_output` puts on the wire for a Unix `\n`, so
///   this is the byte stream a real program's output actually becomes;
/// - **descenders and an underscore** (`y`, `_`), which are the glyph rows a font table truncated to
///   seven rows would lose.
pub const GREETING: &[u8] = b"nife\r\n\x1b[32mglyphs_ok\x1b[0m\r\nby a vt\r\n\x1b[7mFOCUS\x1b[0m";

/// **What the user types.** Delivered as `OP_BYTES`, the terminal contract's driver half, which is
/// the same framing the compositor uses to route a keystroke to the focused client (DECISIONS §33).
/// Echoed into the grid, so a keystroke that never arrived is a missing letter on the screen rather
/// than a silent nothing.
pub const TYPED: &[u8] = b"\r\n> hi";

/// **A wrong picture, for the negative control.** The greeting with one letter changed.
///
/// The host-side scanout check has to reject this, and that is the assertion that makes the whole
/// text proof mean something: a checker that only asked "is the screen not blank?" would pass every
/// run, including the ones where the terminal drew the wrong thing. One letter, because a wholly
/// different screen would be rejected by a much weaker check.
pub const GREETING_TYPO: &[u8] =
    b"nife\r\n\x1b[32mglyphs_0k\x1b[0m\r\nby a vt\r\n\x1b[7mFOCUS\x1b[0m";

/// **The banner each terminal in the compositor test prints**, indexed by window.
///
/// Different text per window on purpose: it is what turns "the screen has text on it" into "each
/// window has *its own* text on it", so an `OP_WRITE` delivered to the wrong terminal is a wrong
/// picture rather than a duplicate one.
pub const WINDOW_BANNER: [&[u8]; 2] = [b"term0", b"term1"];

/// **What is typed at each terminal in the compositor test** after focus moves to it.
///
/// The letters differ so that focus routing is checkable from the *picture*: an `A` appearing in
/// window 1, or a `B` in both, is a compositor that sent a keystroke to a client that should not
/// have received it. Since a client can only be sent input because it *holds* an input endpoint
/// (DECISIONS §33), this is the capability claim made visible in pixels.
pub const WINDOW_TYPED: [&[u8]; 2] = [b"\r\nA", b"\r\nB"];

/// **The key the host presses on the real keyboard device**, as QEMU's monitor `sendkey` names it.
///
/// The keyboard test's one genuinely host-driven input: nothing in the guest can press a key, so
/// `cargo xtask` sends this on the monitor beside the suite, the same connection the scanout check
/// already uses. Shared here so the side that presses and the side that asserts cannot disagree
/// about which key it was.
pub const HOST_KEY: &str = "a";

/// The byte [`HOST_KEY`] must arrive as, once the driver's evdev event has been through
/// [`crate::keymap`]. Unshifted, so this is the plain letter.
pub const HOST_KEY_BYTE: u8 = b'a';

/// The terminal a full-scanout wiring shows after the whole script: the greeting, then the typing.
///
/// Built rather than stored, because the *engine* is the definition. A stored bitmap would be a
/// second definition that could drift from the one the component runs.
pub fn full_screen() -> crate::Vt {
    let mut vt = crate::Vt::new(COLS, ROWS);
    vt.feed(GREETING);
    vt.feed(TYPED);
    vt
}

/// What window `i`'s terminal shows in the compositor test: its own banner, then what was typed at
/// it while it had focus.
pub fn window(i: usize, cols: u32, rows: u32) -> crate::Vt {
    let mut vt = crate::Vt::new(cols, rows);
    vt.feed(WINDOW_BANNER[i]);
    vt.feed(WINDOW_TYPED[i]);
    vt
}
