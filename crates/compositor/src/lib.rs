//! **The compositor contract** (milestone 33, the display ladder's rung two).
//!
//! One screen, several mutually distrusting clients, each with its own surface. This crate is the
//! part of that which is arithmetic and can therefore be checked on the host in milliseconds: the
//! scene (which windows exist, where, and in what order they stack), the clipping and
//! damage-rectangle math, the composition itself, and the protocol the parties speak. The prose
//! contract is notes/compositor.md; the processes are `user/src/compositor.rs` (the compositor) and
//! `user/src/window.rs` (a client).
//!
//! The split is the one `gfx_proto` makes for the framebuffer and `line_editor::proto` for the
//! terminal: one definition, shared by the compositor, its clients, the kernel-side test, and the
//! host-side scanout check, so none of them can drift from the others about what the screen should
//! look like.
//!
//! # The shape
//!
//! ```text
//!   virtio-gpu ──► display ──gfx FLUSH(damage)──► compositor ◄──doorbell CALL──── window clients
//!                   │      (rung one's            │  │                        (one surface each)
//!                   └──the scanout, shared────────┘  └──per-client input endpoint (focus)
//! ```
//!
//! The compositor is **rung one's client, unchanged**: it holds the display endpoint and the scanout
//! frames exactly as `painter` did, and `display` cannot tell the difference. Everything rung two adds
//! is above that seam.
//!
//! # Examples
//!
//! The clipping is where the bugs are, and there are **two clips** because each catches a different
//! one. A client reports damage in its own surface coordinates, and it may lie about them:
//!
//! ```
//! use compositor::{EMPTY, Rect, SCREEN_H, SCREEN_W, Window, damage_to_screen};
//!
//! // A window at (8, 8), 64 by 32.
//! let win = Window::new(8, 8, 64, 32);
//!
//! // Ordinary damage, placed on the screen.
//! assert_eq!(damage_to_screen(&win, Rect::new(4, 5, 9, 7)), Rect::new(12, 13, 9, 7));
//!
//! // Damage past the client's OWN edge. Without the first clip the compositor would read pixels the
//! // client does not own, so it is clipped to the surface before it is placed.
//! let clipped = damage_to_screen(&win, Rect::new(60, 30, 100, 100));
//! assert_eq!(clipped, Rect::new(68, 38, 4, 2));
//!
//! // A window hanging off the left edge, with damage in the part that is off-screen. Without the
//! // second clip the compositor would write outside the framebuffer.
//! let hanging = Window::new(-8, 30, 40, 40);
//! assert_eq!(damage_to_screen(&hanging, Rect::new(0, 0, 4, 4)), EMPTY);
//! // The part that IS on screen survives, starting at x = 0.
//! assert_eq!(damage_to_screen(&hanging, Rect::new(0, 0, 16, 4)), Rect::new(0, 30, 8, 4));
//!
//! // Nothing that comes out of this function can leave the screen.
//! let out = damage_to_screen(&hanging, Rect::new(0, 0, 999, 999));
//! assert!(out.right() <= SCREEN_W as i32 && out.bottom() <= SCREEN_H as i32);
//! ```
//!
//! Composition is checked against an **independent** definition of the finished screen, rather than
//! against itself. [`expected_screen_pixel`] answers "which window owns this pixel" by looking down
//! the stack; [`composite`] builds the same picture the other way round, painting the background and
//! then blitting each window in order. They agree only if both are right:
//!
//! ```
//! use compositor::{
//!     Rect, SCENE, SCREEN_PIXELS, SCREEN_W, background_pixel, composite, expected_screen_pixel,
//!     window_pixel,
//! };
//!
//! // What each client painted into its own surface.
//! let surfaces: Vec<Vec<u32>> = SCENE
//!     .iter()
//!     .enumerate()
//!     .map(|(id, w)| {
//!         (0..w.pixels())
//!             .map(|i| {
//!                 let (sx, sy) = ((i % w.w as usize) as u32, (i / w.w as usize) as u32);
//!                 window_pixel(id as u32, sx, sy)
//!             })
//!             .collect()
//!     })
//!     .collect();
//! let refs: Vec<&[u32]> = surfaces.iter().map(|s| s.as_slice()).collect();
//!
//! // Compose all three windows over the whole screen.
//! let mut screen = vec![0u32; SCREEN_PIXELS];
//! composite(&mut screen, &refs, SCENE.len(), Rect::screen());
//!
//! for i in 0..SCREEN_PIXELS {
//!     let (x, y) = ((i % SCREEN_W as usize) as u32, (i / SCREEN_W as usize) as u32);
//!     assert_eq!(screen[i], expected_screen_pixel(SCENE.len(), x, y), "pixel ({x}, {y})");
//! }
//!
//! // Stacking order is observable, which is the point of a scene with overlap: window 1 covers part
//! // of window 0, so the topmost one wins.
//! assert_eq!(expected_screen_pixel(2, 45, 30), window_pixel(1, 5, 6));
//! assert_eq!(expected_screen_pixel(1, 45, 30), window_pixel(0, 37, 22));
//! assert_eq!(expected_screen_pixel(0, 45, 30), background_pixel(45, 30));
//! ```
//!
//! # Where authority lives, and why the doorbell can be shared
//!
//! Every client rings **one shared endpoint** and every request on it is content-free: `HELLO` ("I
//! have started") and `COMMIT` ("look at the surfaces"). That is deliberate, and it is the design's
//! load-bearing idea.
//!
//! A shared endpoint carries **no sender identity** (there are no badged capabilities here; DECISIONS
//! §26.5). So a protocol that named a surface, a window, or a rectangle in its *message* would be
//! forgeable: any client could ask about any other's. Instead:
//!
//! - **every per-client fact lives in per-client memory** (the control page in [`proto::ctl`], which
//!   only that client and the compositor map), so the only surface a client can describe is its own,
//!   and the only pixels it can write are its own;
//!
//! - **every privileged answer travels through privileged memory**, never through the reply words. A
//!   screenshot lands in the requester's own capture mapping; the window list is a page the
//!   compositor publishes. A client that holds no such mapping cannot receive the answer even if it
//!   asks, and asking is not how it would get it anyway;
//!
//! - the reply words carry **status only**, and they are routed by the kernel's one-shot Reply
//!   capability, so a request is answered to whoever called without the compositor ever naming a
//!   caller.
//!
//! The result is a compositor with **no authorization code in it at all**. It does not check who may
//! read the screen, because reading the screen is a mapping it does not hand out. That is the
//! difference in kind from Wayland, whose compositor holds every client's connection and decides by
//! policy what each may do. See notes/compositor.md.
//!
//! # What deliberately is NOT here
//!
//! No GPU acceleration (rung four, milestone 34), no fonts or text (rung three), no window
//! management (the scene is fixed; see [`SCENE`]), and no alpha blending: windows are opaque and
//! composition is a copy in stacking order. The honest limits are listed in notes/compositor.md.
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `compose`. Refused `compose` (a verb,
//! where the tenet says a namespace is a thing; the crate's own first line already said
//! compositor), `compositor_proto` (promises a wire definition and delivers an algorithm) and
//! `scene` (undersells the clipping and damage arithmetic). Sharing the program's name is the point
//! rather than a collision: the crate is that program's logic, lifted out to be host-testable.

#![no_std]
// milestone 68's ratchet is workspace-wide (§107); this crate opts out until its 19-item
// worklist (notes/doc-coverage.md) is burned down.
#![allow(missing_docs)]

// ================================================================================================
// The screen.
// ================================================================================================

/// The screen's width. **The screen is rung one's surface**, so this is `gfx_proto`'s geometry rather
/// than a second declaration of it: the compositor composites into the very frames `painter` used to
/// paint, and flushes them through the same `FLUSH(rect)` the display driver already honours.
pub const SCREEN_W: u32 = gfx_proto::WIDTH;
/// The screen's height. See [`SCREEN_W`].
pub const SCREEN_H: u32 = gfx_proto::HEIGHT;
/// Pixels in the screen.
pub const SCREEN_PIXELS: usize = gfx_proto::PIXELS;

/// The most windows a scene may hold. Fixed because the compositor has no allocator and builds its
/// window table as an array; four is what [`SCENE`] needs and what the kernel wires.
pub const MAX_WINDOWS: usize = 4;

// ================================================================================================
// Rectangles: the arithmetic that compositors get wrong.
// ================================================================================================

/// A rectangle, with a **signed** origin.
///
/// Signed because a window may hang off the left or top edge of the screen, and that case is where
/// clipping bugs live: the naive `u32` version cannot represent it, so it gets "fixed" by moving the
/// window instead, and the off-by-one is never found. Width and height stay unsigned; a negative
/// extent is not a rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// The canonical empty rectangle. Every operation that produces nothing produces *this*, so an empty
/// result compares equal regardless of where the emptiness came from.
pub const EMPTY: Rect = Rect {
    x: 0,
    y: 0,
    w: 0,
    h: 0,
};

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    /// A rectangle covering the whole screen.
    pub const fn screen() -> Rect {
        Rect::new(0, 0, SCREEN_W, SCREEN_H)
    }

    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// The first column past the rectangle. **Half-open**, like every sane pixel interval: a
    /// rectangle covers `x .. right()`, and two rectangles that share this edge do not overlap. Every
    /// classic clipping off-by-one is a confusion between this and "the last column".
    pub const fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    /// The first row past the rectangle. See [`right`](Self::right).
    pub const fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub const fn translate(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// The overlap of two rectangles, or [`EMPTY`]. This is the whole of clipping: a damage rectangle
    /// clipped to a surface, a surface clipped to the screen, and a repaint clipped to the damage are
    /// all this one function.
    pub const fn intersect(&self, o: &Rect) -> Rect {
        if self.is_empty() || o.is_empty() {
            return EMPTY;
        }
        let x = if self.x > o.x { self.x } else { o.x };
        let y = if self.y > o.y { self.y } else { o.y };
        let r = if self.right() < o.right() {
            self.right()
        } else {
            o.right()
        };
        let b = if self.bottom() < o.bottom() {
            self.bottom()
        } else {
            o.bottom()
        };
        if r <= x || b <= y {
            return EMPTY;
        }
        Rect::new(x, y, (r - x) as u32, (b - y) as u32)
    }

    /// The bounding box of two rectangles, ignoring empties. The compositor accumulates one damage
    /// rectangle per frame this way: a bounding box costs a few extra pixels of copying and saves
    /// keeping a region list, which is the right trade at this scale and the wrong one at a
    /// desktop's (notes/compositor.md records it as a limit).
    pub const fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = if self.x < o.x { self.x } else { o.x };
        let y = if self.y < o.y { self.y } else { o.y };
        let r = if self.right() > o.right() {
            self.right()
        } else {
            o.right()
        };
        let b = if self.bottom() > o.bottom() {
            self.bottom()
        } else {
            o.bottom()
        };
        Rect::new(x, y, (r - x) as u32, (b - y) as u32)
    }

    pub const fn area(&self) -> u64 {
        (self.w as u64) * (self.h as u64)
    }

    /// This rectangle as unsigned screen coordinates, for [`gfx_proto::rect`]. `None` if it is empty
    /// or not wholly on the screen, because the display driver **refuses** an out-of-surface flush
    /// rather than clamping it (rung one's rule), so handing it one is a bug we should not paper over.
    pub const fn as_flush_rect(&self) -> Option<(u32, u32, u32, u32)> {
        if self.is_empty() || self.x < 0 || self.y < 0 {
            return None;
        }
        if self.right() > SCREEN_W as i32 || self.bottom() > SCREEN_H as i32 {
            return None;
        }
        Some((self.x as u32, self.y as u32, self.w, self.h))
    }
}

// ================================================================================================
// The scene: which windows exist, and in what order they stack.
// ================================================================================================

/// A window: a client's surface, and where it sits on the screen.
///
/// The surface's own coordinate system starts at `(0, 0)` and is `w` by `h`; `origin` is where that
/// corner lands on the screen and may be negative. A client is never told its origin, and does not
/// need it: it draws in its own coordinates and the compositor places it. That is not a convenience,
/// it is the isolation: a client that does not know where it is on screen cannot reason about what is
/// underneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub origin_x: i32,
    pub origin_y: i32,
    pub w: u32,
    pub h: u32,
}

impl Window {
    pub const fn new(origin_x: i32, origin_y: i32, w: u32, h: u32) -> Window {
        Window {
            origin_x,
            origin_y,
            w,
            h,
        }
    }

    /// Where this window sits on the screen, in screen coordinates.
    pub const fn rect(&self) -> Rect {
        Rect::new(self.origin_x, self.origin_y, self.w, self.h)
    }

    /// The surface's own bounds, in surface coordinates.
    pub const fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    /// Bytes per row of this surface. Tightly packed, like the scanout: no padding, so a stride bug
    /// is a wrong picture rather than a hidden slack byte.
    pub const fn stride(&self) -> u32 {
        self.w * 4
    }

    pub const fn pixels(&self) -> usize {
        (self.w as usize) * (self.h as usize)
    }

    /// How many 4 KiB frames this surface's pixels need. Mapping is frame-granular, so a surface that
    /// does not fill its last frame leaves the tail mapped and unused; that tail is inside the
    /// client's own grant, so it is slack, not a hole.
    pub const fn frames(&self) -> u32 {
        (self.w * self.h * 4).div_ceil(4096)
    }
}

/// **The scene, bottom window first.** A window's index in this array is both its id and its
/// stacking order: index 0 is at the back, the last is in front.
///
/// Fixed at compile time, and that is a real limitation stated plainly: a compositor normally learns
/// its windows from clients that ask for surfaces and from a user who moves them. Rung two's subject
/// is *authority* routing, not window management, so the scene is a constant that the compositor, the
/// clients, the kernel test, and the host-side scanout check all read, which is what makes the
/// composed screen a value that can be predicted and then checked pixel for pixel.
///
/// The geometry is chosen so that a wrong composite cannot pass:
///
/// - three different sizes, so a compositor that assumed one geometry for all surfaces fails;
/// - **overlap**: windows 1 and 2 each cover part of window 0, so stacking order is observable;
/// - **clipping**: window 2 hangs off the left edge (negative origin) and off the bottom, so a
///   compositor that clipped only against the surface, or only against the screen, produces a
///   different picture. (The right and top edges are covered by the host tests with synthetic
///   windows, so the live scene does not have to carry every case.)
pub const SCENE: &[Window] = &[
    Window::new(8, 8, 64, 32),
    Window::new(40, 24, 48, 24),
    Window::new(-8, 30, 40, 40),
];

// ================================================================================================
// The pictures: what a client paints, and what the screen must then hold.
// ================================================================================================

/// The most bytes any surface in [`SCENE`] takes, and so the most a client can be asked to paint. A
/// client checks its published geometry against this before it paints: a geometry larger than the
/// grant it was mapped would make it fault on its own surface, and a client should refuse a control
/// page it cannot believe rather than discover that as a crash.
pub const MAX_SURFACE_BYTES: u32 = {
    let mut m = 0;
    let mut i = 0;
    while i < SCENE.len() {
        let b = SCENE[i].w * SCENE[i].h * 4;
        if b > m {
            m = b;
        }
        i += 1;
    }
    m
};

/// **The small damage rectangle**, in surface coordinates, that the damage test's client commits for
/// its second frame. Here rather than in the client because three parties must agree on it: the
/// client that commits it, the kernel test that predicts the flush rectangle it produces, and the host
/// tests that check the arithmetic without a machine.
///
/// Deliberately not aligned to anything and not at the origin: an odd offset and an odd size are what
/// catch a compositor that rounded a damage rectangle out to a tile or a row.
pub const SMALL_DAMAGE: Rect = Rect::new(4, 5, 9, 7);

/// **What window `id` paints at its own `(x, y)`.** A per-coordinate, per-window function, for the
/// reason `gfx_proto::pixel` is one: a fill proves nothing. A blank surface, a surface painted by the
/// wrong client, a surface blitted at the wrong offset, and a transposed surface all have to be
/// visibly wrong, and they are, because no two windows agree on any pixel and no window is constant.
///
/// The red channel is always **odd**, and [`background_pixel`]'s red is always **even**, so no window
/// pixel can be mistaken for background. That is asserted exhaustively over the whole screen in this
/// crate's tests rather than argued here.
pub const fn window_pixel(id: u32, x: u32, y: u32) -> u32 {
    let k = id.wrapping_mul(53).wrapping_add(11);
    let r = (x.wrapping_mul(3).wrapping_add(k) | 1) & 0xff;
    let g = (y
        .wrapping_mul(7)
        .wrapping_add(x >> 1)
        .wrapping_add(id.wrapping_mul(29)))
        & 0xff;
    let b = ((x ^ (y << 1))
        .wrapping_mul(id.wrapping_mul(2).wrapping_add(13))
        .wrapping_add(id.wrapping_mul(7)))
        & 0xff;
    (r << 16) | (g << 8) | b
}

/// **What the screen holds where no window is.** Coarse 8x8 blocks plus a gradient, so a compositor
/// that forgot to paint the background inside a damage rectangle leaves visibly stale pixels rather
/// than plausible ones. Red is always even (see [`window_pixel`]).
pub const fn background_pixel(x: u32, y: u32) -> u32 {
    let block = ((x >> 3) ^ (y >> 3)) & 1;
    let r = (block.wrapping_mul(0x30)) & 0xfe;
    let g = (0x10 + (y >> 1)) & 0xff;
    let b = (0x08 + (x >> 2)) & 0xff;
    (r << 16) | (g << 8) | b
}

/// **What screen pixel `(x, y)` must be** when the first `n` windows of [`SCENE`] are live: the
/// topmost window covering it, or the background.
///
/// This is the *independent* definition of the composed screen. [`composite`] builds the same picture
/// the other way round (paint the background over the damage, then blit each window in stacking
/// order), so the two agree only if both are right. The kernel test, the client that captures the
/// screen, and the host-side `screendump` check all compare against this one.
pub fn expected_screen_pixel(n: usize, x: u32, y: u32) -> u32 {
    expected_screen_pixel_with(n, x, y, |i, sx, sy| window_pixel(i as u32, sx, sy))
}

/// **What screen pixel `(x, y)` must be when the windows draw something other than
/// [`window_pixel`]**: the same topmost-window-wins lookup, with the per-window picture supplied by
/// the caller as `content(id, surface_x, surface_y)`.
///
/// Added for milestone 29's display terminals (a window whose content is a *character grid* rendered
/// by the `video_terminal` engine, not a coordinate pattern). The composition rule is the compositor's and the
/// content is the client's, and keeping that split here rather than teaching this crate about fonts
/// is why `compositor` still does not depend on `video_terminal`: a window is a rectangle of pixels, whatever put
/// them there.
pub fn expected_screen_pixel_with(
    n: usize,
    x: u32,
    y: u32,
    content: impl Fn(usize, u32, u32) -> u32,
) -> u32 {
    let mut i = n.min(SCENE.len());
    while i > 0 {
        i -= 1;
        let win = &SCENE[i];
        if win.rect().contains(x as i32, y as i32) {
            return content(
                i,
                (x as i32 - win.origin_x) as u32,
                (y as i32 - win.origin_y) as u32,
            );
        }
    }
    background_pixel(x, y)
}

/// The digest of the whole composed screen, using rung one's position-sensitive checksum so the
/// guest-side witnesses and the kernel speak one language about "is the screen right".
pub fn expected_screen_checksum(n: usize) -> u64 {
    gfx_proto::checksum(|i| {
        let x = (i % SCREEN_W as usize) as u32;
        let y = (i / SCREEN_W as usize) as u32;
        expected_screen_pixel(n, x, y)
    })
}

// ================================================================================================
// Composition.
// ================================================================================================

/// Map a client's damage rectangle (in **its own** surface coordinates) to the screen: clip it to the
/// surface, place it, then clip it to the screen. Returns [`EMPTY`] if nothing of it is visible.
///
/// Both clips are needed and each catches a different bug. Without the first, a client that reports
/// damage past its own edge makes the compositor read pixels it does not own. Without the second, a
/// window that hangs off the screen makes the compositor write outside the framebuffer. The client's
/// number is *untrusted input* to the first clip, which is why it is a clip and not an assertion.
pub fn damage_to_screen(win: &Window, damage: Rect) -> Rect {
    let inside = damage.intersect(&win.bounds());
    inside
        .translate(win.origin_x, win.origin_y)
        .intersect(&Rect::screen())
}

/// **Composite the first `n` windows into `screen`, touching only `damage`.**
///
/// `screen` is `SCREEN_W * SCREEN_H` pixels; `sources[i]` is window `i`'s surface, at least
/// `SCENE[i].pixels()` long. `damage` is in screen coordinates and is clipped here, so a caller
/// cannot make this write outside the framebuffer.
///
/// Painter's algorithm: background first, then each window in stacking order, each clipped to the
/// damage. Opaque windows, so "blend" is a copy and the last writer wins, which is what makes the
/// stacking order observable in the result.
///
/// **An empty source is a window that is not on screen yet.** The compositor passes `&[]` for a client
/// that has never committed, so the first paint (before any client has run) is the background alone
/// rather than a screen full of somebody's uninitialised frames. It is also the honest answer to "what
/// does a window with no content look like": nothing.
///
/// Every pixel *outside* `damage` is left exactly as it was. That is the whole point of a damage
/// rectangle, and it is what the host tests and the kernel test both check by poisoning the rest of
/// the screen and finding the poison intact afterwards.
pub fn composite(screen: &mut [u32], sources: &[&[u32]], n: usize, damage: Rect) {
    let damage = damage.intersect(&Rect::screen());
    if damage.is_empty() {
        return;
    }

    for y in damage.y..damage.bottom() {
        let row = (y as u32 as usize) * SCREEN_W as usize;
        for x in damage.x..damage.right() {
            screen[row + x as usize] = background_pixel(x as u32, y as u32);
        }
    }

    for i in 0..n.min(SCENE.len()).min(sources.len()) {
        let win = &SCENE[i];
        let vis = win.rect().intersect(&damage);
        if vis.is_empty() {
            continue;
        }
        let src = sources[i];
        if src.is_empty() {
            continue; // never committed: not on screen
        }
        for y in vis.y..vis.bottom() {
            let sy = (y - win.origin_y) as u32 as usize;
            let drow = (y as u32 as usize) * SCREEN_W as usize;
            let srow = sy * win.w as usize;
            for x in vis.x..vis.right() {
                let sx = (x - win.origin_x) as u32 as usize;
                screen[drow + x as usize] = src[srow + sx];
            }
        }
    }
}

// ================================================================================================
// The protocol.
// ================================================================================================

/// The IPC the compositor serves, and the shared-memory layouts that carry everything the IPC
/// deliberately does not.
///
/// Request words are packed the way `gfx_proto` and `line_editor::proto` pack theirs (opcode in bits
/// 63:56), and the helpers are re-exported rather than redefined so all three contracts read alike.
pub mod proto {
    pub use gfx_proto::{op, operand, req};

    /// `CALL(req(HELLO, 0), 0)` -> `(0, 0)`. "I have started." Carries nothing, and its reply
    /// carries nothing: the client's geometry is in its own control page, which the compositor has
    /// published by the time it can answer this at all. The rendezvous *is* the synchronization, so
    /// no client has to poll for a valid control page.
    pub const HELLO: u64 = 1;

    /// `CALL(req(COMMIT, 0), 0)` -> `(0, 0)`. "Look at the surfaces." The compositor reads every
    /// client's control page, composites whatever changed, flushes the damage to the display, and
    /// replies. Content-free on purpose: a shared endpoint has no sender identity, so a request that
    /// named a surface would let any client speak for any other (see the crate note).
    ///
    /// Because the caller is blocked in `CALL` for the whole of composition, a client cannot race the
    /// compositor's read of its own pixels. That is flow control by rendezvous, not by trust.
    pub const COMMIT: u64 = 2;

    // **There is no capture verb, and no enumerate verb**, and their absence is the design rather
    // than a gap. A `CAPTURE` opcode was drafted and dropped: it would have been safe (the pixels
    // would land in a mapping the caller may not hold, so a forging client would learn nothing), but
    // it would also have been a verb that does nothing for anyone who does not already hold the
    // mapping that *is* the authority. Reading the screen is a read-only mapping; enumerating windows
    // is a read-only page. A client holding neither has nothing to call and nowhere to look, which is
    // a stronger statement than a guarded verb and a smaller protocol. The one thing it costs is a
    // consistent snapshot (a live mapping can be read mid-composite); notes/compositor.md records that
    // as a limit and the served-copy path as what would fix it.

    /// The reply to a word whose opcode the compositor does not implement. Negative-is-an-error, the
    /// convention `fs_proto` set and `gfx_proto` follows.
    pub const EBADOP: i64 = -22;

    /// **A client's control page**: the per-client channel that carries everything the shared
    /// doorbell cannot. Byte offsets into the one 4 KiB frame the compositor shares with exactly one
    /// client, in the *client's* address space at `CTL_VA` and in the compositor's at its own.
    ///
    /// Direction matters and is stated per field, because the trust flows one way in each: what the
    /// compositor writes is authoritative (it published the surface), and what the client writes is
    /// untrusted input to be clipped.
    pub mod ctl {
        /// u32, compositor -> client. [`MAGIC_VALUE`] once the page is valid.
        pub const MAGIC: u64 = 0x00;
        /// u32, compositor -> client: which window this client is. **A label, not a name**: no
        /// request anywhere in this protocol takes a window id from a client, so a client that lies
        /// about its id can only paint the wrong pattern into its own surface.
        pub const ID: u64 = 0x04;
        /// u32, compositor -> client: the surface's width in pixels. A client is *told* its geometry
        /// rather than assuming it, which is the runtime half of the contract rung one added `INFO`
        /// for; at rung two the compositor really does choose it.
        pub const WIDTH: u64 = 0x08;
        /// u32, compositor -> client: the surface's height in pixels.
        pub const HEIGHT: u64 = 0x0c;
        /// u32, compositor -> client: bytes per row.
        pub const STRIDE: u64 = 0x10;
        /// u32, compositor -> client: [`STATUS_OK`] or [`STATUS_CLIPPED`] for the last commit.
        pub const STATUS: u64 = 0x14;
        /// u32, client -> compositor: bumped when the client has painted something new. The
        /// compositor composites a surface only when this changes, so an idle client costs nothing.
        pub const SEQ: u64 = 0x18;
        /// u32, compositor -> client: the [`SEQ`] value the compositor has composited.
        pub const ACKED: u64 = 0x1c;
        /// i32 x u32 x u32 x u32, client -> compositor: the damaged rectangle in **surface**
        /// coordinates. Untrusted: clipped to the surface, never believed.
        pub const DAMAGE_X: u64 = 0x20;
        pub const DAMAGE_Y: u64 = 0x24;
        pub const DAMAGE_W: u64 = 0x28;
        pub const DAMAGE_H: u64 = 0x2c;

        /// "CMP1". A client that reads anything else has not been published yet, which cannot happen
        /// through the `HELLO` rendezvous but is checked anyway: the alternative to a magic number is
        /// painting into a surface whose size you guessed.
        pub const MAGIC_VALUE: u32 = 0x434d_5031;
        pub const STATUS_OK: u32 = 0;
        /// The last commit's damage rectangle was not wholly inside the surface, so it was clipped.
        ///
        /// Rung one **refuses** an out-of-surface rectangle rather than clamping it, because there the
        /// caller is the only party who can tell a coordinate bug from intent. Here the compositor
        /// scans every surface on one identity-free doorbell, so it cannot attribute a bad rectangle
        /// to a caller in order to refuse *it*; and the worst a lie can do is make the compositor
        /// re-copy the liar's own pixels. So it clips, and says so **in the liar's own control page**,
        /// which is per-client feedback through the only channel that can carry it.
        pub const STATUS_CLIPPED: u32 = 1;
    }

    /// **The window list**: one page the compositor publishes and maps *read-only* into whoever was
    /// granted the enumeration authority.
    ///
    /// There is deliberately **no enumerate verb**. Window enumeration is a page, holding it is the
    /// grant, and a client that was not granted it has no mapping there at all: its attempt to look
    /// is a fault at an unmapped address, which is "you hold no such capability" in the address
    /// space's dialect rather than a permission error from a server that checked a list. Screen
    /// sharing is the same act with the screen's own frames, mapped read-only into a third party.
    pub mod wlist {
        /// u32: how many windows the list holds.
        pub const COUNT: u64 = 0x00;
        /// u32: which window currently has input focus. Live: it changes when the compositor moves
        /// focus, which is what lets a holder (and the kernel test) witness a focus decision it did
        /// not make.
        pub const FOCUSED: u64 = 0x04;
        /// The first record. Each is four `i32`/`u32` (x, y, w, h) in stacking order, bottom first.
        pub const RECORDS: u64 = 0x10;
        /// Bytes per record.
        pub const RECORD_STRIDE: u64 = 0x10;
    }

    /// **The input ring**: one page the input source shares with the compositor, and nobody else.
    ///
    /// Keystrokes arrive as bytes here, not as message words, for exactly the reason surfaces do: the
    /// doorbell is shared, so a keystroke carried in a message would be forgeable and any client
    /// could inject input into the focused client. Writing this page is the input driver's authority,
    /// and it is a mapping no client has.
    ///
    /// The compositor forwards what it reads here to the focused client as
    /// `line_editor::proto::OP_BYTES`, the terminal contract's driver half verbatim
    /// (notes/terminal-contract.md), so a terminal is a client of this compositor without changing a
    /// line of either contract.
    pub mod ring {
        /// u32, compositor: bytes consumed.
        pub const HEAD: u64 = 0x00;
        /// u32, source: bytes produced. The source writes bytes, then this, then rings the doorbell.
        pub const TAIL: u64 = 0x04;
        /// The byte buffer.
        pub const BYTES: u64 = 0x08;
        /// How many bytes the ring holds. Small: this is a keyboard, not a pipe.
        pub const CAPACITY: u32 = 256;
    }

    /// The byte that moves focus to the next window: ASCII TAB. A policy decision, made in
    /// userspace, in the compositor, where window policy belongs; the kernel never sees it.
    pub const FOCUS_NEXT: u8 = 0x09;
}

/// What each process reports to whoever spawned it. Status, not contract: a client cannot ask for any
/// of this, and the kernel-side test is the only party that compares the accounts.
pub mod status {
    /// The compositor is up: `send(REPORT, COMP_UP, window count, focused)`. Its control pages are
    /// published and it is serving the doorbell.
    /// `COMP_UP` is the compositor's **only** report, ever, and that is deliberate: a status `SEND`
    /// is a rendezvous, so a compositor that narrated each frame would block until its spawner
    /// listened, and a spawner that stopped listening would deadlock the clients behind it. What was
    /// flushed is observable where it belongs, at the display endpoint (kernel/src/user.rs's display
    /// stand-in reads the rectangle straight off the `FLUSH`).
    pub const COMP_UP: u64 = 0xC33_0001;

    /// A client painted and committed: `send(REPORT, WIN_PAINTED, digest, seq)`, where `digest` is
    /// [`super::surface_checksum`] over its own surface read back after the commit.
    pub const WIN_PAINTED: u64 = 0xC33_0011;
    /// A client asked for something it holds no capability for and was refused:
    /// `send(REPORT, WIN_REFUSED, errno, what)`. `errno` is the kernel's, and the whole point is
    /// which one: `NoSuchSlot` (-1), "there is nothing there", not a permission error.
    pub const WIN_REFUSED: u64 = 0xC33_0012;
    /// A capture-capable client read the screen: `send(REPORT, WIN_CAPTURED, digest, focused | count << 32)`.
    pub const WIN_CAPTURED: u64 = 0xC33_0013;
    /// A client re-read its own surface after a neighbour attacked it:
    /// `send(REPORT, WIN_INTACT, digest, 0)`. The witness that matters for isolation.
    pub const WIN_INTACT: u64 = 0xC33_0014;
    /// A client received input: `send(REPORT, WIN_INPUT, byte, count so far)`.
    pub const WIN_INPUT: u64 = 0xC33_0015;
    /// A client is about to touch memory it was not granted: `send(REPORT, WIN_PROBING, va, 0)`.
    /// Sent *before* the access, because the access is expected to be fatal, and a test that saw no
    /// probe report would otherwise not know whether the probe happened at all.
    pub const WIN_PROBING: u64 = 0xC33_0016;
    /// A client survived an access it should not have: `send(REPORT, WIN_ESCAPED, va, value)`. If a
    /// test ever sees this word, the isolation this milestone exists to prove is broken.
    pub const WIN_ESCAPED: u64 = 0xC33_0017;
}

/// A position-sensitive digest of a `w` by `h` surface, `read(i)` giving pixel `i` in row-major
/// order. Rung one's checksum, reused so every witness in the system digests the same way.
pub fn surface_checksum(w: u32, h: u32, mut read: impl FnMut(usize) -> u32) -> u64 {
    let mut hsh: u64 = 0xcbf2_9ce4_8422_2325;
    for i in 0..(w as usize) * (h as usize) {
        for b in read(i).to_le_bytes() {
            hsh ^= b as u64;
            hsh = hsh.wrapping_mul(0x100_0000_01b3);
        }
    }
    hsh
}

/// The digest window `id` must report for a surface holding exactly its pattern.
pub fn expected_window_checksum(id: usize) -> u64 {
    let win = &SCENE[id];
    surface_checksum(win.w, win.h, |i| {
        let x = (i % win.w as usize) as u32;
        let y = (i / win.w as usize) as u32;
        window_pixel(id as u32, x, y)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    /// **Half-open intervals, or every clip is off by one.** Two rectangles sharing an edge do not
    /// overlap; a rectangle one pixel further in does. This is the bug this whole crate exists to
    /// keep out of the compositor, so it is the first test.
    #[test]
    fn a_shared_edge_is_not_an_overlap() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(
            a.intersect(&Rect::new(10, 0, 10, 10)),
            EMPTY,
            "rectangles that touch at x=10 must not overlap",
        );
        assert_eq!(
            a.intersect(&Rect::new(9, 0, 10, 10)),
            Rect::new(9, 0, 1, 10),
            "a one-pixel overlap must be found",
        );
        assert_eq!(a.intersect(&Rect::new(0, 10, 10, 10)), EMPTY);
        assert_eq!(
            a.intersect(&Rect::new(0, 9, 10, 10)),
            Rect::new(0, 9, 10, 1)
        );
        assert_eq!(a.intersect(&Rect::new(-5, -5, 5, 5)), EMPTY, "up and left");
        assert_eq!(
            a.intersect(&Rect::new(-5, -5, 6, 6)),
            Rect::new(0, 0, 1, 1),
            "a negative origin still intersects correctly",
        );
        assert_eq!(a.intersect(&EMPTY), EMPTY, "nothing overlaps nothing");
        assert_eq!(EMPTY.intersect(&a), EMPTY);
        // Containment both ways round.
        let inner = Rect::new(2, 3, 4, 5);
        assert_eq!(a.intersect(&inner), inner);
        assert_eq!(inner.intersect(&a), inner);
    }

    /// `contains` agrees with `intersect` on every pixel of a small area, including the edges. Two
    /// independent phrasings of "is this pixel in this rectangle" that must not disagree, since
    /// `expected_screen_pixel` uses one and `composite` uses the other.
    #[test]
    fn contains_and_intersect_agree_pixel_by_pixel() {
        let r = Rect::new(-3, 2, 7, 4);
        for y in -6..12 {
            for x in -6..12 {
                let by_contains = r.contains(x, y);
                let by_intersect = !r.intersect(&Rect::new(x, y, 1, 1)).is_empty();
                assert_eq!(by_contains, by_intersect, "disagreement at ({x},{y})");
            }
        }
    }

    /// A union is a bounding box, and an empty rectangle contributes nothing. The compositor unions
    /// the frame's damage this way, so a union that swallowed an empty into `(0,0)` would flush the
    /// top-left corner of the screen on every frame for no reason.
    #[test]
    fn a_union_ignores_empties() {
        let a = Rect::new(4, 4, 2, 2);
        let b = Rect::new(10, 1, 1, 20);
        assert_eq!(a.union(&EMPTY), a);
        assert_eq!(EMPTY.union(&a), a);
        assert_eq!(EMPTY.union(&EMPTY), EMPTY);
        assert_eq!(a.union(&b), Rect::new(4, 1, 7, 20));
        assert_eq!(a.union(&b), b.union(&a), "union is symmetric");
    }

    /// **A damage rectangle is clipped twice, and each clip catches a different lie.** A client that
    /// claims damage past its own edge must not make the compositor read another surface's pixels; a
    /// window that hangs off the screen must not make it write outside the framebuffer.
    #[test]
    fn damage_is_clipped_to_the_surface_and_then_to_the_screen() {
        // A window wholly on screen: an honest rectangle passes through, placed.
        let w = Window::new(8, 8, 64, 32);
        assert_eq!(
            damage_to_screen(&w, Rect::new(2, 3, 4, 5)),
            Rect::new(10, 11, 4, 5),
        );
        // A client claiming the whole world gets its own surface, and no more.
        assert_eq!(
            damage_to_screen(&w, Rect::new(0, 0, 10_000, 10_000)),
            w.rect(),
        );
        // A client claiming a rectangle entirely outside its surface gets nothing.
        assert_eq!(damage_to_screen(&w, Rect::new(64, 0, 10, 10)), EMPTY);
        assert_eq!(damage_to_screen(&w, Rect::new(0, 32, 10, 10)), EMPTY);
        // Exactly the surface's last pixel: the boundary case a `<=` instead of a `<` would lose.
        assert_eq!(
            damage_to_screen(&w, Rect::new(63, 31, 1, 1)),
            Rect::new(71, 39, 1, 1),
        );

        // Off the left and top: the case a u32 origin cannot even represent.
        let nw = Window::new(-8, -6, 40, 40);
        assert_eq!(
            damage_to_screen(&nw, Rect::new(0, 0, 40, 40)),
            Rect::new(0, 0, 32, 34),
            "the parts off the left and top edges must be clipped away",
        );
        assert_eq!(
            damage_to_screen(&nw, Rect::new(0, 0, 4, 4)),
            EMPTY,
            "damage entirely off the screen is nothing to flush",
        );
        // Off the right and bottom.
        let se = Window::new(SCREEN_W as i32 - 8, SCREEN_H as i32 - 4, 32, 32);
        assert_eq!(
            damage_to_screen(&se, Rect::new(0, 0, 32, 32)),
            Rect::new(SCREEN_W as i32 - 8, SCREEN_H as i32 - 4, 8, 4),
        );
        // A window flush against the right edge is NOT clipped: the classic one-pixel error.
        let edge = Window::new(SCREEN_W as i32 - 16, 0, 16, 8);
        assert_eq!(
            damage_to_screen(&edge, Rect::new(0, 0, 16, 8)),
            edge.rect(),
            "a window ending exactly at the screen edge must survive whole",
        );
    }

    /// The scene is a fair test: every window is visible somewhere, some window is on top of another
    /// somewhere, some window is clipped, and the background shows. A scene that failed any of these
    /// would let a broken compositor pass.
    #[test]
    fn the_scene_exercises_overlap_clipping_and_background() {
        let n = SCENE.len();
        let mut visible = vec![0usize; n];
        let mut background = 0;
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                let mut top = None;
                for (i, win) in SCENE.iter().enumerate() {
                    if win.rect().contains(x as i32, y as i32) {
                        top = Some(i);
                    }
                }
                match top {
                    Some(i) => visible[i] += 1,
                    None => background += 1,
                }
            }
        }
        for (i, &count) in visible.iter().enumerate() {
            assert!(count > 0, "window {i} is completely hidden");
        }
        assert!(background > 0, "no background is visible anywhere");

        // Overlap: at least two windows must cover some common pixel, or stacking order is untested.
        let mut overlapping = 0;
        for (i, a) in SCENE.iter().enumerate() {
            for b in &SCENE[i + 1..] {
                overlapping += a.rect().intersect(&b.rect()).area();
            }
        }
        assert!(
            overlapping > 0,
            "no two windows overlap: z-order is untested"
        );

        // Clipping: some window must extend past an edge.
        assert!(
            SCENE
                .iter()
                .any(|w| w.rect().intersect(&Rect::screen()) != w.rect()),
            "every window fits on screen: clipping is untested",
        );
        // And off the LEFT edge specifically. The scene's doc promises a negative origin,
        // which is the case this crate's signed `Rect` exists to represent; a scene whose
        // windows all start on screen would still pass the check above by clipping at the
        // bottom, leaving the negative-origin path exercised only by synthetic rectangles.
        assert!(
            SCENE.iter().any(|w| w.origin_x < 0),
            "no window hangs off the left edge: the live scene never clips a negative origin",
        );
        // And every surface must fit the frames the kernel grants it.
        for (i, win) in SCENE.iter().enumerate() {
            assert!(
                win.pixels() * 4 <= win.frames() as usize * 4096,
                "window {i} does not fit its frames",
            );
        }
    }

    /// **No pixel of the composed screen is ambiguous.** At every screen position, the pattern of the
    /// window that should own it differs from every other window's pattern *at that same position* and
    /// from the background. Without this, a z-order or offset bug could produce the right value by
    /// coincidence and the pixel checks would not see it.
    #[test]
    fn every_screen_pixel_distinguishes_its_owner() {
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                let want = expected_screen_pixel(SCENE.len(), x, y);
                let bg = background_pixel(x, y);
                let mut candidates: Vec<u32> = Vec::new();
                for (i, win) in SCENE.iter().enumerate() {
                    if win.rect().contains(x as i32, y as i32) {
                        candidates.push(window_pixel(
                            i as u32,
                            (x as i32 - win.origin_x) as u32,
                            (y as i32 - win.origin_y) as u32,
                        ));
                    }
                }
                if candidates.is_empty() {
                    assert_eq!(want, bg, "({x},{y}) should be background");
                    continue;
                }
                assert_ne!(want, bg, "window pixel ({x},{y}) looks like background");
                for (k, &c) in candidates.iter().enumerate() {
                    if k + 1 == candidates.len() {
                        assert_eq!(want, c, "({x},{y}) is not the topmost window's pixel");
                    } else {
                        assert_ne!(
                            want, c,
                            "({x},{y}): a lower window's pixel is indistinguishable from the top's",
                        );
                    }
                }
            }
        }
    }

    /// A window's pattern is not a fill, is not symmetric, and is nobody else's. The `gfx_proto`
    /// pattern tests' properties, per window.
    #[test]
    fn a_window_pattern_cannot_be_produced_by_accident() {
        for id in 0..MAX_WINDOWS as u32 {
            let first = window_pixel(id, 0, 0);
            assert!(
                (0..64).any(|y| (0..64).any(|x| window_pixel(id, x, y) != first)),
                "window {id}'s pattern is a solid fill",
            );
            for x in 0..63 {
                assert_ne!(
                    window_pixel(id, x, 5),
                    window_pixel(id, x + 1, 5),
                    "window {id} repeats along a row at x={x}",
                );
            }
            for y in 0..63 {
                assert_ne!(
                    window_pixel(id, 7, y),
                    window_pixel(id, 7, y + 1),
                    "window {id} repeats along a column at y={y}",
                );
            }
            assert_eq!(window_pixel(id, 3, 9) >> 24, 0, "the X byte must be zero");
            // Red odd for windows, even for background: the invariant the ambiguity test leans on.
            assert_eq!((window_pixel(id, 11, 13) >> 16) & 1, 1);
        }
        for y in 0..32u32 {
            for x in 0..32u32 {
                assert_eq!(
                    (background_pixel(x, y) >> 16) & 1,
                    0,
                    "background red is odd"
                );
            }
        }
    }

    /// Paint the surfaces, composite the whole screen, and it must equal the per-pixel definition.
    /// Two algorithms, one answer: painter's-algorithm-with-clipping against topmost-window-lookup.
    #[test]
    fn compositing_the_whole_screen_matches_the_expected_picture() {
        let surfaces = painted_scene();
        let refs: Vec<&[u32]> = surfaces.iter().map(|s| s.as_slice()).collect();
        let mut screen = vec![0u32; SCREEN_PIXELS];
        composite(&mut screen, &refs, SCENE.len(), Rect::screen());
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                assert_eq!(
                    screen[(y * SCREEN_W + x) as usize],
                    expected_screen_pixel(SCENE.len(), x, y),
                    "composed screen differs at ({x},{y})",
                );
            }
        }
        assert_eq!(
            gfx_proto::checksum(|i| screen[i]),
            expected_screen_checksum(SCENE.len()),
            "the digest the guest reports and the picture the host checks must agree",
        );
    }

    /// **A damaged composite touches the damage and nothing else.** Poison the screen, composite one
    /// window's damage rectangle, and every pixel outside it must still hold the poison while every
    /// pixel inside holds the composed picture. This is "a one-window redraw does not cost a full
    /// screen", proved on the host in microseconds; the kernel test proves the same thing end to end
    /// by watching what the compositor flushes.
    #[test]
    fn a_damaged_composite_leaves_the_rest_of_the_screen_alone() {
        let surfaces = painted_scene();
        let refs: Vec<&[u32]> = surfaces.iter().map(|s| s.as_slice()).collect();
        const POISON: u32 = 0xDEAD_BEEF;
        let mut screen = vec![POISON; SCREEN_PIXELS];

        let damage = damage_to_screen(&SCENE[1], SMALL_DAMAGE);
        assert_eq!(damage, Rect::new(44, 29, 9, 7));
        assert!(
            damage.area() * 20 < Rect::screen().area(),
            "the 'small' damage rectangle is not small: this test proves nothing about cost",
        );
        composite(&mut screen, &refs, SCENE.len(), damage);

        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                let got = screen[(y * SCREEN_W + x) as usize];
                if damage.contains(x as i32, y as i32) {
                    assert_eq!(
                        got,
                        expected_screen_pixel(SCENE.len(), x, y),
                        "inside the damage, ({x},{y}) was not composited",
                    );
                } else {
                    assert_eq!(got, POISON, "outside the damage, ({x},{y}) was overwritten");
                }
            }
        }
        // The damaged region is window 1's, and window 0 lies **underneath** it there, so the check
        // above is also a z-order check inside a damage rectangle: a compositor that painted the
        // windows in the wrong order, or stopped at the damaged one, would leave window 0's pixels
        // showing. Asserted rather than assumed, because if the scene ever changes shape this test
        // would quietly stop testing that.
        assert!(
            !SCENE[0].rect().intersect(&damage).is_empty(),
            "no window lies under the damaged one: the z-order half of this test is inert",
        );
    }

    /// **A window that has not committed is not on screen**, and the screen before any client has run
    /// is exactly the background. This is the state the compositor's first paint produces, and the
    /// state the kernel test checks the driver's own digest against, so it needs to be a defined
    /// picture rather than whatever the surfaces happened to hold.
    #[test]
    fn an_uncommitted_window_is_not_drawn() {
        let mut screen = vec![0u32; SCREEN_PIXELS];
        let none: [&[u32]; 3] = [&[], &[], &[]];
        composite(&mut screen, &none, SCENE.len(), Rect::screen());
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                assert_eq!(
                    screen[(y * SCREEN_W + x) as usize],
                    background_pixel(x, y),
                    "({x},{y}) is not background before anyone committed",
                );
            }
        }
        assert_eq!(
            gfx_proto::checksum(|i| screen[i]),
            expected_screen_checksum(0),
            "the empty screen must be the n=0 expected picture",
        );

        // And one committed window among uncommitted ones draws only itself.
        let surfaces = painted_scene();
        let one: [&[u32]; 3] = [&[], surfaces[1].as_slice(), &[]];
        composite(&mut screen, &one, SCENE.len(), Rect::screen());
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                let inside = SCENE[1].rect().contains(x as i32, y as i32);
                let want = if inside {
                    window_pixel(
                        1,
                        (x as i32 - SCENE[1].origin_x) as u32,
                        (y as i32 - SCENE[1].origin_y) as u32,
                    )
                } else {
                    background_pixel(x, y)
                };
                assert_eq!(screen[(y * SCREEN_W + x) as usize], want, "at ({x},{y})");
            }
        }
    }

    /// A caller cannot make `composite` write outside the framebuffer, however absurd the rectangle.
    /// The compositor clips its own damage before calling this, so this is the belt to that braces: a
    /// bug there must not become a wild write here.
    #[test]
    fn composite_cannot_be_talked_outside_the_framebuffer() {
        let surfaces = painted_scene();
        let refs: Vec<&[u32]> = surfaces.iter().map(|s| s.as_slice()).collect();
        let mut screen = vec![0u32; SCREEN_PIXELS];
        for r in [
            Rect::new(-1000, -1000, 5000, 5000),
            Rect::new(SCREEN_W as i32, 0, 10, 10),
            Rect::new(0, SCREEN_H as i32, 10, 10),
            Rect::new(-10, -10, 5, 5),
            EMPTY,
        ] {
            composite(&mut screen, &refs, SCENE.len(), r); // must not panic or index out of range
        }
    }

    /// The flush rectangle handed to the display driver is always inside the screen, and an empty or
    /// off-screen one is refused rather than turned into a bogus flush. Rung one refuses an
    /// out-of-surface rectangle, so producing one would be a driver-visible bug.
    #[test]
    fn only_an_on_screen_rectangle_becomes_a_flush() {
        assert_eq!(
            Rect::screen().as_flush_rect(),
            Some((0, 0, SCREEN_W, SCREEN_H))
        );
        assert_eq!(Rect::new(3, 4, 5, 6).as_flush_rect(), Some((3, 4, 5, 6)));
        assert_eq!(EMPTY.as_flush_rect(), None);
        assert_eq!(Rect::new(-1, 0, 5, 5).as_flush_rect(), None);
        assert_eq!(Rect::new(0, -1, 5, 5).as_flush_rect(), None);
        assert_eq!(
            Rect::new(SCREEN_W as i32 - 4, 0, 5, 5).as_flush_rect(),
            None,
            "one pixel past the right edge is not a flush",
        );
        assert_eq!(
            Rect::new(SCREEN_W as i32 - 5, 0, 5, 5).as_flush_rect(),
            Some((SCREEN_W - 5, 0, 5, 5)),
            "exactly against the right edge is",
        );
        // And what the driver is handed round-trips through its own packing.
        let (x, y, w, h) = Rect::new(9, 8, 7, 6).as_flush_rect().unwrap();
        assert_eq!(gfx_proto::unrect(gfx_proto::rect(x, y, w, h)), (9, 8, 7, 6));
        assert!(gfx_proto::rect_in_surface(x, y, w, h));
    }

    /// The control page and the window list fit in one frame each, with no field overlapping another.
    /// Two processes read these offsets from different address spaces; an overlap would be a silent
    /// corruption rather than a compile error.
    #[test]
    fn the_shared_page_layouts_are_consistent() {
        use proto::{ctl, ring, wlist};
        let fields = [
            ctl::MAGIC,
            ctl::ID,
            ctl::WIDTH,
            ctl::HEIGHT,
            ctl::STRIDE,
            ctl::STATUS,
            ctl::SEQ,
            ctl::ACKED,
            ctl::DAMAGE_X,
            ctl::DAMAGE_Y,
            ctl::DAMAGE_W,
            ctl::DAMAGE_H,
        ];
        for (i, &a) in fields.iter().enumerate() {
            assert_eq!(a % 4, 0, "field {i} is not 4-byte aligned");
            for &b in &fields[i + 1..] {
                assert_ne!(a, b, "two control fields share an offset");
            }
            assert!(a + 4 <= 4096, "a control field runs off its page");
        }
        assert!(
            wlist::RECORDS + wlist::RECORD_STRIDE * MAX_WINDOWS as u64 <= 4096,
            "the window list does not fit in a page",
        );
        assert!(
            ring::BYTES + ring::CAPACITY as u64 <= 4096,
            "the input ring does not fit in a page",
        );
        assert_eq!(ctl::MAGIC_VALUE, 0x434d_5031);
    }

    /// A request word round-trips, and an unknown opcode has a defined answer. The compositor's whole
    /// message vocabulary is three content-free words, which is the point; this pins them.
    #[test]
    fn the_request_words_round_trip() {
        for op in [proto::HELLO, proto::COMMIT] {
            let w = proto::req(op, 0);
            assert_eq!(proto::op(w), op);
            assert_eq!(proto::operand(w), 0, "these verbs carry no operand at all");
        }
        assert_ne!(proto::HELLO, proto::COMMIT);
        assert_eq!(proto::EBADOP, -22);
        // Two verbs, both content-free, is the whole vocabulary. If a third ever arrives, the question
        // to ask first is whether it needs to know who is calling, because this endpoint cannot say.
        assert_eq!(
            proto::COMMIT,
            2,
            "the protocol grew a verb without a decision"
        );
    }

    /// The per-window digest a client reports is the digest of that window's pattern, and no two
    /// windows share one. The kernel compares a client's report against this without trusting it.
    #[test]
    fn window_digests_are_distinct_and_predictable() {
        let mut seen: Vec<u64> = Vec::new();
        for (id, win) in SCENE.iter().enumerate() {
            let d = expected_window_checksum(id);
            assert!(!seen.contains(&d), "two windows digest the same");
            seen.push(d);
            // A blank surface must not match, which is the failure a client that never painted has.
            let blank = surface_checksum(win.w, win.h, |_| 0);
            assert_ne!(d, blank);
        }
    }

    /// A rectangle with one zero extent is empty, whatever the other extent says. `is_empty` is
    /// the guard on every clip and union in this crate, and "zero width OR zero height" is the
    /// half of it the canonical [`EMPTY`] (which zeroes both) can never witness.
    #[test]
    fn a_degenerate_rectangle_is_empty() {
        assert!(Rect::new(7, 9, 0, 5).is_empty(), "zero width is empty");
        assert!(Rect::new(7, 9, 5, 0).is_empty(), "zero height is empty");
        // The consequence a caller sees: a degenerate rect contributes nothing to the frame's
        // damage union. Were it treated as real, its far-away origin would smear the bounding
        // box and every frame would flush pixels nothing touched.
        let a = Rect::new(1, 2, 3, 4);
        assert_eq!(Rect::new(100, 100, 0, 5).union(&a), a);
        assert_eq!(a.union(&Rect::new(100, 100, 5, 0)), a);
        assert_eq!(Rect::new(3, 4, 0, 5).as_flush_rect(), None);
    }

    /// The geometry the compositor publishes is exact: stride is width times four with no
    /// padding, and [`MAX_SURFACE_BYTES`] is the scene's largest surface, byte for byte. These
    /// numbers cross a control page into another process, which paints by them; an
    /// off-by-anything here is a fault in the client's own address space.
    #[test]
    fn stride_and_max_surface_bytes_are_exact() {
        assert_eq!(Window::new(8, 8, 64, 32).stride(), 256);
        assert_eq!(
            Window::new(0, 0, 1, 1).stride(),
            4,
            "one pixel is four bytes"
        );
        // 64*32*4 = 8192, 48*24*4 = 4608, 40*40*4 = 6400: the first window is the largest.
        assert_eq!(MAX_SURFACE_BYTES, 8192);
        assert_eq!(
            MAX_SURFACE_BYTES,
            SCENE.iter().map(|w| w.w * w.h * 4).max().unwrap(),
            "the const loop and the iterator must find the same maximum",
        );
    }

    /// **Exact pixel values, hand-computed from the definitions.** The property tests prove
    /// the patterns are distinct and asymmetric, but a flipped mask, shift, or bit-op can keep
    /// every property while changing every pixel, and the guest writes these exact words into
    /// shared memory. Inputs are chosen so `&`, `|`, `^` and both shift directions each give a
    /// different answer; the arithmetic for each constant is shown where it is asserted.
    #[test]
    fn known_pixels_pin_the_pattern_arithmetic() {
        // window_pixel(2, 5, 3): k = 2*53 + 11 = 117;
        //   r = ((5*3 + 117) | 1) & 0xff           = 133 = 0x85
        //   g = (3*7 + (5 >> 1) + 2*29) & 0xff     = 21 + 2 + 58 = 81 = 0x51
        //   b = ((5 ^ (3 << 1)) * (2*2 + 13) + 2*7) & 0xff = 3*17 + 14 = 65 = 0x41
        // x=5 against y<<1=6 pins the xor: 5^6 = 3, 5|6 = 7, 5&6 = 4, all different.
        assert_eq!(window_pixel(2, 5, 3), 0x0085_5141);
        // window_pixel(0, 7, 4): k = 11;
        //   r = ((21 + 11) | 1) & 0xff = 33 = 0x21
        //   g = (28 + 3 + 0) & 0xff    = 31 = 0x1f
        //   b = ((7 ^ 8) * 13 + 0) & 0xff = 195 = 0xc3
        assert_eq!(window_pixel(0, 7, 4), 0x0021_1fc3);

        // background_pixel walks 8x8 blocks; (x>>3, y>>3) of (1,0), (1,1), (2,0) pin the xor
        // and the &1 in turn (block parities 1, 0, and 0-from-an-even-2).
        // (8, 0):  block 1^0 = 1 -> r = 0x30; g = 0x10 + 0 = 0x10; b = 0x08 + 2 = 0x0a.
        assert_eq!(background_pixel(8, 0), 0x0030_100a);
        // (8, 8):  block 1^1 = 0 -> r = 0; g = 0x10 + 4 = 0x14; b = 0x0a.
        assert_eq!(background_pixel(8, 8), 0x0000_140a);
        // (16, 0): block (2^0)&1 = 0 -> r = 0; g = 0x10; b = 0x08 + 4 = 0x0c.
        assert_eq!(background_pixel(16, 0), 0x0000_100c);
    }

    /// **The digest is FNV-1a 64 over exactly `w * h` pixels, in index order**, pinned against
    /// a value computed with an independent FNV-1a implementation. Every other checksum test in
    /// this crate compares the function to itself, so a digest that iterated one pixel short,
    /// or or-ed where it should xor, would agree with itself everywhere and never be seen.
    #[test]
    fn the_surface_checksum_is_fnv1a_over_every_pixel() {
        // Six pixels with asymmetric bytes, as a 2x3 surface. FNV-1a 64: start at the offset
        // basis 0xcbf2_9ce4_8422_2325; per little-endian byte, h ^= b, then h *= 0x100_0000_01b3.
        let px: [u32; 6] = [
            0x12b7_45d9,
            0x00e1_073c,
            0x8a5f_3b16,
            0x47c2_d801,
            0x3e99_a47f,
            0x6d18_c5b2,
        ];
        assert_eq!(surface_checksum(2, 3, |i| px[i]), 0x23f8_e2f3_afb0_3cb0);
        // And the pixel count really is w * h, stated directly rather than inferred from the
        // digest: a 2x3 surface is read six times, not five (w + h) and not zero (w / h).
        let mut reads = 0;
        surface_checksum(2, 3, |_| {
            reads += 1;
            0
        });
        assert_eq!(reads, 6);
    }

    /// A window's expected digest unfolds pixel `i` as `(i % w, i / w)`, row-major. Checked
    /// against the same surface built by the nested loops the clients actually paint with, so a
    /// transposed or misfolded index cannot agree with it.
    #[test]
    fn a_window_digest_reads_the_surface_row_major() {
        let surfaces = painted_scene();
        for (id, win) in SCENE.iter().enumerate() {
            let s = &surfaces[id];
            assert_eq!(
                expected_window_checksum(id),
                surface_checksum(win.w, win.h, |i| s[i]),
                "window {id}'s digest does not match its painted surface",
            );
        }
    }

    /// Every surface painted with its own window's pattern, as the clients paint them.
    fn painted_scene() -> Vec<Vec<u32>> {
        let mut surfaces = Vec::new();
        for (i, win) in SCENE.iter().enumerate() {
            let mut s = vec![0u32; win.pixels()];
            for y in 0..win.h {
                for x in 0..win.w {
                    s[(y * win.w + x) as usize] = window_pixel(i as u32, x, y);
                }
            }
            surfaces.push(s);
        }
        surfaces
    }
}
