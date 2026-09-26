//! The pipeline, driven the way the facade drives it.
//!
//! `tests/throttling.rs` establishes what the two rules do to a stream of asks, with time
//! under the test's control. This file is about the other half: whether the facade can
//! install this one and get frames out of it at all.
//!
//! That is not the same question, and `gpui_morphorm`'s facade test exists because
//! assuming it was cost a transparent window. A pipeline is handed a `is_dirty` and a
//! `&WindowMetrics` by a window it does not own, and the metrics are a snapshot the
//! window publishes — an implementation can look perfect against a hand-built
//! `WindowMetrics` and still never admit a frame in a window, because the seam it is
//! called through is not the seam its tests build. Only a test that draws through
//! `set_frame_pipeline_factory` can tell the two apart.
//!
//! Run: `cargo test --features test-support --test facade -- --nocapture`.
//!
//! Two traps are worth recording here:
//!
//! **Do not `use gpui::*;` in a file that expands `#[gpui::test]`.** The glob brings
//! gpui's own `test` attribute into scope, which shadows the built-in `#[test]`, and
//! the `#[test]` that `#[gpui::test]` emits then resolves to gpui's macro and expands
//! itself forever. rustc reports `recursion limit reached while expanding #[test]`,
//! with no hint that the glob is at fault. The import list below is curated for that
//! reason.
//!
//! **The engine throttles an unfocused window before any pipeline sees it.**
//! `WindowOptions::default().inactive_frame_interval` caps an inactive window to 30 fps
//! in the frame *source*, and a test window is inactive. The throttle only engages when a
//! request carries `force_render` or lands with next-frame callbacks pending, and the
//! view here neither forces a render nor asks for the next frame — so the asks below
//! reach the pipeline rather than being eaten by it.
//!
//! The asks are delivered by marking the window dirty and letting the harness draw when
//! the window is next served, rather than by calling `simulate_frame_request` directly:
//! that method is on a `TestWindow`, which `TestAppContext::test_window` hands out, and
//! that accessor is `pub(crate)`. An out-of-tree crate cannot reach it, so this file
//! drives the same seam the only way it can and says so rather than pretending to the
//! in-tree idiom.

#![cfg(feature = "test-support")]

use std::{
    cell::{Cell, RefCell},
    num::NonZeroU32,
    rc::Rc,
    time::Duration,
};

use gpui::prelude::*;
use gpui::{
    App, Context, FocusId, FramePipeline, PreparedRoots, Render, StandardImmediatePipeline,
    TestAppContext, Window, WindowMetrics, div, rgb,
};
use gpui_pass::{FakeClock, Ledger, PassPipeline, Phase, RatePolicy};

/// How many asks one run makes: 300 asks on a 60Hz grid is five seconds.
const ASKS: usize = 300;

fn fps(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("a rate above zero")
}

/// A view that draws something and, deliberately, never requests another frame — so the
/// frame source has no pending next-frame callbacks and the throttle above cannot
/// engage. The ask stream is the test's, not the view's.
struct Plain;

impl Render for Plain {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0x112233))
    }
}

/// Counts the asks the pipeline inside it admitted, and forwards every pass.
struct CountedFrames<P> {
    inner: P,
    drawn: Rc<Cell<usize>>,
}

impl<P: FramePipeline> FramePipeline for CountedFrames<P> {
    fn should_render(&mut self, is_dirty: bool, metrics: &WindowMetrics) -> bool {
        let admit = self.inner.should_render(is_dirty, metrics);
        if admit {
            self.drawn.set(self.drawn.get() + 1);
        }
        admit
    }

    fn begin_frame(&mut self, window: &mut Window<'_>, cx: &mut App) {
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

/// Installs a throttled pipeline at `rate`, then marks the window dirty and draws
/// [`ASKS`] times on a 60Hz grid, and returns how many of those draws the pipeline
/// admitted.
///
/// The clock the throttle is built on is the test's own, and it is what the asks are
/// spaced by — the decision is about time, so the test has to be the one holding it.
/// `burst` is [`RatePolicy::allow_burst`]'s, and two at every rate below is the setting
/// the crate documents: one interval of slack.
fn draws_over_a_grid(rate: u32, burst: u32) -> (usize, Rc<RefCell<Ledger>>) {
    let mut cx = TestAppContext::single();
    let clock = FakeClock::default();
    let drawn = Rc::new(Cell::new(0usize));
    let ledger = Ledger::shared(Ledger::DEFAULT_CAPACITY);

    cx.update({
        let clock = clock.clone();
        let drawn = drawn.clone();
        let ledger = ledger.clone();
        move |app: &mut App| {
            app.set_frame_pipeline_factory(Rc::new(move |_| {
                let inner = PassPipeline::with_clock(
                    StandardImmediatePipeline,
                    RatePolicy::at(fps(rate)).allow_burst(fps(burst)),
                    ledger.clone(),
                    clock.clone(),
                );
                Box::new(CountedFrames {
                    inner,
                    drawn: drawn.clone(),
                }) as Box<dyn FramePipeline>
            }));
        }
    });

    let (view, vcx) = cx.add_window_view(|_, _| Plain);

    // Opening the window drew the first frame, and a frame spends a slot. Let a second
    // pass so the window's own frame is not counted against the asks below.
    clock.advance(Duration::from_secs(1));
    drawn.set(0);

    for ask in 0..ASKS {
        if ask > 0 {
            clock.advance(Duration::from_secs_f64(1.0 / 60.0));
        }
        // A drawn frame leaves the window clean, and `StandardImmediatePipeline` draws
        // only a dirty one — so each ask needs the mark the platform would have left.
        view.update(&mut vcx.cx, |_, cx| cx.notify());
        vcx.run_until_parked();
    }

    (drawn.get(), ledger)
}

/// The calibration: at the grid's own rate the pipeline admits every ask.
///
/// 60 fps against a 60Hz grid is one frame per ask, so anything less than every ask is
/// the harness losing frames rather than the pipeline throttling them — which is what
/// makes the number below it meaningful rather than a count of something unnamed.
#[gpui::test]
fn every_ask_is_admitted_at_the_grid_rate(_cx: &mut TestAppContext) {
    let (drawn, _ledger) = draws_over_a_grid(60, 2);
    println!("60 fps through the facade: {drawn} of {ASKS} asks admitted");
    assert!(
        drawn >= ASKS - 2,
        "asked for a frame on every ask and got {drawn} of {ASKS}"
    );
}

/// A rate between the grid's asks is the one the source cannot deliver: 24 fps on a
/// 60Hz grid.
///
/// This is the number the demo shows as a card, taken the way the facade would produce
/// it. Five seconds at 24 fps is 120 frames; the extra one is the burst of two, spent at
/// the front.
#[gpui::test]
fn the_rate_between_the_asks_is_the_one_that_arrives(_cx: &mut TestAppContext) {
    let (drawn, _ledger) = draws_over_a_grid(24, 2);
    println!(
        "24 fps through the facade: {drawn} of {ASKS} asks admitted ({} fps)",
        drawn as f64 / 5.0
    );
    assert!(
        (119..=124).contains(&drawn),
        "asked for 24 fps over five seconds and got {drawn} frames"
    );
}

/// The frames the engine draws through the facade are the asks the ledger counts.
///
/// A pipeline can look right against a hand-built `WindowMetrics` and still never
/// admit a frame in a window, which is what `gpui_morphorm`'s facade test exists to
/// catch. This is that half again: the ledger's own accounting agrees with the
/// counter wrapped around it, through the seam the engine actually drives.
///
/// The costs recorded here are zero, and by construction — this run is on
/// [`FakeClock`] so the grid is exact, and a fake clock does not move while a frame
/// runs. [`the_ledger_times_the_passes_on_the_real_clock`] is where timings are
/// checked.
#[gpui::test]
fn the_ledger_accounts_for_the_frames_the_engine_draws(_cx: &mut TestAppContext) {
    let (drawn, ledger) = draws_over_a_grid(24, 2);
    let ledger = ledger.borrow();

    assert!(drawn > 0, "the harness drew no frames to account for");
    assert!(
        ledger.admitted() >= drawn,
        "the ledger saw {} of the {drawn} asks the counter admitted",
        ledger.admitted()
    );
    assert!(
        ledger.deferred() > 0,
        "a rate below the grid should pass asks over, and none were"
    );
    assert!(ledger.drawn() > 0, "no frame reached the ring");

    let last = ledger.last().expect("a filed frame");
    assert!(
        last.total() >= last.phase(Phase::Paint),
        "a total that missed its own paint pass"
    );
    assert!(
        ledger.drawn() + 1 >= ledger.admitted(),
        "{} admits should file at most one frame short of themselves, filed {}",
        ledger.admitted(),
        ledger.drawn()
    );
}

/// The other half of the ledger: on the real clock, the passes are actually timed.
///
/// The grid tests run on [`FakeClock`] so their decisions are exact — which makes
/// their recorded costs exact too, and zero, because a fake clock does not move
/// while a frame runs. This installs the real one and checks the passes were
/// charged for.
#[gpui::test]
fn the_ledger_times_the_passes_on_the_real_clock(_cx: &mut TestAppContext) {
    let mut cx = TestAppContext::single();
    let ledger = Ledger::shared(Ledger::DEFAULT_CAPACITY);

    cx.update({
        let ledger = ledger.clone();
        move |app: &mut App| {
            app.set_frame_pipeline_factory(Rc::new(move |_| {
                Box::new(PassPipeline::new(
                    StandardImmediatePipeline,
                    RatePolicy::at(fps(60)),
                    ledger.clone(),
                )) as Box<dyn FramePipeline>
            }));
        }
    });

    let (view, vcx) = cx.add_window_view(|_, _| Plain);
    view.update(&mut vcx.cx, |_, cx| cx.notify());
    vcx.run_until_parked();

    let ledger = ledger.borrow();
    assert!(ledger.drawn() > 0, "no frame reached the ring");
    let last = ledger.last().expect("a filed frame");
    assert!(
        last.total() > Duration::ZERO,
        "the passes were not timed by a real clock"
    );
    assert!(
        last.phase(Phase::Paint) > Duration::ZERO,
        "paint was never timed"
    );
}
