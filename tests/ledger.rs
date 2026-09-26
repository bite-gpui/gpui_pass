//! What the ledger makes visible that timing alone cannot: the asks the throttle
//! passed over.
//!
//! `tests/throttling.rs` establishes *which* asks a rate admits, with time under
//! the test's control. This file is about what the pass says afterwards — the
//! count of asks that did not draw. That is the number the facade's own
//! instrumentation has no way to report, because it only ever sees the frames that
//! did: its `PhaseMetrics` counts frames, and an ask the throttle turned away never
//! reaches a pass.
//!
//! Driving `should_render` is enough to check that, and it needs no window: a
//! refusal never reaches a pass at all, so the accounting has to be right in the
//! decision rather than assembled from timings.

use core::num::NonZeroU32;
use std::{cell::RefCell, rc::Rc, time::Duration};

use gpui_authoring::{FramePipeline, StandardImmediatePipeline, WindowMetrics};
use gpui_pass::{FakeClock, Ledger, PassPipeline, RatePolicy};
use gpui_types::{Bounds, Pixels, Point, Size};

/// How many asks one run makes: 120 asks on a 60Hz grid is two seconds.
const ASKS: usize = 120;

fn fps(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("a rate above zero")
}

/// A metrics value for an active window, which is the state every ask here is in.
fn metrics() -> WindowMetrics {
    let size = Size::new(Pixels::from(1280.0), Pixels::from(800.0));
    WindowMetrics {
        bounds: Bounds::new(Point::new(Pixels::from(0.0), Pixels::from(0.0)), size),
        viewport: Bounds::new(Point::new(Pixels::from(0.0), Pixels::from(0.0)), size),
        content_size: size,
        scale_factor: 1.0,
        display_id: None,
        is_active: true,
        is_fullscreen: false,
    }
}

/// Drives [`ASKS`] asks on a 60Hz grid at `rate`, and hands back the ledger that
/// recorded them.
fn ask_on_a_grid(rate: u32, burst: u32) -> Rc<RefCell<Ledger>> {
    let clock = FakeClock::default();
    let ledger = Ledger::shared(Ledger::DEFAULT_CAPACITY);
    let mut pass = PassPipeline::with_clock(
        StandardImmediatePipeline,
        RatePolicy::at(fps(rate)).allow_burst(fps(burst)),
        ledger.clone(),
        clock.clone(),
    );
    let metrics = metrics();
    for ask in 0..ASKS {
        if ask > 0 {
            clock.advance(Duration::from_secs_f64(1.0 / 60.0));
        }
        pass.should_render(true, &metrics);
    }
    ledger
}

/// Every ask is accounted for, and the ones the rate did not allow are counted as
/// passed over rather than simply missing.
///
/// Two seconds at 30 fps is sixty frames; the burst of two adds one at the front,
/// so sixty-one of the hundred and twenty asks draw and the rest are the frames the
/// throttle coalesced away.
#[test]
fn the_ledger_counts_the_asks_the_throttle_passed_over() {
    let ledger = ask_on_a_grid(30, 2);
    let ledger = ledger.borrow();

    assert_eq!(ledger.asks(), ASKS);
    assert_eq!(ledger.admitted() + ledger.deferred(), ASKS);
    assert!(
        (60..=62).contains(&ledger.admitted()),
        "admitted {} of {ASKS} asks at 30 fps",
        ledger.admitted()
    );
    assert!(
        (0.47..=0.51).contains(&ledger.deferral_ratio()),
        "passed over {:.3} of the asks",
        ledger.deferral_ratio()
    );
}

/// A rate the grid divides admits every ask, so nothing is passed over.
#[test]
fn an_uncapped_pass_passes_nothing_over() {
    let ledger = ask_on_a_grid(60, 2);
    let ledger = ledger.borrow();

    assert_eq!(ledger.admitted(), ASKS);
    assert_eq!(ledger.deferred(), 0);
    assert_eq!(ledger.deferral_ratio(), 0.0);
}

/// The budget a ledger is read against is the interval the rate implies, so no
/// second knob has to be kept in step with the policy.
#[test]
fn the_budget_is_the_interval_the_rate_implies() {
    let pass = PassPipeline::with_clock(
        StandardImmediatePipeline,
        RatePolicy::at(fps(60)),
        Ledger::shared(8),
        FakeClock::default(),
    );

    assert_eq!(pass.rate(), 60);
    // A second over sixty, truncated to whole nanoseconds.
    assert_eq!(pass.budget(), Duration::from_nanos(16_666_666));
}

/// Re-pacing a window moves the budget with it.
#[test]
fn the_budget_follows_a_policy_change() {
    let mut pass = PassPipeline::with_clock(
        StandardImmediatePipeline,
        RatePolicy::at(fps(60)),
        Ledger::shared(8),
        FakeClock::default(),
    );

    pass.set_policy(RatePolicy::at(fps(24)));
    assert_eq!(pass.rate(), 24);
    assert_eq!(pass.budget(), Duration::from_nanos(41_666_666));
}
