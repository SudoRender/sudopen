//! The text view: a read-only, virtualized NSView that draws only the lines
//! on screen. It does its own scrolling (and draws its own overlay scroll
//! bars) because NSScrollView misbehaves with documents that are hundreds of
//! millions of pixels tall.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use memchr::memrchr;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
    NSAttributedStringNSStringDrawing, NSBeep, NSBezierPath, NSColor, NSCursor, NSEvent,
    NSEventModifierFlags, NSFont, NSFontAttributeName, NSFontWeightRegular,
    NSForegroundColorAttributeName, NSPasteboard, NSPasteboardTypeString, NSStringDrawing, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedStringKey, NSDictionary, NSMutableAttributedString,
    NSObject, NSPoint, NSRange, NSRect, NSSize, NSString,
};
use sudopen_core::Document;
use sudopen_core::highlight::{self, BLOCK, BlockCache, HlBlock};
use sudopen_core::layout::{self, DEFAULT_COLOR};
use sudopen_core::lines::{Progress, line_bounds};

use crate::ipc::now_ns;

const FONT_SIZE: f64 = 13.0;
const PAD_TOP: f64 = 6.0;
const PAD_BOTTOM: f64 = 24.0;
const PAD_TEXT: f64 = 12.0;
/// Width of the zone at the right/bottom edge that grabs the scroll bars.
const SCROLLER_HIT: f64 = 14.0;
const SCROLLER_SHOW: Duration = Duration::from_millis(1200);
/// Copying more than this to the clipboard is refused.
const MAX_COPY: u64 = 256 << 20;

type Attrs = NSDictionary<NSAttributedStringKey, AnyObject>;

/// Sent back to `sudopen --timing` once the first frame is drawn.
pub struct FirstPaint {
    pub t0: u64,
    pub tx: Option<Sender<String>>,
    pub path: String,
    /// (phase, ns timestamp), for the `--timing` breakdown.
    pub marks: Vec<(&'static str, u64)>,
}

impl FirstPaint {
    pub fn new(t0: u64, tx: Option<Sender<String>>, path: String) -> FirstPaint {
        FirstPaint { t0, tx, path, marks: Vec::new() }
    }

    pub fn mark(&mut self, phase: &'static str) {
        self.marks.push((phase, now_ns()));
    }

    /// Sends "painted\t<total ms>\t<path>\t<phase breakdown>".
    pub fn report(mut self) {
        self.mark("drawn");
        let Some(tx) = self.tx else { return };
        let mut prev = self.t0;
        let detail: Vec<String> = self
            .marks
            .iter()
            .map(|&(name, t)| {
                let d = t.saturating_sub(prev) as f64 / 1e6;
                prev = t;
                format!("{name} {d:.1}")
            })
            .collect();
        let total = prev.saturating_sub(self.t0) as f64 / 1e6;
        let _ = tx.send(format!("painted\t{total:.1}\t{}\t{}", self.path, detail.join(", ")));
    }
}

struct Palette {
    dark: bool,
    bg: Retained<NSColor>,
    fg: Retained<NSColor>,
    gutter_fg: Retained<NSColor>,
    selection: Retained<NSColor>,
    knob: Retained<NSColor>,
}

#[derive(Clone, Copy)]
enum Gran {
    Char,
    Word,
    Line,
}

#[derive(Clone, Copy)]
enum Drag {
    None,
    Select { gran: Gran, origin: (u64, u64) },
    VKnob { grab: f64 },
    HKnob { grab: f64 },
}

pub struct State {
    doc: Document,
    font: Retained<NSFont>,
    char_w: f64,
    line_h: f64,
    text_dy: f64,
    scroll_x: f64,
    scroll_y: f64,
    pal: Palette,
    base_attrs: Retained<Attrs>,
    gutter_attrs: Retained<Attrs>,
    cache: BlockCache,
    colors: HashMap<u32, Retained<NSColor>>,
    sel: Option<(u64, u64)>,
    drag: Drag,
    scroller_until: Option<Instant>,
    /// Widest line seen while drawing, in columns (tabs make lines wider
    /// than their byte length).
    widest_cols: u64,
    progress: Progress,
    exact_blocks_seen: u64,
    first_paint: Option<FirstPaint>,
    /// Line to jump to once indexing reaches it.
    pending_goto: Option<u64>,
    /// Bounds of lines longer than `layout::LONG_LINE`, by start offset.
    long_bounds: HashMap<u64, (u64, u64)>,
}

/// Geometry derived from the view size and what is known about the file.
struct Geo {
    w: f64,
    h: f64,
    lines: u64,
    gutter_w: f64,
    text_x: f64,
    content_h: f64,
    max_y: f64,
    content_w: f64,
    max_x: f64,
}

pub struct Ivars {
    st: RefCell<State>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements and TextView does not
    // implement Drop.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SudopenTextView"]
    #[ivars = Ivars]
    pub struct TextView;

    impl TextView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            self.draw(dirty);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, ev: &NSEvent) {
            let line_h = self.ivars().st.borrow().line_h;
            let (mut dx, mut dy) = (ev.scrollingDeltaX(), ev.scrollingDeltaY());
            if !ev.hasPreciseScrollingDeltas() {
                dx *= line_h * 2.0;
                dy *= line_h * 2.0;
            }
            // Lock to the dominant axis so vertical swipes do not drift sideways.
            if dy.abs() > dx.abs() * 2.0 {
                dx = 0.0;
            } else if dx.abs() > dy.abs() * 2.0 {
                dy = 0.0;
            }
            self.scroll_by(-dx, -dy);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, ev: &NSEvent) {
            if !self.handle_key(ev) {
                unsafe { msg_send![super(self), keyDown: ev] }
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, ev: &NSEvent) {
            self.on_mouse_down(ev);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, ev: &NSEvent) {
            self.on_mouse_dragged(ev);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _ev: &NSEvent) {
            let was_knob = {
                let mut st = self.ivars().st.borrow_mut();
                let was_knob = matches!(st.drag, Drag::VKnob { .. } | Drag::HKnob { .. });
                st.drag = Drag::None;
                if was_knob {
                    st.scroller_until = Some(Instant::now() + SCROLLER_SHOW);
                }
                was_knob
            };
            if was_knob {
                self.schedule_scroller_hide();
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            self.copy_selection();
        }

        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            let mut st = self.ivars().st.borrow_mut();
            let len = st.doc.bytes().len() as u64;
            st.sel = Some((0, len));
            drop(st);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let b = self.bounds();
            let r = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(b.size.width - SCROLLER_HIT, b.size.height));
            self.addCursorRect_cursor(r, &NSCursor::IBeamCursor());
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn appearance_changed(&self) {
            let dark = is_dark(&self.effectiveAppearance());
            let mut st = self.ivars().st.borrow_mut();
            if st.pal.dark != dark {
                st.pal = palette(dark);
                st.base_attrs = attrs(&st.font, &st.pal.fg);
                st.gutter_attrs = attrs(&st.font, &st.pal.gutter_fg);
                st.cache.clear();
                if let Some(w) = self.window() {
                    w.setBackgroundColor(Some(&st.pal.bg));
                }
                drop(st);
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(sudopenTick:))]
        fn tick(&self, _arg: Option<&AnyObject>) {
            self.on_tick();
        }

        #[unsafe(method(sudopenHideScroller:))]
        fn hide_scroller(&self, _arg: Option<&AnyObject>) {
            self.setNeedsDisplay(true);
        }
    }
);

impl TextView {
    pub fn new(mtm: MainThreadMarker, doc: Document, frame: NSRect, fp: Option<FirstPaint>) -> Retained<Self> {
        let font = NSFont::monospacedSystemFontOfSize_weight(FONT_SIZE, unsafe { NSFontWeightRegular });
        let probe = NSString::from_str("0000000000");
        let fg_probe = attrs(&font, &NSColor::textColor());
        let char_w = unsafe { probe.sizeWithAttributes(Some(&fg_probe)) }.width / 10.0;
        let font_h = font.ascender() - font.descender() + font.leading();
        let line_h = (font_h * 1.25).round();
        let text_dy = ((line_h - font_h) / 2.0).floor();

        let dark = is_dark(&NSApplication::sharedApplication(mtm).effectiveAppearance());
        let pal = palette(dark);
        let progress = doc.progress();
        let busy = doc.is_busy();
        let st = State {
            base_attrs: attrs(&font, &pal.fg),
            gutter_attrs: attrs(&font, &pal.gutter_fg),
            doc,
            font,
            char_w,
            line_h,
            text_dy,
            scroll_x: 0.0,
            scroll_y: 0.0,
            pal,
            cache: BlockCache::new(256),
            colors: HashMap::new(),
            sel: None,
            drag: Drag::None,
            scroller_until: None,
            widest_cols: progress.max_line_len,
            progress,
            exact_blocks_seen: 0,
            first_paint: fp,
            pending_goto: None,
            long_bounds: HashMap::new(),
        };
        let this = Self::alloc(mtm).set_ivars(Ivars { st: RefCell::new(st) });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        if busy {
            this.schedule(sel!(sudopenTick:), 0.1);
        }
        this
    }

    /// Adds a `--timing` phase mark if the first frame is not drawn yet.
    pub fn mark(&self, phase: &'static str) {
        if let Some(fp) = self.ivars().st.borrow_mut().first_paint.as_mut() {
            fp.mark(phase);
        }
    }

    /// Scrolls to `line` (1-based) and selects it. Waits for the index if
    /// that part of a big file is not indexed yet.
    pub fn goto_line(&self, line: u64) {
        let mut st = self.ivars().st.borrow_mut();
        if !st.progress.done && line > st.progress.indexed_lines {
            st.pending_goto = Some(line);
            return;
        }
        self.apply_goto(&mut st, line);
    }

    fn apply_goto(&self, st: &mut State, line: u64) {
        st.pending_goto = None;
        let n = line.saturating_sub(1).min(st.progress.lines.saturating_sub(1));
        let (start, _) = st.doc.line_start(n, st.progress.lines);
        let bytes = st.doc.bytes();
        let (_, next) = line_bounds(bytes, start);
        st.sel = Some((start, next.min(bytes.len() as u64)));
        let h = self.bounds().size.height;
        st.scroll_y = (PAD_TOP + n as f64 * st.line_h - h / 3.0).max(0.0);
        st.scroll_x = 0.0;
        self.setNeedsDisplay(true);
    }

    pub fn shared_source(&self) -> std::sync::Arc<sudopen_core::source::Source> {
        self.ivars().st.borrow().doc.shared_source()
    }

    pub fn cancel_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.ivars().st.borrow().doc.cancel_flag()
    }

    pub fn path(&self) -> std::path::PathBuf {
        self.ivars().st.borrow().doc.path().to_path_buf()
    }

    /// Hands over the pending `--timing` report, for when another view is
    /// shown before this one ever draws.
    pub fn take_first_paint(&self) -> Option<FirstPaint> {
        self.ivars().st.borrow_mut().first_paint.take()
    }

    pub fn background(&self) -> Retained<NSColor> {
        self.ivars().st.borrow().pal.bg.clone()
    }

    /// Text for the window subtitle: language, size, line count.
    pub fn subtitle(&self) -> String {
        let st = self.ivars().st.borrow();
        subtitle(&st.doc, &st.progress)
    }

    fn geo(&self, st: &State) -> Geo {
        let b = self.bounds();
        let (w, h) = (b.size.width, b.size.height);
        let lines = st.progress.lines.max(1);
        let digits = (lines.ilog10() + 1).max(3) as f64;
        let gutter_w = ((digits + 2.0) * st.char_w).round();
        let text_x = gutter_w + PAD_TEXT;
        let content_h = PAD_TOP + lines as f64 * st.line_h + PAD_BOTTOM;
        let content_w = text_x + st.widest_cols.max(st.progress.max_line_len) as f64 * st.char_w + 40.0;
        Geo {
            w,
            h,
            lines,
            gutter_w,
            text_x,
            content_h,
            max_y: (content_h - h).max(0.0),
            content_w,
            max_x: (content_w - w).max(0.0),
        }
    }

    fn draw(&self, dirty: NSRect) {
        let mut guard = self.ivars().st.borrow_mut();
        if let Some(fp) = guard.first_paint.as_mut() {
            fp.mark("to-draw");
        }
        let g = self.geo(&guard);
        let st = &mut *guard;
        st.scroll_y = st.scroll_y.clamp(0.0, g.max_y);
        st.scroll_x = st.scroll_x.clamp(0.0, g.max_x);

        st.pal.bg.setFill();
        NSBezierPath::fillRect(dirty);

        let theme = if st.pal.dark { &highlight::themes().dark } else { &highlight::themes().light };
        let bytes = st.doc.bytes();
        let len = bytes.len() as u64;
        let first = ((st.scroll_y - PAD_TOP) / st.line_h).floor().max(0.0) as u64;
        let last = (((st.scroll_y + g.h - PAD_TOP) / st.line_h).ceil().max(0.0) as u64).min(g.lines);
        let col0 = (st.scroll_x / st.char_w).floor() as u32;
        let col1 = col0 + ((g.w - g.text_x) / st.char_w).ceil() as u32 + 2;
        let (mut off, exact) = st.doc.line_start(first, g.lines);
        let sel = st.sel.map(|(a, b)| (a.min(b), a.max(b))).filter(|(a, b)| a != b);

        let mut block: Option<(u64, Arc<HlBlock>)> = None;
        let mut n = first;
        let mut drawn_to = first;
        while n < last && off <= len {
            let (line, next) = visible_line(&mut st.long_bounds, bytes, off, col1, n + 1 < last);
            let end = off + line.len() as u64;
            let y = PAD_TOP + n as f64 * st.line_h - st.scroll_y;

            if line.len() <= layout::LONG_LINE {
                let tabs = memchr::memchr_iter(b'\t', line).count() as u64;
                st.widest_cols = st.widest_cols.max(line.len() as u64 + tabs * (layout::TAB as u64 - 1));
            }

            if let Some((s, e)) = sel
                && e > off
                && s <= end
            {
                let cs = if s <= off { 0 } else { layout::col_at_byte(line, (s - off) as usize) };
                let ce = if e > end {
                    layout::col_at_byte(line, line.len()) + 1
                } else {
                    layout::col_at_byte(line, (e - off) as usize)
                };
                if ce > cs {
                    st.pal.selection.setFill();
                    NSBezierPath::fillRect(NSRect::new(
                        NSPoint::new(g.text_x + cs as f64 * st.char_w - st.scroll_x, y),
                        NSSize::new((ce - cs) as f64 * st.char_w, st.line_h),
                    ));
                }
            }

            let spans: &[highlight::Span] = if exact && st.doc.is_highlighted() {
                let b = n / BLOCK;
                if block.as_ref().is_none_or(|(k, _)| *k != b) {
                    block = st.doc.highlighted_block(b, theme, &mut st.cache).map(|blk| (b, blk));
                }
                match &block {
                    Some((_, blk)) => blk.lines.get((n % BLOCK) as usize).map_or(&[], |v| v.as_slice()),
                    None => &[],
                }
            } else {
                &[]
            };

            let vis = layout::layout_visible(line, spans, col0, col1);
            if !vis.text.is_empty() {
                let ns = NSString::from_str(&vis.text);
                let s = unsafe {
                    NSMutableAttributedString::initWithString_attributes(
                        NSMutableAttributedString::alloc(),
                        &ns,
                        Some(&st.base_attrs),
                    )
                };
                for run in vis.runs.iter().filter(|r| r.color != DEFAULT_COLOR) {
                    let color = st.colors.entry(run.color).or_insert_with(|| ns_color(run.color));
                    unsafe {
                        s.addAttribute_value_range(
                            NSForegroundColorAttributeName,
                            color,
                            NSRange::new(run.start as usize, run.len as usize),
                        )
                    };
                }
                s.drawAtPoint(NSPoint::new(
                    g.text_x + vis.first_col as f64 * st.char_w - st.scroll_x,
                    y + st.text_dy,
                ));
            }
            drawn_to = n + 1;
            let Some(next) = next else { break };
            off = next;
            n += 1;
        }

        // Gutter goes on top so horizontally scrolled text slides under it.
        st.pal.bg.setFill();
        NSBezierPath::fillRect(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(g.gutter_w + PAD_TEXT / 2.0, g.h)));
        if exact {
            for n in first..drawn_to {
                let label = NSString::from_str(&(n + 1).to_string());
                let digits = (n + 1).ilog10() as f64 + 1.0;
                let x = g.gutter_w - (digits + 1.0) * st.char_w;
                let y = PAD_TOP + n as f64 * st.line_h - st.scroll_y + st.text_dy;
                unsafe { label.drawAtPoint_withAttributes(NSPoint::new(x, y), Some(&st.gutter_attrs)) };
            }
        }

        let show = st.scroller_until.is_some_and(|t| Instant::now() < t)
            || matches!(st.drag, Drag::VKnob { .. } | Drag::HKnob { .. });
        if show {
            st.pal.knob.setFill();
            if g.max_y > 0.0 {
                let (ky, kh) = v_knob(&g, st.scroll_y);
                let r = NSRect::new(NSPoint::new(g.w - 10.0, ky), NSSize::new(7.0, kh));
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 3.5, 3.5).fill();
            }
            if g.max_x > 0.0 {
                let (kx, kw) = h_knob(&g, st.scroll_x);
                let r = NSRect::new(NSPoint::new(kx, g.h - 10.0), NSSize::new(kw, 7.0));
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 3.5, 3.5).fill();
            }
        }

        if let Some(fp) = st.first_paint.take() {
            fp.report();
        }
    }

    fn scroll_by(&self, dx: f64, dy: f64) {
        {
            let mut st = self.ivars().st.borrow_mut();
            let g = self.geo(&st);
            st.scroll_x = (st.scroll_x + dx).clamp(0.0, g.max_x);
            st.scroll_y = (st.scroll_y + dy).clamp(0.0, g.max_y);
            st.scroller_until = Some(Instant::now() + SCROLLER_SHOW);
        }
        self.schedule_scroller_hide();
        self.setNeedsDisplay(true);
    }

    fn scroll_to(&self, x: Option<f64>, y: Option<f64>) {
        let st = self.ivars().st.borrow();
        let (cx, cy) = (st.scroll_x, st.scroll_y);
        drop(st);
        self.scroll_by(x.map_or(0.0, |x| x - cx), y.map_or(0.0, |y| y - cy));
    }

    fn schedule_scroller_hide(&self) {
        unsafe {
            let _: () = msg_send![
                NSObject::class(),
                cancelPreviousPerformRequestsWithTarget: self,
                selector: sel!(sudopenHideScroller:),
                object: None::<&AnyObject>
            ];
        }
        self.schedule(sel!(sudopenHideScroller:), SCROLLER_SHOW.as_secs_f64() + 0.05);
    }

    fn schedule(&self, sel: objc2::runtime::Sel, delay: f64) {
        unsafe {
            let _: () = msg_send![self, performSelector: sel, withObject: None::<&AnyObject>, afterDelay: delay];
        }
    }

    fn handle_key(&self, ev: &NSEvent) -> bool {
        let mods = ev.modifierFlags();
        let cmd = mods.contains(NSEventModifierFlags::Command);
        let shift = mods.contains(NSEventModifierFlags::Shift);
        let st = self.ivars().st.borrow();
        let g = self.geo(&st);
        let (line_h, char_w) = (st.line_h, st.char_w);
        drop(st);
        let page = (g.h - line_h * 2.0).max(line_h);
        match ev.keyCode() {
            126 if cmd => self.scroll_to(None, Some(0.0)),
            125 if cmd => self.scroll_to(None, Some(g.max_y)),
            123 if cmd => self.scroll_to(Some(0.0), None),
            124 if cmd => self.scroll_to(Some(g.max_x), None),
            126 => self.scroll_by(0.0, -line_h),
            125 => self.scroll_by(0.0, line_h),
            123 => self.scroll_by(-char_w * 4.0, 0.0),
            124 => self.scroll_by(char_w * 4.0, 0.0),
            116 => self.scroll_by(0.0, -page),
            121 => self.scroll_by(0.0, page),
            115 => self.scroll_to(Some(0.0), Some(0.0)),
            119 => self.scroll_to(None, Some(g.max_y)),
            49 => self.scroll_by(0.0, if shift { -page } else { page }),
            53 => {
                self.ivars().st.borrow_mut().sel = None;
                self.setNeedsDisplay(true);
            }
            _ => return false,
        }
        true
    }

    /// Absolute byte offset under `p` (view coordinates).
    fn hit(&self, st: &State, g: &Geo, p: NSPoint) -> u64 {
        let bytes = st.doc.bytes();
        let n = ((p.y + st.scroll_y - PAD_TOP) / st.line_h).floor();
        if n < 0.0 {
            return 0;
        }
        if n as u64 >= g.lines {
            return bytes.len() as u64;
        }
        let (off, _) = st.doc.line_start(n as u64, g.lines);
        let (end, _) = line_bounds(bytes, off);
        let x = (p.x - g.text_x + st.scroll_x) / st.char_w;
        off + layout::byte_at_col(&bytes[off as usize..end as usize], x) as u64
    }

    fn on_mouse_down(&self, ev: &NSEvent) {
        let p = self.convertPoint_fromView(ev.locationInWindow(), None);
        let mut st = self.ivars().st.borrow_mut();
        let g = self.geo(&st);

        if g.max_y > 0.0 && p.x >= g.w - SCROLLER_HIT {
            let (ky, kh) = v_knob(&g, st.scroll_y);
            let grab = if p.y >= ky && p.y <= ky + kh { p.y - ky } else { kh / 2.0 };
            st.drag = Drag::VKnob { grab };
            drop(st);
            self.drag_knob(p);
            return;
        }
        if g.max_x > 0.0 && p.y >= g.h - SCROLLER_HIT {
            let (kx, kw) = h_knob(&g, st.scroll_x);
            let grab = if p.x >= kx && p.x <= kx + kw { p.x - kx } else { kw / 2.0 };
            st.drag = Drag::HKnob { grab };
            drop(st);
            self.drag_knob(p);
            return;
        }

        let pos = self.hit(&st, &g, p);
        let gran = match ev.clickCount() {
            2 => Gran::Word,
            n if n >= 3 => Gran::Line,
            _ => Gran::Char,
        };
        let range = unit_at(st.doc.bytes(), pos, gran);
        let extend = ev.modifierFlags().contains(NSEventModifierFlags::Shift);
        match (extend, st.sel) {
            (true, Some((anchor, _))) => {
                st.sel = Some((anchor, pos));
                st.drag = Drag::Select { gran: Gran::Char, origin: (anchor, anchor) };
            }
            _ => {
                st.sel = Some(range);
                st.drag = Drag::Select { gran, origin: range };
            }
        }
        drop(st);
        self.setNeedsDisplay(true);
    }

    fn on_mouse_dragged(&self, ev: &NSEvent) {
        let p = self.convertPoint_fromView(ev.locationInWindow(), None);
        let drag = self.ivars().st.borrow().drag;
        match drag {
            Drag::VKnob { .. } | Drag::HKnob { .. } => self.drag_knob(p),
            Drag::Select { gran, origin } => {
                let (line_h, char_w) = {
                    let mut st = self.ivars().st.borrow_mut();
                    let g = self.geo(&st);
                    let pos = self.hit(&st, &g, p);
                    let (u0, u1) = unit_at(st.doc.bytes(), pos, gran);
                    st.sel = Some(if u0 < origin.0 { (origin.1, u0) } else { (origin.0, u1.max(origin.1)) });
                    (st.line_h, st.char_w)
                };
                // Crude autoscroll while dragging outside the view.
                let b = self.bounds();
                let dy = if p.y < 0.0 { p.y.max(-line_h * 4.0) } else if p.y > b.size.height { (p.y - b.size.height).min(line_h * 4.0) } else { 0.0 };
                let dx = if p.x < 0.0 { -char_w * 2.0 } else if p.x > b.size.width { char_w * 2.0 } else { 0.0 };
                if dx != 0.0 || dy != 0.0 {
                    self.scroll_by(dx, dy);
                } else {
                    self.setNeedsDisplay(true);
                }
            }
            Drag::None => {}
        }
    }

    fn drag_knob(&self, p: NSPoint) {
        {
            let mut st = self.ivars().st.borrow_mut();
            let g = self.geo(&st);
            match st.drag {
                Drag::VKnob { grab } => {
                    let (_, kh) = v_knob(&g, st.scroll_y);
                    let track = (g.h - 4.0 - kh).max(1.0);
                    st.scroll_y = ((p.y - grab - 2.0) / track * g.max_y).clamp(0.0, g.max_y);
                }
                Drag::HKnob { grab } => {
                    let (_, kw) = h_knob(&g, st.scroll_x);
                    let track = (g.w - 4.0 - kw).max(1.0);
                    st.scroll_x = ((p.x - grab - 2.0) / track * g.max_x).clamp(0.0, g.max_x);
                }
                _ => {}
            }
        }
        self.setNeedsDisplay(true);
    }

    fn copy_selection(&self) {
        let st = self.ivars().st.borrow();
        let Some((a, b)) = st.sel else { return };
        let (s, e) = (a.min(b), a.max(b));
        if s == e {
            return;
        }
        if e - s > MAX_COPY {
            NSBeep();
            return;
        }
        let text = String::from_utf8_lossy(&st.doc.bytes()[s as usize..e as usize]);
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        pb.setString_forType(&NSString::from_str(&text), unsafe { NSPasteboardTypeString });
    }

    fn on_tick(&self) {
        let Some(window) = self.window() else { return };
        let mut st = self.ivars().st.borrow_mut();
        let p = st.doc.progress();
        let mut dirty = p != st.progress;
        let finished_index = p.done && !st.progress.done;
        st.progress = p;
        let eb = st.doc.exact_blocks();
        if eb != st.exact_blocks_seen {
            st.exact_blocks_seen = eb;
            dirty |= st.cache.drop_stale(eb);
        }
        if let Some(line) = st.pending_goto
            && (p.done || line <= p.indexed_lines)
        {
            self.apply_goto(&mut st, line);
        }
        let busy = st.doc.is_busy();
        if finished_index {
            window.setSubtitle(&NSString::from_str(&subtitle(&st.doc, &st.progress)));
        }
        drop(st);
        if dirty {
            self.setNeedsDisplay(true);
        }
        if busy {
            self.schedule(sel!(sudopenTick:), 0.1);
        } else {
            // One last pass so blocks highlighted just before the end get redone.
            let mut st = self.ivars().st.borrow_mut();
            let eb = st.doc.exact_blocks();
            if st.cache.drop_stale(eb) {
                drop(st);
                self.setNeedsDisplay(true);
            }
        }
    }
}

/// The part of the line at `start` worth laying out, and where the next line
/// starts. Lines longer than `layout::LONG_LINE` are only read as far as the
/// view needs unless the next line is needed too (`need_next`); their full
/// bounds are remembered, so a 100 MB single-line file is never rescanned
/// on every frame.
fn visible_line<'a>(
    cache: &mut HashMap<u64, (u64, u64)>,
    bytes: &'a [u8],
    start: u64,
    col1: u32,
    need_next: bool,
) -> (&'a [u8], Option<u64>) {
    let s = start as usize;
    let probe_end = (s + layout::LONG_LINE).min(bytes.len());
    let bounds = if probe_end == bytes.len() || memchr::memchr(b'\n', &bytes[s..probe_end]).is_some() {
        Some(line_bounds(bytes, start))
    } else if let Some(&b) = cache.get(&start) {
        Some(b)
    } else if need_next {
        let b = line_bounds(bytes, start);
        cache.insert(start, b);
        Some(b)
    } else {
        None
    };
    match bounds {
        Some((end, next)) => (&bytes[s..end as usize], Some(next)),
        None => {
            let want = (s + (col1 as usize + 1024).max(layout::LONG_LINE + 1)).min(bytes.len());
            let e = memchr::memchr(b'\n', &bytes[s..want]).map_or(want, |i| s + i);
            (&bytes[s..e], None)
        }
    }
}

fn v_knob(g: &Geo, scroll_y: f64) -> (f64, f64) {
    let kh = (g.h * g.h / g.content_h).clamp(32.0_f64.min(g.h - 4.0), (g.h - 4.0).max(1.0));
    let ky = 2.0 + scroll_y / g.max_y.max(1.0) * (g.h - 4.0 - kh);
    (ky, kh)
}

fn h_knob(g: &Geo, scroll_x: f64) -> (f64, f64) {
    let kw = (g.w * g.w / g.content_w).clamp(32.0_f64.min(g.w - 4.0), (g.w - 4.0).max(1.0));
    let kx = 2.0 + scroll_x / g.max_x.max(1.0) * (g.w - 4.0 - kw);
    (kx, kw)
}

/// Selection unit around byte `pos`: the position itself, the word, or the
/// whole line including its newline.
fn unit_at(bytes: &[u8], pos: u64, gran: Gran) -> (u64, u64) {
    if let Gran::Char = gran {
        return (pos, pos);
    }
    let p = (pos as usize).min(bytes.len());
    let start = memrchr(b'\n', &bytes[..p]).map_or(0, |i| i + 1) as u64;
    let (end, next) = line_bounds(bytes, start);
    match gran {
        Gran::Line => (start, next.min(bytes.len() as u64)),
        _ => {
            let (a, b) = layout::word_at(&bytes[start as usize..end as usize], (pos - start) as usize);
            (start + a as u64, start + b as u64)
        }
    }
}

fn subtitle(doc: &Document, p: &Progress) -> String {
    let mut parts = vec![
        if doc.binary { "Binary".to_string() } else { doc.syntax_name().unwrap_or("Plain Text").to_string() },
        sudopen_core::human_size(doc.source().file_len()),
    ];
    if p.done {
        parts.push(format!("{} {}", group_digits(p.lines), if p.lines == 1 { "line" } else { "lines" }));
    } else {
        parts.push("indexing…".to_string());
    }
    let enc = doc.source().encoding;
    if enc != sudopen_core::source::Encoding::Utf8 {
        parts.push(enc.label().to_string());
    }
    parts.join(" · ")
}

fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn is_dark(appearance: &NSAppearance) -> bool {
    let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    let names = NSArray::from_slice(&[aqua, dark]);
    appearance.bestMatchFromAppearancesWithNames(&names).is_some_and(|n| &*n == dark)
}

fn palette(dark: bool) -> Palette {
    let t = if dark { &highlight::themes().dark } else { &highlight::themes().light };
    let s = &t.settings;
    let pack = |c: Option<sudopen_core::syntect::highlighting::Color>, fallback: u32| c.map_or(fallback, highlight::pack);
    let fg = pack(s.foreground, if dark { 0xDCDFE4FF } else { 0x24292EFF });
    Palette {
        dark,
        bg: ns_color(pack(s.background, if dark { 0x282C34FF } else { 0xFFFFFFFF })),
        fg: ns_color(fg),
        gutter_fg: ns_color((fg & 0xFFFFFF00) | 0x60),
        selection: ns_color(pack(s.selection, if dark { 0x3E4451FF } else { 0xC8E1FFFF })),
        knob: if dark { ns_color(0xFFFFFF66) } else { ns_color(0x00000059) },
    }
}

fn ns_color(c: u32) -> Retained<NSColor> {
    let [r, g, b, a] = c.to_be_bytes();
    NSColor::colorWithSRGBRed_green_blue_alpha(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, a as f64 / 255.0)
}

fn attrs(font: &NSFont, color: &NSColor) -> Retained<Attrs> {
    let keys: [&NSAttributedStringKey; 2] = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let vals: [&AnyObject; 2] = [font, color];
    NSDictionary::from_slices(&keys, &vals)
}
