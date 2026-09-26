//! The puffin scopes a window's passes emit, read back in-process.
//!
//! [`PassPipeline`](gpui_pass::PassPipeline) keeps a summary of a window: what each
//! pass cost, how many asks the throttle passed over, where the recent frames sit
//! against the budget. [`PuffinPipeline`] is the other shape a measurement takes — one
//! scope per pass, on a timeline a viewer can be dragged across. This file drives real
//! frames through the facade with the scopes on and reads the frames back, which is the
//! half a profiler whose data lives only inside an external GUI cannot do: here the
//! test *is* the viewer.
//!
//! Both halves of that are why the crate carries puffin rather than a C++ client. The
//! scopes are pure Rust, so a consumer links nothing extra. And they are collected
//! in-process, so `GlobalFrameView` is a sink a test can install and a frame is an
//! `Arc<FrameData>` it can assert on — no GUI, no socket, no display.
//!
//! Run: `cargo test --features puffin,test-support --test puffin -- --nocapture`.
//!
//! Three traps, worth recording because each fails in a way that does not name itself:
//!
//! **Do not `use gpui::*;` in a file that expands `#[gpui::test]`.** The glob brings
//! gpui's own `test` attribute into scope, which shadows the built-in `#[test]`, and
//! the `#[test]` that `#[gpui::test]` emits then resolves to gpui's macro and expands
//! itself forever. The import list below is curated for that reason.
//!
//! **A frame is only readable after another has begun.** Puffin's frame boundary
//! collects the frame that *finished*, and this crate puts that call at the top of
//! `begin_frame` where puffin's own documentation does — so a single drawn frame leaves
//! its scopes pending and `latest_frame()` returns `None`. The tests below draw several.
//!
//! **Puffin's state is global and process-wide.** `set_scopes_on` flips one atomic for
//! the whole process and the profiler collects scopes from every thread, so two tests
//! here running on libtest's threads would read each other's frames. [`one_at_a_time`]
//! serializes them.

#![cfg(all(feature = "puffin", feature = "test-support"))]

use std::{
    num::NonZeroU32,
    rc::Rc,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use gpui::prelude::*;
use gpui::{
    App, Context, FramePipeline, Render, StandardImmediatePipeline, TestAppContext, Window, div,
    rgb,
};
use gpui_pass::{FakeClock, Ledger, PassPipeline, PuffinPipeline, RatePolicy};
use puffin::GlobalFrameView;

/// The passes [`FramePipeline::draw`] runs, in the order it runs them — one scope each.
const PASSES: [&str; 7] = [
    "gpui::begin_frame",
    "gpui::evaluate_roots",
    "gpui::layout_roots",
    "gpui::paint_roots",
    "gpui::finish_frame",
    "gpui::complete_frame",
    "gpui::end_frame",
];

/// How many frames to draw. A frame's scopes are collected when the next frame opens
/// its boundary, so one draw reads back nothing and two read back one.
const DRAWS: usize = 4;

fn fps(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("a rate above zero")
}

/// Serializes the tests in this binary. Puffin's on/off flag and its sinks are
/// process-wide, so two of these running at once would read each other's frames.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A view that draws something and, deliberately, never requests another frame — so
/// the frame source has no pending next-frame callbacks and its throttle above cannot
/// engage. The ask stream is the test's, not the view's.
struct Plain;

impl Render for Plain {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0x112233))
    }
}

/// Every pass the engine runs is a scope, named for the trait method that runs it.
///
/// The count is exact, and that is the assertion: the seven passes of [`FramePipeline`]
/// are the seven scopes, no more and no fewer. A pass that forgot to scope would show up
/// as a short count, and a scope around the decision would show up as a long one.
#[gpui::test]
fn every_pass_of_a_drawn_frame_is_a_scope(_cx: &mut TestAppContext) {
    let _serial = one_at_a_time();
    puffin::set_scopes_on(true);
    let view = GlobalFrameView::default();

    let mut cx = TestAppContext::single();
    cx.update(|app: &mut App| {
        app.set_frame_pipeline_factory(Rc::new(|_| {
            Box::new(PuffinPipeline::new(StandardImmediatePipeline)) as Box<dyn FramePipeline>
        }));
    });

    let (view_handle, vcx) = cx.add_window_view(|_, _| Plain);
    for _ in 0..DRAWS {
        // A drawn frame leaves the window clean, and `StandardImmediatePipeline` draws
        // only a dirty one — so each ask needs the mark the platform would have left.
        view_handle.update(&mut vcx.cx, |_, cx| cx.notify());
        vcx.run_until_parked();
    }

    let lock = view.lock();
    let frame = lock
        .latest_frame()
        .expect("no completed frame reached the sink");
    assert_eq!(
        frame.meta().num_scopes,
        PASSES.len(),
        "one scope per pass, and {} is not {}",
        frame.meta().num_scopes,
        PASSES.len()
    );

    let collection = lock.scope_collection();
    for name in PASSES {
        assert!(
            collection.fetch_by_name(name).is_some(),
            "no scope was registered under {name}"
        );
    }
    // The decision is asked once per *ask*, not once per drawn frame, so a scope
    // around it would report a per-ask duration as if it were part of a frame. The
    // exact count above catches it too; this says so directly.
    assert!(
        collection.fetch_by_name("gpui::should_render").is_none(),
        "should_render must not be scoped — it is asked per ask, not per frame"
    );
}

/// The scope emitter and the throttle compose, and the scopes survive being wrapped.
///
/// A decorator measures whatever it encloses, so the observable calls compose in an
/// application: pass the pipeline by a rate, then wrap the result in the scope emitter.
/// This drives that composition — the one a consumer installs — and asserts the frames
/// still reach the sink with a scope per pass. The clock is the test's, spaced a whole
/// second per ask so the throttle admits every one: what the scenes scale is the
/// composition, not the throttle, which `tests/throttling.rs` measures on its own.
#[gpui::test]
fn the_throttle_and_the_scope_emitter_compose_on_one_pass(_cx: &mut TestAppContext) {
    let _serial = one_at_a_time();
    puffin::set_scopes_on(true);
    let view = GlobalFrameView::default();

    let mut cx = TestAppContext::single();
    let clock = FakeClock::default();
    let ledger = Ledger::shared(Ledger::DEFAULT_CAPACITY);

    cx.update({
        let clock = clock.clone();
        let ledger = ledger.clone();
        move |app: &mut App| {
            app.set_frame_pipeline_factory(Rc::new(move |_| {
                let throttled = PassPipeline::with_clock(
                    StandardImmediatePipeline,
                    RatePolicy::at(fps(60)).allow_burst(fps(2)),
                    ledger.clone(),
                    clock.clone(),
                );
                Box::new(PuffinPipeline::new(throttled)) as Box<dyn FramePipeline>
            }));
        }
    });

    let (view_handle, vcx) = cx.add_window_view(|_, _| Plain);
    for _ in 0..DRAWS {
        clock.advance(Duration::from_secs(1));
        view_handle.update(&mut vcx.cx, |_, cx| cx.notify());
        vcx.run_until_parked();
    }

    let drawn = ledger.borrow().drawn();
    assert!(
        drawn >= DRAWS - 1,
        "the composed pipeline drew {drawn} frames, too few to read"
    );

    let lock = view.lock();
    let frame = lock
        .latest_frame()
        .expect("no completed frame reached the sink");
    assert_eq!(frame.meta().num_scopes, PASSES.len());
}
