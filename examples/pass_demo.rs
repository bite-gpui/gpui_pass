//! A frame-stepped animation at a rate the refresh period does not divide.
//!
//! `cargo run --example throttle_demo --features demo            # the throttle
//! cargo run --example throttle_demo --features demo -- naive    # the shipped cap
//!
//! # Why 24 fps and not 60
//!
//! At 60 fps on a 60Hz panel there is nothing to see and nothing to show: the asks
//! arrive once per refresh and both rules admit every one of them. 24 fps on the same
//! panel is the interesting case, and the reason is arithmetic. 24 frames a second is
//! 60/24 = 2.5 refreshes a frame, which is not a whole number.
//!
//! `ThrottledPipeline` decides from one number, `now - last_frame`, so the only cadence
//! it can settle into is a *fixed* number of refreshes a frame. Its quarter-interval
//! tolerance locks to two refreshes — 30 fps, a quarter fast; with no tolerance it locks
//! to three — 20 fps, a sixth slow. Neither is 24, and no tolerance setting produces the
//! alternating three-and-two that is, because the half-refresh has nowhere to live.
//!
//! A carried schedule is that place. It keeps the half-refresh across frames, so
//! `RatePolicy::at(24)` with one interval of slack (`allow_burst(2)`) delivers the 3:2
//! pulldown: holds of three refreshes and two, alternating, exactly 24 frames a second.
//! `tests/throttling.rs` measures both of those.
//!
//! # What the two runs show
//!
//! The animation advances one step for every frame that is drawn, which is what a
//! frame-sequence animation is — a GIF, a ticker, a sprite. The strip is two seconds of
//! the rate asked for: the bright cell is where the frames *actually drawn* put the
//! animation, and the dim one is where wall-clock time does.
//!
//! - **`throttle`** keeps them together. 24 steps a second, and the card reads 24.0.
//! - **`naive`** runs the bright cell ahead of the dim one and keeps going, and the card
//!   reads 30.0. Nothing here is broken — 24 fps is simply a rate that rule cannot
//!   express — and the animation pays for it by playing a quarter fast.
//!
//! Run it twice rather than side by side. A policy that reads the window's state would be
//! judged on which window happened to have focus, so this demo does not read it: both
//! runs throttle the same way whether they are focused or not, and the rule is the only
//! variable. `RatePolicy::inactive_at` is the crate's other capability and needs its own
//! demonstration rather than a confound in this one.

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, Context, FocusId, FontWeight, FramePipeline, Hsla, PreparedRoots, Render,
    StandardImmediatePipeline, Window, WindowBounds, WindowMetrics, WindowOptions, application,
    div, prelude::*, px, rgb, size,
};
use gpui_pass::{Ledger, PassPipeline, Phase, RatePolicy};
use gpui_runtime::ThrottledPipeline;

/// The rate both throttles are asked for. 24 does not divide 60, which is the whole
/// point: a fifth of it would be unremarkable.
const RATE: u32 = 24;

/// The strip's length, in frames drawn at [`RATE`] — 48 frames is two seconds of the
/// rate asked for, which is a lap slow enough to read.
const LAP_CELLS: u64 = 48;

/// How many frame intervals the graph keeps before the oldest scroll off.
const MAX_SAMPLES: usize = 120;

/// The graph's vertical scale, in milliseconds.
const GRAPH_MAX_MS: f32 = 100.0;

/// A frame interval above this is a hitch rather than a cadence, and is drawn amber. It
/// is well clear of the 50ms a 3:2 hold reaches, so the pulldown is never mistaken for
/// a stall.
const STALL_MS: f32 = 70.0;

/// The display the demo is written against. A refresh of it, and the two holds a 3:2
/// pulldown alternates between, are derived from this rather than spelled out repeatedly.
const REFRESH_HZ: f32 = 60.0;
const REFRESH_MS: f32 = 1000.0 / REFRESH_HZ;

const GRAPH_HEIGHT: f32 = 120.0;

const BACKGROUND: u32 = 0x0b0f14;
const SURFACE: u32 = 0x151b23;
const BORDER: u32 = 0x30363d;
const TEXT: u32 = 0xe6edf3;
const MUTED: u32 = 0x8b949e;
const ACCENT: u32 = 0x38bdf8;
const GOOD: u32 = 0x34d399;
const WARN: u32 = 0xf59e0b;
const VIOLET: u32 = 0xa78bfa;

fn fps(n: u32) -> core::num::NonZeroU32 {
    core::num::NonZeroU32::new(n).expect("a rate above zero")
}

/// Frame intervals, newest last, plus the number of frames drawn and when the first one
/// was.
#[derive(Default)]
struct FrameStats {
    intervals_ms: VecDeque<f32>,
    frames: u64,
    last_started: Option<Instant>,
    started: Option<Instant>,
}

impl FrameStats {
    fn record(&mut self, now: Instant) {
        self.started.get_or_insert(now);
        if let Some(last) = self.last_started {
            let interval = (now - last).as_secs_f32() * 1000.0;
            self.intervals_ms.push_back(interval);
            if self.intervals_ms.len() > MAX_SAMPLES {
                self.intervals_ms.pop_front();
            }
        }
        self.last_started = Some(now);
        self.frames += 1;
    }

    fn latest_ms(&self) -> f32 {
        self.intervals_ms.back().copied().unwrap_or(0.0)
    }

    /// Frames a second, from the average of the last window of intervals.
    fn fps(&self) -> f32 {
        let count = self.intervals_ms.len();
        if count == 0 {
            return 0.0;
        }
        let average = self.intervals_ms.iter().sum::<f32>() / count as f32;
        if average <= 0.0 {
            0.0
        } else {
            1000.0 / average
        }
    }
}

/// Records the interval between drawn frames and forwards everything else.
struct IntervalPipeline<P> {
    inner: P,
    stats: Rc<RefCell<FrameStats>>,
}

impl<P: FramePipeline> FramePipeline for IntervalPipeline<P> {
    fn should_render(&mut self, is_dirty: bool, metrics: &WindowMetrics) -> bool {
        self.inner.should_render(is_dirty, metrics)
    }

    fn begin_frame(&mut self, window: &mut Window<'_>, cx: &mut App) {
        self.stats.borrow_mut().record(Instant::now());
        self.inner.begin_frame(window, cx);
    }

    fn evaluate_roots(&mut self, window: &mut Window<'_>, cx: &mut App) -> PreparedRoots {
        self.inner.evaluate_roots(window, cx)
    }

    fn layout_roots(&mut self, window: &mut Window<'_>, roots: &mut PreparedRoots, cx: &mut App) {
        self.inner.layout_roots(window, roots, cx);
    }

    fn paint_roots(&mut self, window: &mut Window<'_>, roots: PreparedRoots, cx: &mut App) {
        self.inner.paint_roots(window, roots, cx);
    }

    fn finish_frame(&mut self, window: &mut Window<'_>, cx: &mut App) {
        self.inner.finish_frame(window, cx);
    }

    fn complete_frame(&mut self, window: &mut Window<'_>, cx: &mut App) -> Option<FocusId> {
        self.inner.complete_frame(window, cx)
    }

    fn end_frame(
        &mut self,
        window: &mut Window<'_>,
        cx: &mut App,
        focus_before_listeners: Option<FocusId>,
    ) {
        self.inner.end_frame(window, cx, focus_before_listeners);
    }
}

/// Stacks the interval recorder onto a pipeline.
trait IntervalPipelined: FramePipeline + Sized {
    fn with_intervals(self, stats: Rc<RefCell<FrameStats>>) -> IntervalPipeline<Self> {
        IntervalPipeline { inner: self, stats }
    }
}

impl<P: FramePipeline> IntervalPipelined for P {}

/// Which rule the window runs, from the command line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pacer {
    Throttle,
    Naive,
}

impl Pacer {
    fn from_args() -> Self {
        match std::env::args().nth(1).as_deref() {
            Some("naive") => Pacer::Naive,
            _ => Pacer::Throttle,
        }
    }

    /// The tier and the claim, above the headline.
    fn badge(self) -> &'static str {
        match self {
            Pacer::Throttle => "WRAP · FRACTIONAL CADENCE PACING",
            Pacer::Naive => "WRAP · WHOLE-INTERVAL CADENCE",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Pacer::Throttle => "PassPipeline: The Carried Schedule",
            Pacer::Naive => "ThrottledPipeline: The Memoryless Cap",
        }
    }

    /// The problem both runs are a demonstration of.
    fn rule(self) -> &'static str {
        match self {
            Pacer::Throttle => {
                "24 fps on a 60 Hz display is 2.5 refreshes a frame. A memoryless limiter can \
                 hold whole intervals and nothing else — 30 fps or 20 — where a carried \
                 schedule accumulates the remainder and lands on the 3:2 pulldown."
            }
            Pacer::Naive => {
                "24 fps on a 60 Hz display is 2.5 refreshes a frame. This rule is \
                 `now - last >= 3/4 of the interval` and remembers only the last frame, so it \
                 cannot hold the half: its tolerance rounds the cadence to two refreshes and \
                 it runs a quarter fast."
            }
        }
    }

    /// The callout under the phase-lock track.
    fn callout(self) -> &'static str {
        match self {
            Pacer::Throttle => {
                "Locked in phase. The bright cell is where the frames actually drawn put the \
                 animation and the dim one is where wall-clock time does — the fractional \
                 remainder is carried, so the two never drift apart."
            }
            Pacer::Naive => {
                "Out of phase. The bright cell runs ahead of the dim one and keeps going: 30 \
                 frames a second is what this rule delivers when 24 was asked for."
            }
        }
    }
}

struct Demo {
    pacer: Pacer,
    stats: Rc<RefCell<FrameStats>>,
    ledger: Rc<RefCell<Ledger>>,
}

impl Render for Demo {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();

        let (fps_now, latest_ms, frames, started, intervals) = {
            let stats = self.stats.borrow();
            (
                stats.fps(),
                stats.latest_ms(),
                stats.frames,
                stats.started,
                stats.intervals_ms.iter().copied().collect::<Vec<f32>>(),
            )
        };

        // Everything the ledger has to say, read once, between frames.
        let (drawn, deferred, shed, paint_ms) = {
            let ledger = self.ledger.borrow();
            let paint_ms = ledger
                .last()
                .map(|frame| frame.phase(Phase::Paint).as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            (
                ledger.drawn(),
                ledger.deferred(),
                ledger.deferral_ratio() * 100.0,
                paint_ms,
            )
        };
        let locked = (fps_now - RATE as f32).abs() < 0.5;

        // Where the two independent clocks are. `clock_steps` is what a perfect 24 fps
        // would have drawn by now, so the difference is how far this throttle has run
        // ahead of the rate it was asked for.
        let elapsed = started.map_or(Duration::ZERO, |started| started.elapsed());
        let clock_steps = (elapsed.as_secs_f64() * f64::from(RATE)) as u64;
        let ahead = frames as i64 - clock_steps as i64;

        let target_ms = 1000.0 / RATE as f32;
        let target_height = (target_ms / GRAPH_MAX_MS * GRAPH_HEIGHT).min(GRAPH_HEIGHT);

        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .bg(rgb(BACKGROUND))
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(ACCENT))
                            .child(self.pacer.badge()),
                    )
                    .child(
                        div()
                            .text_size(px(21.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(TEXT))
                            .child(self.pacer.title()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(MUTED))
                            .child(self.pacer.rule()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .child(stat_card(
                        format!("{fps_now:.1}"),
                        "Cadence (FPS)",
                        if locked {
                            "locked to target".to_string()
                        } else {
                            format!("target {RATE} fps")
                        },
                        rgb(ACCENT),
                    ))
                    .child(stat_card(
                        format!("{latest_ms:.1} ms"),
                        "Last Frame Interval",
                        format!(
                            "the 3:2 cycle: {:.1} / {:.1} ms",
                            REFRESH_MS * 2.0,
                            REFRESH_MS * 3.0
                        ),
                        rgb(GOOD),
                    ))
                    .child(stat_card(
                        frames.to_string(),
                        "Frames Rendered",
                        format!("{deferred} redraws deferred ({shed:.0}% shed)"),
                        rgb(VIOLET),
                    ))
                    .child(stat_card(
                        format!("{ahead:+}"),
                        "Phase Lead (frames)",
                        "carried, so it never drifts".to_string(),
                        if ahead.abs() <= 2 {
                            rgb(GOOD)
                        } else {
                            rgb(WARN)
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section_title(format!(
                        "Phase Lock: wall clock vs. rendered state ({LAP_CELLS}-frame window)"
                    )))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_4()
                            .child(legend(MUTED, "dim — where an ideal 24 fps clock is"))
                            .child(legend(ACCENT, "bright — the frame on screen now")),
                    )
                    .child(step_strip(frames, clock_steps))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(GOOD))
                            .child(self.pacer.callout()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section_title(
                        "Frame Intervals & 3:2 Alternation (newest → right)",
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_4()
                            .child(legend(
                                ACCENT,
                                format!(
                                    "3:2 stems — {:.1} / {:.1} ms, alternating",
                                    REFRESH_MS * 2.0,
                                    REFRESH_MS * 3.0
                                ),
                            ))
                            .child(legend(WARN, format!("over {STALL_MS:.0} ms — a stall"))),
                    )
                    .child(graph(&intervals, target_height))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(MUTED))
                            .child(format!(
                                "-- {target_ms:.1} ms target ({RATE} fps average) --"
                            )),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "LEDGER: {drawn} drawn · {deferred} redraws deferred ({shed:.0}% of asks \
                         shed) · last frame paint {paint_ms:.2} ms"
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(explainer(
                        "The 2.5-refresh problem",
                        "24 fps on a 60 Hz display is 2.5 refreshes a frame — 41.7 ms, and not a \
                         whole number of anything. A limiter that compares only against \
                         `last_frame` cannot express the half, so it collapses onto two \
                         refreshes (30 fps) or three (20 fps).",
                    ))
                    .child(explainer(
                        "The carried accumulator",
                        "The schedule keeps the remainder. A frame that holds two refreshes \
                         (33.3 ms) passes half of one to the next, which holds three (50.0 ms); \
                         the alternation repeats as the 3:2 pulldown, and the average is exactly \
                         24 fps.",
                    ))
                    .child(explainer(
                        "Selective dispatch",
                        "Neither run chooses when the display presents — it cannot from here. \
                         What it chooses is whether the application spends cycles preparing a \
                         frame. Skipping the asks the rate does not need leaves the thread idle \
                         instead of drawing frames nobody sees.",
                    )),
            )
    }
}

/// A number, what it is, and how to read it.
fn stat_card(
    value: String,
    label: &'static str,
    subtext: String,
    color: impl Into<Hsla>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .rounded_lg()
        .bg(rgb(SURFACE))
        .child(
            div()
                .text_size(px(20.))
                .font_weight(FontWeight::BOLD)
                .text_color(color)
                .child(value),
        )
        .child(div().text_size(px(11.)).text_color(rgb(TEXT)).child(label))
        .child(
            div()
                .text_size(px(10.))
                .text_color(rgb(MUTED))
                .child(subtext),
        )
}

/// A bold line above a visualizer.
fn section_title(text: impl Into<String>) -> impl IntoElement {
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(TEXT))
        .child(text.into())
}

/// A swatch and what it stands for, for a track or a graph.
fn legend(swatch: u32, text: impl Into<String>) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(div().w(px(9.)).h(px(9.)).rounded_full().bg(rgb(swatch)))
        .child(
            div()
                .text_size(px(10.))
                .text_color(rgb(MUTED))
                .child(text.into()),
        )
}

/// A claim and the reason for it, under the visualizers.
fn explainer(lead: &'static str, body: &'static str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(TEXT))
                .child(lead),
        )
        .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(body))
}

/// The two clocks side by side, as one cell lit per step.
///
/// `frames` is what the throttle has drawn and `clock_steps` is what the rate asked for
/// would have drawn, so a lit cell that stays put is the two agreeing and one that
/// creeps ahead is not. The dim cell is drawn under the bright one, which is why a run
/// at the asked rate looks like a single moving cell.
fn step_strip(frames: u64, clock_steps: u64) -> impl IntoElement {
    let drawn = frames % LAP_CELLS;
    let clock = clock_steps % LAP_CELLS;
    div()
        .flex()
        .flex_row()
        .gap(px(2.))
        .w_full()
        .h(px(16.))
        .children((0..LAP_CELLS).map(move |cell| {
            let color = if cell == drawn {
                rgb(ACCENT)
            } else if cell == clock {
                rgb(MUTED)
            } else {
                rgb(SURFACE)
            };
            div().flex_1().h_full().rounded_full().bg(color)
        }))
}

fn graph(intervals: &[f32], target_height: f32) -> impl IntoElement {
    div()
        .relative()
        .w_full()
        .h(px(GRAPH_HEIGHT))
        .rounded_lg()
        .bg(rgb(SURFACE))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .flex()
                .flex_row()
                .items_end()
                .gap(px(2.))
                .children(intervals.iter().map(|&ms| frame_bar(ms))),
        )
        .child(
            div()
                .absolute()
                .left(px(0.))
                .right(px(0.))
                .bottom(px(target_height))
                .h(px(1.))
                .bg(rgb(BORDER)),
        )
}

/// One frame interval as a bar, amber past the point a cadence would reach.
fn frame_bar(ms: f32) -> impl IntoElement {
    let height = (ms / GRAPH_MAX_MS).clamp(0.0, 1.0) * GRAPH_HEIGHT;
    let color = if ms > STALL_MS {
        rgb(WARN)
    } else {
        rgb(ACCENT)
    };
    div().w(px(3.)).h(px(height)).bg(color)
}

fn run_demo(pacer: Pacer) {
    let stats = Rc::new(RefCell::new(FrameStats::default()));
    let ledger = Ledger::shared(Ledger::DEFAULT_CAPACITY);

    application()
        .with_frame_pipeline({
            let stats = stats.clone();
            let ledger = ledger.clone();
            move |_window_id| match pacer {
                Pacer::Throttle => Box::new(
                    PassPipeline::new(
                        StandardImmediatePipeline,
                        // `allow_burst(2)` is not decoration. The asks arrive on the
                        // display's boundaries and the schedule is anchored to the moment
                        // it admits, so a target equal to the refresh period leaves no
                        // margin to absorb that hair and halves. One interval of slack is
                        // what keeps a rate — and it is also what lets a rate *between*
                        // the asks, like 24 on 60Hz, carry its remainder across frames
                        // instead of locking to two refreshes or three. See
                        // `tests/throttling.rs`.
                        RatePolicy::at(fps(RATE)).allow_burst(fps(2)),
                        ledger.clone(),
                    )
                    .with_intervals(stats.clone()),
                ),
                Pacer::Naive => Box::new(
                    ThrottledPipeline::new(StandardImmediatePipeline, RATE)
                        .with_intervals(stats.clone()),
                ),
            }
        })
        .run(move |cx: &mut App| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(720.), px(860.)),
                        cx,
                    ))),
                    // The engine caps an unfocused window at 30 fps in the frame source,
                    // before any pipeline is consulted (`WindowOptions`'s
                    // `inactive_frame_interval`). That is a throttle of its own, and it
                    // would put the asks on a 30Hz grid the moment this window lost focus —
                    // which is not a 60Hz grid, so the 3:2 cycle could not appear at all.
                    // Off, so that the ask grid is the display's whatever has focus and the
                    // throttling rule is the only thing that differs between the two runs.
                    inactive_frame_interval: None,
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| Demo {
                        pacer,
                        stats: stats.clone(),
                        ledger: ledger.clone(),
                    })
                },
            )
            .expect("the platform opened the window");
            cx.activate(true);
        });
}

fn main() {
    run_demo(Pacer::from_args());
}
