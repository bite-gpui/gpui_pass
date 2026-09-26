//! Two windows: one you can weigh, one that weighs it.
//!
//! `pass_demo` shows a rate, `pass_ticket` a ledger, `puffin_scopes` a timeline. This is the
//! one that answers *where* the time goes. A **measured** window draws a scene whose every
//! ingredient is a knob, and a **telemetry** window draws what each pass of that scene cost
//! — read from the very `Ledger` the measured window's pipeline fills.
//!
//! The knobs are chosen so each one lands on a different pass:
//!
//! | knob | what it adds | the pass it moves |
//! | --- | --- | --- |
//! | buttons | sibling boxes in one flex row | `evaluate_roots` and `layout_roots` |
//! | paragraphs | glyph runs the shaper has to place | `layout_roots` (shaping) and `paint_roots` (glyphs) |
//! | canvas primitives | quads a `canvas` inserts into the scene | `paint_roots` (the scene build) |
//!
//! Turn one up and watch its bar grow in the other window. The two windows do not share a
//! ledger: the factory below gives each window its own, and the telemetry window reads the
//! *other* one — so its own frames never pollute the measurement, and neither does having
//! focus.
//!
//! # What the numbers are, and are not
//!
//! Everything here is the **passes**: the CPU work a frame is made of. The GPU submit, the
//! swap and the vblank are below the `FramePipeline` seam, in the platform, and this — or any
//! pipeline — never sees them. A 60 fps frame is 16.67 ms; the passes fill a slice and the
//! platform presents the rest. The budget bar is the passes' share, nothing more.
//!
//! This is a real window on the real platform, so its numbers are the real passes for the
//! scene you dial up. They are a `dev` build, though; a release build's are several times
//! smaller, and a real application's scene is larger than any knob here reaches.
//!
//! # Two layers of accounting, and the ceiling between them
//!
//! The ledger's seven phases are the **seam's** granularity, and it cannot go finer: the trait
//! is seven methods, and everything a frame does inside them is invisible to a pipeline. Two of
//! those phases are coarser than their names suggest, which is why the numbers look the way they
//! do:
//!
//! - **`layout_roots` is layout *and prepaint*.** Taffy sizing, every element's `prepaint`, the
//!   text shaper's line layout and the pointer hit test all run inside it. That is why it is the
//!   pass that scales with the node count, and why a base of it is constant: the tree is always
//!   walked, whatever is in it.
//! - **`paint_roots` builds the scene, it does not rasterize it.** It walks the tree again and
//!   appends primitives — quads, glyph runs, paths — to the frame's `Scene`. A flat quad is
//!   nearly free to append, which is why this pass moves so little for boxes; it grows when the
//!   primitives cost something to produce (glyph runs, shadows, gradients, masks).
//! - **`finish_frame` is not idle bookkeeping.** It ages the text system's line-layout cache,
//!   dropping a generation of shaped lines — so it scales with the text a frame drew — and it
//!   finalizes the scene by sorting every primitive bucket by draw order, so it also scales with
//!   the primitives the frame produced, and text contributes a glyph sprite per run. A "trailing
//!   pass" that grows with what you drew is the same lesson as `layout_roots`: the seven names
//!   hide where the work actually is.
//! - **Shaping is cached, so identical text is nearly free.** The text system knits a cache of
//!   shaped lines and reuses them by content, so N copies of *one* paragraph are shaped once and
//!   the rest are lookups — which is why a paragraph count can move `paint_roots` and
//!   `finish_frame` (sprites and their sort) while barely moving the shaping inside `layout_roots`.
//!   Make the paragraphs differ and every one is shaped. The `distinct text` knob flips between
//!   those two worlds.
//! - **The cache is shared by every window, and it is two generations deep.** Each window's
//!   `finish_frame` swaps the current generation into the previous and clears it, so the second
//!   window's roll clears the generation the first would have reused. Two windows, and nothing is
//!   reused across frames either — every paragraph is shaped every frame.
//! - **The expensive half is not here at all.** Turning the `Scene` into GPU commands, uploading
//!   glyph atlases, drawing, and waiting on the swapchain happen below this seam — much of it on
//!   the render thread — so no pipeline, and no profiler scoped to a pipeline, can account for
//!   them.
//!
//! To see *below* a pass you have to instrument inside it, and only the thing doing the work can.
//! This example does that for its own elements: the text canvas times its shaping (which runs in
//! prepaint, inside `layout_roots`) apart from its glyph painting (which runs inside
//! `paint_roots`), and reports both in the telemetry window. That is the shape of any finer
//! granularity — element prepaint and paint, the text system's shape/layout/rasterize — and it is
//! reachable from a consumer, not from the pipeline. Real depth (the engine's own element tree
//! and text system emitting scopes for you) is an engine feature, not a decorator.
//!
//! If the `puffin` feature is on as well, the factory also wraps each window in the scope
//! emitter, so the same frames can be streamed to `puffin_viewer` (see `examples/puffin_scopes.rs`
//! for the in-process reader). Off, the telemetry window's ledger is the whole story.
//!
//! Run: `cargo run --example pass_lab --features demo` (add `,puffin` for scopes)

#![cfg(feature = "demo")]

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    num::NonZeroU32,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, ClickEvent, Context, ElementId, Entity, FontWeight, FramePipeline, Render,
    ScrollHandle, SharedString, StandardImmediatePipeline, Subscription, TextAlign,
    TitlebarOptions, Window, WindowBounds, WindowId, WindowOptions, application, canvas, div, fill,
    point, prelude::*, px, rgb, size,
};
use gpui_pass::{Ledger, PassPipeline, Phase, RatePolicy};

/// The rate the measured window is throttled to, and the budget the panel measures against.
/// 60 divides the display's grid, so the throttle is deliberately not the variable here — the
/// cost of what is drawn is.
const RATE: u32 = 60;

/// The `canvas` lays its primitives out in this many columns, so a knob's count reads as a
/// growing grid rather than an unreadable pile.
const SHAPE_COLUMNS: usize = 24;

const BACKDROP: u32 = 0x0b0f14;
const SURFACE: u32 = 0x151b23;
const EDGE: u32 = 0x30363d;
const INK: u32 = 0xe6edf3;
const MUTED: u32 = 0x8b949e;

/// One paragraph of text — long enough that a few of them cost the shaper something. Every
/// copy is identical, so a frame's text is the same words at a bigger size, not different ones.
const PARAGRAPH: &str = "The pass is the hot counter at the end of the line, where every plate is \
inspected, timed and paced before it leaves the kitchen. A frame goes through the same place: \
evaluated, laid out, painted, and accounted for before it is presented.";

/// The registry of per-window ledgers. The factory below fills it as windows are built, and the
/// telemetry window reads every entry but its own.
type Ledgers = Rc<RefCell<HashMap<WindowId, Rc<RefCell<Ledger>>>>>;

fn fps(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("a rate above zero")
}

/// The interval the pipeline's rate implies — the yardstick the panel draws the passes against.
fn budget() -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(RATE))
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// A colour near `base`, nudged by `index` so a grid of boxes is legible rather than flat.
fn shade(base: u32, index: usize) -> u32 {
    base.wrapping_add((index % 8) as u32 * 0x000404)
}

/// The colour each pass is drawn in, so a bar in one window and a scope in the other read alike.
fn phase_color(phase: Phase) -> u32 {
    match phase {
        Phase::Begin => 0x64748b,
        Phase::Evaluate => 0x38bdf8,
        Phase::Layout => 0xa78bfa,
        Phase::Paint => 0x34d399,
        Phase::Finish => 0xf59e0b,
        Phase::Complete => 0xf472b6,
        Phase::End => 0x94a3b8,
    }
}

/// What the demo's own elements measured *inside* themselves: a leaf's prepaint (which runs
/// during `layout_roots`) and its paint (which runs during `paint_roots`). The ledger accounts for
/// the seven passes and nothing between them; this is the layer below, and only the element that
/// does the work can report it. Single-threaded, on the frame thread.
#[derive(Default)]
struct Probes {
    text_shape: Cell<u64>,
    text_paint: Cell<u64>,
    canvas_paint: Cell<u64>,
    glyphs_total: Cell<u64>,
    glyphs_unique: Cell<u64>,
}

impl Probes {
    fn reset(&self) {
        self.text_shape.set(0);
        self.text_paint.set(0);
        self.canvas_paint.set(0);
        self.glyphs_total.set(0);
        self.glyphs_unique.set(0);
    }
}

fn elapsed_nanos(started: Instant) -> u64 {
    started.elapsed().as_nanos() as u64
}

/// The leaf phases of one measured frame, published beside the pass they belong to.
///
/// These are *sub-slices*, not extra budget: prepaint work runs inside `layout_roots` and paint
/// work inside `paint_roots`, so each is contained by its pass, not added to it. `Stage` writes
/// this at the top of a frame — while the scratch [`Probes`] still hold the previous frame's
/// numbers and the ledger's `last()` is that same frame — so a sub-phase and its parent describe
/// one frame rather than two adjacent ones.
#[derive(Clone, Copy, Default)]
struct Leaf {
    layout: Duration,
    paint: Duration,
    text_shape: Duration,
    text_paint: Duration,
    canvas_paint: Duration,
    glyphs_total: u64,
    glyphs_unique: u64,
}

impl Leaf {
    /// What a pass cost beyond the sub-phases named for it.
    fn unattributed(&self, parent: Duration, parts: &[Duration]) -> Duration {
        parts
            .iter()
            .fold(parent, |rest, part| rest.saturating_sub(*part))
    }
}

/// The knobs, as one plain value. Both windows hold the same `Entity<Settings>` and observe it,
/// so a click in the telemetry window redraws the measured one.
#[derive(Clone)]
struct Settings {
    buttons: usize,
    paragraphs: usize,
    shapes: usize,
    animate: bool,
    distinct_text: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            buttons: 24,
            paragraphs: 2,
            shapes: 480,
            animate: true,
            distinct_text: true,
        }
    }
}

fn pipeline_factory(ledgers: Ledgers) -> impl Fn(WindowId) -> Box<dyn FramePipeline> + 'static {
    move |window_id| {
        // One ledger per window, made once and kept in the registry so the telemetry window can
        // find the measured one.
        let ledger = ledgers
            .borrow_mut()
            .entry(window_id)
            .or_insert_with(|| Ledger::shared(Ledger::DEFAULT_CAPACITY))
            .clone();

        let pipeline =
            PassPipeline::new(StandardImmediatePipeline, RatePolicy::at(fps(RATE)), ledger);

        // Scopes are the same passes in the other shape. Inert without a sink — see the module
        // docs — but free when the feature is off, which is the point.
        #[cfg(feature = "puffin")]
        let pipeline = {
            puffin::set_scopes_on(true);
            gpui_pass::PuffinPipeline::new(pipeline)
        };

        Box::new(pipeline) as Box<dyn FramePipeline>
    }
}

// ---------------------------------------------------------------------------
// The measured window
// ---------------------------------------------------------------------------

/// The window that is being weighed. Its content is a pure function of [`Settings`], and it
/// animates by asking for the next frame only while the `animate` knob is on.
struct Stage {
    settings: Entity<Settings>,
    ledgers: Ledgers,
    probes: Rc<Probes>,
    leaf: Rc<Cell<Leaf>>,
    phase: f32,
    _settings: Subscription,
}

impl Stage {
    fn new(
        settings: Entity<Settings>,
        ledgers: Ledgers,
        probes: Rc<Probes>,
        leaf: Rc<Cell<Leaf>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&settings, |_, _, cx| cx.notify());
        Self {
            settings,
            ledgers,
            probes,
            leaf,
            phase: 0.0,
            _settings: subscription,
        }
    }

    /// Files the previous frame's leaf numbers, paired with the pass totals of the same frame.
    ///
    /// `render` is the evaluate pass — the ledger's `last()` here is the last frame it filed, and
    /// the scratch [`Probes`] still hold that same frame's numbers, because this is the first thing
    /// to touch them since. Publishing here is what keeps a sub-phase and its parent about one
    /// frame instead of two.
    fn publish_leaf(&self, window: &Window) {
        let mine = window.window_handle().window_id();
        let frame = self
            .ledgers
            .borrow()
            .get(&mine)
            .and_then(|ledger| ledger.borrow().last());
        self.leaf.set(Leaf {
            layout: frame.map_or(Duration::ZERO, |frame| frame.phase(Phase::Layout)),
            paint: frame.map_or(Duration::ZERO, |frame| frame.phase(Phase::Paint)),
            text_shape: Duration::from_nanos(self.probes.text_shape.get()),
            text_paint: Duration::from_nanos(self.probes.text_paint.get()),
            canvas_paint: Duration::from_nanos(self.probes.canvas_paint.get()),
            glyphs_total: self.probes.glyphs_total.get(),
            glyphs_unique: self.probes.glyphs_unique.get(),
        });
    }
}

impl Render for Stage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.publish_leaf(window);
        // The buckets belong to this frame, so they start empty now that the last one is filed.
        self.probes.reset();

        let settings = self.settings.read(cx).clone();
        if settings.animate {
            // Six radians is a little over one turn; wrapping keeps the number small.
            self.phase = (self.phase + 0.03) % 6.283_185_5;
            window.request_animation_frame();
        }
        let phase = self.phase;

        // Boxes: siblings in one wrapped row. No glyphs, so this is the layout knob.
        let buttons = (0..settings.buttons)
            .map(move |index| {
                let lift = ((phase + index as f32 * 0.4).sin() * 0.5 + 0.5) * 40.0;
                div()
                    .size(px(56.))
                    .rounded_lg()
                    .bg(rgb(shade(0x1f6f8b, index)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.))
                    .text_color(rgb(INK))
                    .opacity(0.4 + lift / 100.0)
                    .child("Btn")
            })
            .collect::<Vec<_>>();

        // Paragraphs, through a canvas that times its own shaping and its own glyph painting —
        // both of which happen *inside* a pass, where the ledger cannot see them. The `distinct
        // text` knob decides whether they share one cached shape or each pay for their own.
        let texts: Vec<SharedString> = (0..settings.paragraphs)
            .map(|index| {
                if settings.distinct_text {
                    SharedString::from(format!("{index:>3} · {PARAGRAPH}"))
                } else {
                    SharedString::from(PARAGRAPH)
                }
            })
            .collect();
        let text = text_probe_canvas(texts, self.probes.clone());

        // Canvas primitives: quads pushed straight into the scene, which is the paint pass.
        let board = shapes_canvas(settings.shapes, self.probes.clone());

        div()
            .size_full()
            .bg(rgb(BACKDROP))
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .child(stage_header(&settings))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1()
                    .children(buttons),
            )
            .child(text)
            .child(board)
    }
}

fn stage_header(settings: &Settings) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(INK))
                .child("MEASURED WINDOW"),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(rgb(MUTED))
                .child(format!(
                    "{} buttons · {} paragraphs · {} canvas primitives · {}",
                    settings.buttons,
                    settings.paragraphs,
                    settings.shapes,
                    if settings.animate {
                        "animating"
                    } else {
                        "still"
                    }
                )),
        )
}

/// Text as a probe. The shaper runs in prepaint (inside the layout pass) and the glyph runs are
/// painted in paint (inside the paint pass); each half is timed into [`Probes`]. This is the
/// granularity the seven-phase ledger cannot reach, because both halves happen *within* a pass —
/// and it is reachable only because this element does the work and can say so itself.
fn text_probe_canvas(texts: Vec<SharedString>, probes: Rc<Probes>) -> impl IntoElement {
    canvas(
        {
            let probes = probes.clone();
            move |bounds, window, _cx| {
                let style = window.text_style();
                let font_size = style.font_size.to_pixels(window.rem_size());
                let line_height = style
                    .line_height
                    .to_pixels(font_size.into(), window.rem_size());
                let started = Instant::now();
                let mut lines = Vec::new();
                for text in &texts {
                    let runs = [style.to_run(text.len())];
                    match window.text_system().shape_text(
                        text.clone(),
                        font_size,
                        &runs,
                        Some(bounds.size.width),
                        None,
                    ) {
                        Ok(shaped) => lines.extend(shaped),
                        Err(error) => {
                            eprintln!("pass_lab: shaping failed: {error}");
                            break;
                        }
                    }
                }
                probes
                    .text_shape
                    .set(probes.text_shape.get() + elapsed_nanos(started));
                (lines, line_height)
            }
        },
        {
            let probes = probes.clone();
            move |bounds, (lines, line_height), window, cx| {
                let started = Instant::now();
                let mut y = bounds.origin.y;
                for line in &lines {
                    if let Err(error) = line.paint(
                        point(bounds.origin.x, y),
                        line_height,
                        TextAlign::Left,
                        Some(bounds),
                        window,
                        cx,
                    ) {
                        eprintln!("pass_lab: glyph paint failed: {error}");
                        break;
                    }
                    y += line_height;
                }
                probes
                    .text_paint
                    .set(probes.text_paint.get() + elapsed_nanos(started));

                // Counting is bookkeeping, not drawing: outside the timed span, but still inside
                // the paint pass the ledger times.
                let mut total = 0_u64;
                let mut unique = HashSet::new();
                for line in &lines {
                    for run in &line.unwrapped_layout.runs {
                        total += run.glyphs.len() as u64;
                        for glyph in &run.glyphs {
                            unique.insert((run.font_id, glyph.id));
                        }
                    }
                }
                probes.glyphs_total.set(total);
                probes.glyphs_unique.set(unique.len() as u64);
            }
        },
    )
    .w_full()
    .flex_1()
    .overflow_hidden()
}

/// Canvas primitives — quads appended straight to the scene. `paint_roots` in one closure, timed.
fn shapes_canvas(shapes: usize, probes: Rc<Probes>) -> impl IntoElement {
    canvas(
        move |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            let started = Instant::now();
            let origin_x = bounds.origin.x.as_f32();
            let origin_y = bounds.origin.y.as_f32();
            let width = bounds.size.width.as_f32();
            let height = bounds.size.height.as_f32();
            let cell = width / SHAPE_COLUMNS as f32;
            if cell > 0.0 {
                for index in 0..shapes {
                    let column = (index % SHAPE_COLUMNS) as f32;
                    let row = (index / SHAPE_COLUMNS) as f32;
                    let x = origin_x + column * cell + 1.0;
                    let y = origin_y + row * cell + 1.0;
                    if y + cell > origin_y + height {
                        break;
                    }
                    let quad = fill(
                        Bounds::new(
                            point(px(x), px(y)),
                            size(px((cell - 2.0).max(0.0)), px((cell - 2.0).max(0.0))),
                        ),
                        rgb(shade(0x38bdf8, index)),
                    );
                    window.paint_quad(quad);
                }
            }
            probes
                .canvas_paint
                .set(probes.canvas_paint.get() + elapsed_nanos(started));
        },
    )
    .flex_1()
    .overflow_hidden()
    .rounded_lg()
    .bg(rgb(SURFACE))
}

// ---------------------------------------------------------------------------
// The telemetry window
// ---------------------------------------------------------------------------

/// A snapshot of the measured ledger, taken once per frame so the borrow is held only for the
/// read.
#[derive(Default)]
struct Reading {
    phases: Vec<(Phase, Duration)>,
    total: Duration,
    deferred_since_previous: usize,
    drawn: usize,
    asks: usize,
    deferral: f64,
    p99_paint: Duration,
    over_budget: usize,
}

impl Reading {
    fn of(ledger: Option<Rc<RefCell<Ledger>>>) -> Self {
        let Some(ledger) = ledger else {
            return Self::default();
        };
        let ledger = ledger.borrow();
        let frame = ledger.last();
        Self {
            phases: frame
                .map(|frame| frame.iter().collect())
                .unwrap_or_default(),
            total: frame.map_or(Duration::ZERO, |frame| frame.total()),
            deferred_since_previous: frame.map_or(0, |frame| frame.deferred_since_previous()),
            drawn: ledger.drawn(),
            asks: ledger.asks(),
            deferral: ledger.deferral_ratio(),
            p99_paint: ledger.phase_percentile(Phase::Paint, 0.99),
            over_budget: ledger.over_budget(budget()),
        }
    }
}

/// The window that weighs the other one. It reads every registered ledger but its own, which is
/// how its own frames stay out of the measurement.
struct Panel {
    settings: Entity<Settings>,
    ledgers: Ledgers,
    leaf: Rc<Cell<Leaf>>,
    scroll: ScrollHandle,
    _settings: Subscription,
}

impl Panel {
    fn new(
        settings: Entity<Settings>,
        ledgers: Ledgers,
        leaf: Rc<Cell<Leaf>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&settings, |_, _, cx| cx.notify());
        Self {
            settings,
            ledgers,
            leaf,
            scroll: ScrollHandle::new(),
            _settings: subscription,
        }
    }

    /// The measured window's ledger: the one entry in the registry that is not this window's.
    fn measured(&self, window: &Window) -> Option<Rc<RefCell<Ledger>>> {
        let mine = window.window_handle().window_id();
        self.ledgers
            .borrow()
            .iter()
            .find(|(id, _)| **id != mine)
            .map(|(_, ledger)| ledger.clone())
    }
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The panel redraws every frame so the reading is live; its own ledger is the one it
        // does not show.
        window.request_animation_frame();

        let reading = Reading::of(self.measured(window));
        let settings = self.settings.read(cx).clone();
        let budget_ms = ms(budget());

        let verdict = if reading.over_budget == 0 {
            (
                format!(
                    "{:.2} of {:.2} ms — inside the budget",
                    ms(reading.total),
                    budget_ms
                ),
                0x34d399,
            )
        } else {
            (
                format!(
                    "{:.2} of {:.2} ms — {} of {} frames over",
                    ms(reading.total),
                    budget_ms,
                    reading.over_budget,
                    reading.drawn
                ),
                0xf59e0b,
            )
        };

        div()
            .id("panel-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .w_full()
                    .bg(rgb(BACKDROP))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(INK))
                                    .child("TELEMETRY · THE OTHER WINDOW"),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(MUTED))
                                    .child(format!(
                                        "{} asks · {} drawn · {:.0}% passed over",
                                        reading.asks,
                                        reading.drawn,
                                        reading.deferral * 100.0
                                    )),
                            ),
                    )
                    .child(phase_bars(&reading))
                    .child(summary_card(&verdict.0, verdict.1))
                    .child(summary_card(
                        &format!(
                            "p99 paint {:.3} ms · this frame stood in for {} passed-over asks",
                            ms(reading.p99_paint),
                            reading.deferred_since_previous
                        ),
                        0x94a3b8,
                    ))
                    .child(leaf_phases(self.leaf.get()))
                    .child(glyph_card(self.leaf.get()))
                    .child(seam_note())
                    .child(knobs(&self.settings, &settings)),
            )
    }
}

/// One line of the leaf decomposition: a pass, or a sub-slice of one indented beneath it.
fn leaf_line(
    label: &str,
    value: Duration,
    parent: Duration,
    indent: bool,
    color: u32,
) -> impl IntoElement {
    let share = if parent.is_zero() {
        0.0
    } else {
        value.as_secs_f64() / parent.as_secs_f64() * 100.0
    };
    let label = if indent {
        format!("  └ {label}")
    } else {
        label.to_owned()
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(150.))
                .text_size(px(10.))
                .text_color(rgb(color))
                .child(label),
        )
        .child(
            div()
                .w(px(64.))
                .text_size(px(10.))
                .text_color(rgb(color))
                .child(format!("{:7.3} ms", ms(value))),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(rgb(MUTED))
                .child(if indent {
                    format!("{share:4.0}% of pass")
                } else {
                    String::new()
                }),
        )
}

/// The layer below the seven phases — and, crucially, how it *fits inside* them. Prepaint work
/// runs in `layout_roots` and paint work in `paint_roots`, so these are sub-slices of those bars,
/// not additional budget; whatever a pass spent outside the sub-phases named for it is the row
/// left over.
fn leaf_phases(leaf: Leaf) -> impl IntoElement {
    let layout_other = leaf.unattributed(leaf.layout, &[leaf.text_shape]);
    let paint_other = leaf.unattributed(leaf.paint, &[leaf.text_paint, leaf.canvas_paint]);

    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(INK))
                .child("WHAT EACH PASS IS MADE OF"),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(rgb(MUTED))
                .child("sub-slices, not extra budget: the seamed passes contain them"),
        )
        .child(leaf_line(
            "layout_roots",
            leaf.layout,
            leaf.layout,
            false,
            phase_color(Phase::Layout),
        ))
        .child(leaf_line(
            "text shape (prepaint)",
            leaf.text_shape,
            leaf.layout,
            true,
            0xa78bfa,
        ))
        .child(leaf_line(
            "everything else",
            layout_other,
            leaf.layout,
            true,
            0x64748b,
        ))
        .child(leaf_line(
            "paint_roots",
            leaf.paint,
            leaf.paint,
            false,
            phase_color(Phase::Paint),
        ))
        .child(leaf_line(
            "text glyphs (paint)",
            leaf.text_paint,
            leaf.paint,
            true,
            0x34d399,
        ))
        .child(leaf_line(
            "canvas quads (paint)",
            leaf.canvas_paint,
            leaf.paint,
            true,
            0x38bdf8,
        ))
        .child(leaf_line(
            "everything else",
            paint_other,
            leaf.paint,
            true,
            0x64748b,
        ))
}

/// A label and a value, for the counters that are not durations.
fn stat_line(label: &str, value: String) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(150.))
                .text_size(px(10.))
                .text_color(rgb(MUTED))
                .child(label.to_owned()),
        )
        .child(div().text_size(px(10.)).text_color(rgb(INK)).child(value))
}

/// How much text the frame actually drew. Total glyphs scale the shaper and the sprite insertion;
/// distinct glyphs are what the atlas cares about — and the atlas is below the seam.
fn glyph_card(leaf: Leaf) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(INK))
                .child("GLYPHS THE FRAME DREW"),
        )
        .child(div().text_size(px(10.)).text_color(rgb(MUTED)).child(
            "total moves the shaper and the sprite insertion; distinct is the atlas — below \
                     the seam, so not in any bar here",
        ))
        .child(stat_line("painted (total)", leaf.glyphs_total.to_string()))
        .child(stat_line("distinct glyphs", leaf.glyphs_unique.to_string()))
}

/// Why one of the seven bars grows with text even though it looks like bookkeeping: `finish_frame`
/// ages the text system's line-layout cache and finalizes the scene, so it is neither free nor
/// constant. It has no sub-probe here because that work is engine internals, not a leaf element —
/// which is exactly the ceiling the pass list has.
fn seam_note() -> impl IntoElement {
    div().text_size(px(10.)).text_color(rgb(MUTED)).child(
        "finish_frame is not idle: it ages the text system's line-layout cache and sorts the \
         scene's primitives, so it grows with the text drawn.",
    )
}

/// One bar per pass, scaled to the frame's own total so the composition is readable at a glance.
fn phase_bars(reading: &Reading) -> impl IntoElement {
    const TRACK: f32 = 168.0;
    let largest = reading
        .phases
        .iter()
        .map(|(_, cost)| cost.as_secs_f64())
        .fold(0.0_f64, f64::max)
        .max(1e-6);

    let bars = reading
        .phases
        .iter()
        .map(|(phase, cost)| {
            let fraction = (cost.as_secs_f64() / largest) as f32;
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(92.))
                        .text_size(px(10.))
                        .text_color(rgb(MUTED))
                        .child(phase.name()),
                )
                .child(
                    div()
                        .w(px(TRACK))
                        .h(px(8.))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(rgb(EDGE))
                        .child(
                            div()
                                .h_full()
                                .w(px((fraction * TRACK).max(1.0)))
                                .bg(rgb(phase_color(*phase))),
                        ),
                )
                .child(
                    div()
                        .w(px(66.))
                        .text_size(px(10.))
                        .text_color(rgb(INK))
                        .child(format!("{:6.3} ms", ms(*cost))),
                )
        })
        .collect::<Vec<_>>();

    div().flex().flex_col().gap_1().children(bars)
}

fn summary_card(line: &str, color: u32) -> impl IntoElement {
    div()
        .px_3()
        .py_2()
        .rounded_lg()
        .bg(rgb(SURFACE))
        .text_size(px(11.))
        .text_color(rgb(color))
        .child(line.to_owned())
}

fn knobs(settings: &Entity<Settings>, current: &Settings) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(INK))
                .child("WHAT THE OTHER WINDOW DRAWS"),
        )
        .child(div().text_size(px(10.)).text_color(rgb(MUTED)).child(
            "boxes move evaluate + layout · paragraphs move layout + paint · primitives move paint",
        ))
        .child(stepper(settings, "buttons", current.buttons, 8, 240, |s| {
            &mut s.buttons
        }))
        .child(stepper(
            settings,
            "paragraphs",
            current.paragraphs,
            2,
            40,
            |s| &mut s.paragraphs,
        ))
        .child(stepper(
            settings,
            "primitives",
            current.shapes,
            240,
            4800,
            |s| &mut s.shapes,
        ))
        .child(toggle_row(
            settings,
            "animate",
            current.animate,
            "off: the window draws only when a knob moves",
            |s| &mut s.animate,
        ))
        .child(toggle_row(
            settings,
            "distinct text",
            current.distinct_text,
            "off: one cached shape answers every paragraph",
            |s| &mut s.distinct_text,
        ))
}

fn stepper(
    settings: &Entity<Settings>,
    label: &'static str,
    value: usize,
    step: usize,
    max: usize,
    field: fn(&mut Settings) -> &mut usize,
) -> impl IntoElement {
    let down = {
        let settings = settings.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            settings.update(cx, |settings, cx| {
                let value = field(settings);
                *value = value.saturating_sub(step);
                cx.notify();
            });
        }
    };
    let up = {
        let settings = settings.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            settings.update(cx, |settings, cx| {
                let value = field(settings);
                *value = (*value + step).min(max);
                cx.notify();
            });
        }
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(92.))
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(chip(format!("{label}-down"), "-", down))
        .child(
            div()
                .w(px(52.))
                .text_size(px(12.))
                .text_color(rgb(INK))
                .child(value.to_string()),
        )
        .child(chip(format!("{label}-up"), "+", up))
}

fn toggle_row(
    settings: &Entity<Settings>,
    label: &'static str,
    value: bool,
    note: &'static str,
    field: fn(&mut Settings) -> &mut bool,
) -> impl IntoElement {
    let toggle = {
        let settings = settings.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            settings.update(cx, |settings, cx| {
                let slot = field(settings);
                *slot = !*slot;
                cx.notify();
            });
        }
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(110.))
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(chip(label, if value { "on" } else { "off" }, toggle))
        .child(div().text_size(px(10.)).text_color(rgb(MUTED)).child(note))
}

fn chip(
    id: impl Into<ElementId>,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .px_2()
        .py_1()
        .rounded_lg()
        .bg(rgb(EDGE))
        .text_size(px(12.))
        .text_color(rgb(INK))
        .child(label)
        // An id is what makes the element stateful, and `on_click` is a stateful element's.
        .id(id)
        .on_click(on_click)
}

// ---------------------------------------------------------------------------

fn stage_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(60.), px(80.)),
            size(px(620.), px(680.)),
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Measured".into()),
            ..Default::default()
        }),
        // The engine caps an unfocused window at 30 fps in the frame *source*, before any
        // pipeline is consulted. The measured window is often the unfocused one, and a 30 fps
        // source would be a throttle of its own, which is not what this demo is about.
        inactive_frame_interval: None,
        ..Default::default()
    }
}

fn panel_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(720.), px(80.)),
            size(px(520.), px(680.)),
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Telemetry".into()),
            ..Default::default()
        }),
        inactive_frame_interval: None,
        ..Default::default()
    }
}

fn main() {
    let ledgers: Ledgers = Rc::new(RefCell::new(HashMap::new()));
    let probes = Rc::new(Probes::default());
    let leaf = Rc::new(Cell::new(Leaf::default()));

    application()
        .with_frame_pipeline(pipeline_factory(ledgers.clone()))
        .run(move |cx: &mut App| {
            let settings = cx.new(|_| Settings::default());

            // The measured window opens first, so its ledger is registered before the panel
            // looks for it. Either order works, but this one shows a reading on frame one.
            cx.open_window(stage_options(), {
                let settings = settings.clone();
                let ledgers = ledgers.clone();
                let probes = probes.clone();
                let leaf = leaf.clone();
                move |_window, app| {
                    app.new(|cx| {
                        Stage::new(
                            settings.clone(),
                            ledgers.clone(),
                            probes.clone(),
                            leaf.clone(),
                            cx,
                        )
                    })
                }
            })
            .expect("the platform opened the measured window");

            cx.open_window(panel_options(), {
                let settings = settings.clone();
                let ledgers = ledgers.clone();
                let leaf = leaf.clone();
                move |_window, app| {
                    app.new(|cx| Panel::new(settings.clone(), ledgers.clone(), leaf.clone(), cx))
                }
            })
            .expect("the platform opened the telemetry window");

            cx.activate(true);
        });
}
