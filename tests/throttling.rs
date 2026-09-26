//! Throttling, with time under the caller's control.
//!
//! The property worth testing about a frame throttle is not "does it defer a frame" —
//! an interval check does that — but *when* the frames it admits land, and what a
//! burst does to the ones after it. Both are questions about time, so the tests below
//! drive the pipeline with a clock they own. [`FakeClock`] exists for this, and it is
//! why the decision can be tested exactly rather than statistically.
//!
//! The one thing a fake clock cannot do is drive the shipped decorator:
//! `ThrottledPipeline` reads `Instant::now()`, so the two columns can only be compared
//! in real time. That comparison is not here — it is `examples/throttle_cost.rs`, which
//! prints it, because it is a measurement rather than an assertion about this crate.

use core::num::NonZeroU32;
use std::time::{Duration, Instant};

use gpui_authoring::{FramePipeline, StandardImmediatePipeline, WindowMetrics};
use gpui_pass::{FakeClock, Ledger, PassPipeline, RatePolicy};
use gpui_types::{Bounds, Pixels, Point, Size};

fn fps(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("a rate above zero")
}

/// A metrics value for a window in the given state.
///
/// `WindowMetrics` is a plain value with public fields and no constructor, so
/// building one is how the seam is handed to a pipeline outside a window — and the
/// only field this crate reads is `is_active`.
fn metrics(is_active: bool) -> WindowMetrics {
    let size = Size::new(Pixels::from(1280.0), Pixels::from(800.0));
    WindowMetrics {
        bounds: Bounds::new(Point::new(Pixels::from(0.0), Pixels::from(0.0)), size),
        viewport: Bounds::new(Point::new(Pixels::from(0.0), Pixels::from(0.0)), size),
        content_size: size,
        scale_factor: 1.0,
        display_id: None,
        is_active,
        is_fullscreen: false,
    }
}

fn throttled(
    policy: RatePolicy,
    clock: &FakeClock,
) -> PassPipeline<StandardImmediatePipeline, FakeClock> {
    PassPipeline::with_clock(
        StandardImmediatePipeline,
        policy,
        Ledger::shared(8),
        clock.clone(),
    )
}

/// No two admitted frames are closer together than the interval, and none are
/// further apart than the interval plus the resolution the asks arrive at.
///
/// This is the whole difference between a carried schedule and an interval check with
/// tolerance. The shipped cap keeps 25% of its interval as slack for display jitter,
/// unconditionally, and a frame admitted 25% early is a frame closer to its
/// predecessor than the rate allows — under a steady stream of asks that settles at
/// 40 frames a second for a nominal 30. Here the spacing *is* the interval.
///
/// What is not claimed is absolute placement. A throttle asked only at 100µs
/// boundaries can only admit on a 100µs boundary, and the schedule keeps the lateness
/// of an admission rather than discarding it, so the schedule itself drifts by up to an
/// ask-step per frame. That is a limit of the resolution it is asked at, not of the
/// rate: the spacing, and so the number of frames a second, is unaffected.
#[test]
fn no_two_frames_are_closer_than_the_interval() {
    let clock = FakeClock::default();
    let mut pipe = throttled(RatePolicy::at(fps(30)), &clock);
    let metrics = metrics(true);

    const STEP_US: u64 = 100;
    const SECONDS: u64 = 1_000_000 / STEP_US;
    let interval = 1_000.0 / 30.0;
    let step_ms = STEP_US as f64 / 1_000.0;
    let mut admitted_at = Vec::new();
    for step in 0..=SECONDS {
        if step > 0 {
            clock.advance(Duration::from_micros(STEP_US));
        }
        if pipe.should_render(true, &metrics) {
            admitted_at.push(step);
        }
    }

    // A second at 30 fps is 30 or 31 frames, and never the 40 an unconditional
    // tolerance would reach.
    let (low, high) = (30, 31);
    assert!(
        (low..=high).contains(&admitted_at.len()),
        "admitted {} frames in a second at 30 fps, expected {low}..={high}",
        admitted_at.len()
    );

    for pair in admitted_at.windows(2) {
        let gap = (pair[1] - pair[0]) as f64 * step_ms;
        assert!(
            (interval..=interval + step_ms).contains(&gap),
            "two frames {gap:.4}ms apart, at a rate of {interval:.4}ms asked on a {step_ms}ms grid"
        );
    }
}

/// A burst is spent once, and what follows it is the sustained rate.
#[test]
fn a_burst_is_frames_now_and_the_rate_after_that() {
    let clock = FakeClock::default();
    let mut pipe = throttled(RatePolicy::at(fps(30)).allow_burst(fps(3)), &clock);
    let metrics = metrics(true);

    // Three frames with no time passing at all: a drag that must track the
    // pointer, before the sustained rate can have anything to say.
    assert!(pipe.should_render(true, &metrics), "burst frame 1");
    assert!(pipe.should_render(true, &metrics), "burst frame 2");
    assert!(pipe.should_render(true, &metrics), "burst frame 3");
    assert!(
        !pipe.should_render(true, &metrics),
        "a fourth frame is not in the burst"
    );

    // And the fourth waits one interval, not one interval per refusal: asking
    // again while the window is dirty is what a deferred frame costs.
    clock.advance(Duration::from_secs_f64(1.0 / 30.0));
    assert!(pipe.should_render(true, &metrics), "the rate, once spent");
}

/// The policy is read per frame, so the same install throttles a window two ways.
#[test]
fn an_inactive_window_is_throttled_by_its_own_rate() {
    let clock = FakeClock::default();
    let mut pipe = throttled(RatePolicy::at(fps(60)).inactive_at(fps(15)), &clock);
    let active = metrics(true);
    let inactive = metrics(false);

    fn admissions_over_a_second(
        pipe: &mut PassPipeline<StandardImmediatePipeline, FakeClock>,
        clock: &FakeClock,
        metrics: &WindowMetrics,
    ) -> usize {
        let mut admitted = 0;
        for _ in 0..1000 {
            clock.advance(Duration::from_millis(1));
            if pipe.should_render(true, metrics) {
                admitted += 1;
            }
        }
        admitted
    }

    let foreground = admissions_over_a_second(&mut pipe, &clock, &active);
    let background = admissions_over_a_second(&mut pipe, &clock, &inactive);

    assert!(
        (58..=62).contains(&foreground),
        "an active window admitted {foreground} frames in a second at 60 fps"
    );
    assert!(
        (13..=17).contains(&background),
        "an inactive window admitted {background} frames in a second at 15 fps"
    );
}

/// A policy is data, so re-throttling a window mid-run takes effect without a restart.
#[test]
fn a_policy_change_applies_from_the_next_frame() {
    let clock = FakeClock::default();
    let mut pipe = throttled(RatePolicy::at(fps(60)), &clock);
    let metrics = metrics(true);

    assert!(pipe.should_render(true, &metrics), "the first frame");
    assert_eq!(pipe.rate(), 60);

    pipe.set_policy(RatePolicy::at(fps(10)));
    assert_eq!(pipe.rate(), 10);
    // The new schedule starts empty, so the window gets its burst back: the frame
    // right after a re-throttle is not made to wait for a rate it was not running at.
    assert!(
        pipe.should_render(true, &metrics),
        "a re-pace hands the window a full burst"
    );
    // From there the new rate is what applies.
    clock.advance(Duration::from_millis(16));
    assert!(
        !pipe.should_render(true, &metrics),
        "a frame 16ms after it is too soon at 10 fps"
    );
    clock.advance(Duration::from_millis(84));
    assert!(
        pipe.should_render(true, &metrics),
        "and one 100ms after it is not"
    );
}

/// A target equal to the refresh needs slack, and without any it halves.
///
/// This is the corner the shipped cap documents in its own source: "a target interval
/// that is an exact multiple of the refresh period (30 fps on a 60 Hz display) sits on
/// a knife's edge: sub-millisecond jitter in delivery would defer a frame that should
/// have drawn, dropping the rate to the next-lower multiple (20 fps)". A carried
/// schedule walks onto that edge with no tolerance at all, because it is anchored to
/// the instant it admitted a frame — a hair after the boundary the frame was asked on.
///
/// How thin the edge is depends on where the target sits against the refresh, and the
/// two cases are worth telling apart:
///
/// - **At the refresh rate the margin is exactly zero**, so an ask arriving a fraction
///   early is refused and the throttle answers every second ask: thirty frames a second
///   for a sixty frame a second request, robustly.
/// - **At half the refresh rate the margin is one nanosecond.** An interval of a
///   second divided by 30 is 33,333,333ns for a true period of 33,333,333.33, and a
///   60Hz ask is 16,666,667ns, so every second ask clears the anchor by 1ns and the
///   rate holds — by rounding, not by design. That is recorded rather than relied on:
///   it is why a burst of one looks safe at 30 fps and fails at 60.
///
/// One interval of slack (`allow_burst(2)`) is the equivalent of the cap's
/// quarter-interval tolerance, and is why a rate the display's refresh period divides
/// should not be asked for with a burst of one.
///
/// The asks here are a real display's: 60Hz, ±0.3ms, alternating, so the average period
/// is exactly the one asked for and only the individual asks waver.
#[test]
fn a_rate_at_a_divisor_of_the_refresh_needs_slack() {
    /// Admitted asks over two seconds of a jittered 60Hz grid.
    fn admitted(target_fps: u32, burst: u32) -> usize {
        let clock = FakeClock::default();
        let mut pipe = throttled(
            RatePolicy::at(fps(target_fps)).allow_burst(fps(burst)),
            &clock,
        );
        let metrics = metrics(true);
        let mut admitted = 0;
        for ask in 0..120 {
            if ask > 0 {
                let jitter = if ask % 2 == 0 { 0.0003 } else { -0.0003 };
                clock.advance(Duration::from_secs_f64(1.0 / 60.0 + jitter));
            }
            if pipe.should_render(true, &metrics) {
                admitted += 1;
            }
        }
        admitted
    }

    // 120 asks is two seconds at 60Hz. Asked for the refresh rate itself, a burst of
    // one answers every second ask: thirty frames a second for a sixty frame a second
    // request.
    let sixty_burst_one = admitted(60, 1);
    // Asked for half the refresh rate, it holds — on the nanosecond described above,
    // which is why the assertion is a floor and not a count.
    let thirty_burst_one = admitted(30, 1);
    println!(
        "0.3ms jitter on 60Hz, burst 1: 60 fps -> {sixty_burst_one}, 30 fps -> {thirty_burst_one} of 120 asks"
    );
    assert!(
        sixty_burst_one < 90,
        "60 fps asked with a burst of one admitted {sixty_burst_one} of 120 asks"
    );
    assert!(
        thirty_burst_one >= 55,
        "30 fps asked with a burst of one should hold, but admitted {thirty_burst_one} of 120 asks"
    );

    // One interval of slack holds the rate in both cases.
    let sixty_burst_two = admitted(60, 2);
    let thirty_burst_two = admitted(30, 2);
    assert!(
        sixty_burst_two >= 115,
        "a burst of two should hold 60 fps, but admitted {sixty_burst_two} of 120 asks"
    );
    assert!(
        (60..=63).contains(&thirty_burst_two),
        "a burst of two should hold 30 fps, but admitted {thirty_burst_two} of 120 asks"
    );
}

/// A burst is spent once and cannot lift the sustained rate.
///
/// A burst is counted in whole intervals, so the smallest step above none is a full
/// interval — there is no quarter-interval to ask for, which is how one interval of
/// slack both fixes the divisor case above *and* stays a burst. The temptation is to
/// read the knob as a rate knob, and this test is why it is not one: asked for 30 fps
/// on a 60Hz grid (an ask every 16.7ms), a burst of two admits one extra frame at the
/// front and then settles on every second ask, because one 16.7ms ask of slack is spent
/// reaching a 33.3ms interval. A burst of four spends three extra frames at the front
/// and settles on the same rate.
#[test]
fn a_burst_does_not_raise_the_sustained_rate() {
    fn admitted_at(burst: u32) -> usize {
        let clock = FakeClock::default();
        let mut pipe = throttled(RatePolicy::at(fps(30)).allow_burst(fps(burst)), &clock);
        let metrics = metrics(true);
        let mut admitted = 0;
        for ask in 0..120 {
            if ask > 0 {
                clock.advance(Duration::from_secs_f64(1.0 / 60.0));
            }
            if pipe.should_render(true, &metrics) {
                admitted += 1;
            }
        }
        admitted
    }

    // Two seconds at 30 fps is sixty frames. A burst of two lands on sixty-one and a
    // burst of four on sixty-three: the whole difference is the frames at the front,
    // and neither is the 120 a raised rate would be.
    let (burst_two, burst_four) = (admitted_at(2), admitted_at(4));
    println!(
        "30 fps asked on a 60Hz grid: burst 2 -> {burst_two}, burst 4 -> {burst_four} of 120 asks"
    );
    assert!(
        (60..=62).contains(&burst_two),
        "a burst of two delivered {burst_two} frames in two seconds at 30 fps"
    );
    assert!(
        (60..=65).contains(&burst_four),
        "a burst of four delivered {burst_four} frames in two seconds at 30 fps"
    );
}

/// A target that is not a whole number of asks still lands on a cycle, and the cycle
/// is the display's.
///
/// 24 fps on a 60Hz grid is the case worth naming, because it is the one people have a
/// word for: 24/60 is 2/5, so the pattern repeats every five asks with two frames — the
/// 3:2 pulldown, a hold of three refreshes and a hold of two, alternating. The throttle
/// knows nothing about pulldown; it is what a carried schedule on a uniform grid does
/// with a rational ratio. What it needs to get there is the slack from `allow_burst(2)`:
/// with a burst of one the same target answers every third ask and delivers 20 fps.
#[test]
fn a_rate_between_asks_lands_on_the_grids_cycle() {
    fn admits(target_fps: u32, burst: u32) -> Vec<u32> {
        let clock = FakeClock::default();
        let mut pipe = throttled(
            RatePolicy::at(fps(target_fps)).allow_burst(fps(burst)),
            &clock,
        );
        let metrics = metrics(true);
        let mut admitted = Vec::new();
        for ask in 0..300 {
            if ask > 0 {
                clock.advance(Duration::from_secs_f64(1.0 / 60.0));
            }
            if pipe.should_render(true, &metrics) {
                admitted.push(ask);
            }
        }
        admitted
    }

    // 300 asks is five seconds at 60Hz. Without slack, 24 fps is every third ask:
    // twenty frames a second for a twenty-four frame a second request.
    let without = admits(24, 1);
    let rate = without.len() as f64 / 5.0;
    assert!(
        (19.0..=21.0).contains(&rate),
        "24 fps asked with a burst of one delivered {rate:.1} fps over five seconds"
    );

    // With one interval of slack it lands on the 3:2 cycle: five seconds at 24 fps is
    // 120 frames, and after the burst's one frame at the front no frame is held for
    // less than two asks or more than three.
    let with = admits(24, 2);
    let gaps: Vec<u32> = with.windows(2).map(|w| w[1] - w[0]).collect();
    println!(
        "24 fps on 60Hz, burst 2: {} frames, first gaps {gaps:?}",
        with.len()
    );
    assert!(
        (118..=122).contains(&with.len()),
        "24 fps asked with a burst of two delivered {} frames in five seconds",
        with.len()
    );
    assert!(
        gaps[1..].iter().all(|gap| (2..=3).contains(gap)),
        "the 3:2 cycle should hold every frame for two or three asks, got {gaps:?}"
    );
}

/// A from-last rule can only lock to a whole number of asks, and this is why 24 fps
/// on a 60Hz grid needs something that remembers more.
///
/// `ThrottledPipeline` decides from one number, `now - last_frame`, and has no memory
/// of anything older. The only pattern such a rule can settle into is a fixed number
/// of asks per frame — and 24 fps on a 60Hz grid is 2.5 asks per frame, which is not a
/// fixed number. Its quarter-interval tolerance settles on two asks (30 fps) and no
/// tolerance settles on three (20). Neither is the 24 that was asked for, and no choice
/// of tolerance produces the alternating 3,2 that is: the fraction has nowhere to be
/// held. A carried schedule is that place, which is the difference the cycle test above
/// measures.
///
/// The rule is transcribed rather than called, because the shipped one reads
/// `Instant::now()` and so cannot be driven by the fake clock the rest of these tests
/// run on. The transcription is its `should_render` without the pass-through:
/// `now - last >= interval - interval/4`, with the tolerance set to zero for the second
/// column.
#[test]
fn a_from_last_rule_locks_to_a_whole_number_of_asks() {
    /// Admits over five seconds of a 60Hz grid for a 24 fps target, by a rule that
    /// remembers only the previous frame and allows `tolerance` of the interval back.
    fn admits(tolerance: f64) -> usize {
        let clock = FakeClock::default();
        let step = Duration::from_secs_f64(1.0 / 60.0);
        let allowed = Duration::from_secs_f64(1.0 / 24.0).mul_f64(1.0 - tolerance);
        let mut now = Duration::ZERO;
        let mut last: Option<Duration> = None;
        let mut admitted = 0;
        for ask in 0..300 {
            if ask > 0 {
                clock.advance(step);
                now += step;
            }
            let allow = match last {
                None => true,
                Some(last) => now - last >= allowed,
            };
            if allow {
                last = Some(now);
                admitted += 1;
            }
        }
        admitted
    }

    let shipped = admits(0.25) as f64 / 5.0;
    let strict = admits(0.0) as f64 / 5.0;
    println!(
        "24 fps by a from-last rule on 60Hz: shipped tolerance -> {shipped:.0} fps, none -> {strict:.0} fps"
    );
    assert!(
        (29.0..=31.0).contains(&shipped),
        "the shipped quarter-interval tolerance should overshoot to 30 fps, got {shipped:.1}"
    );
    assert!(
        (19.0..=21.0).contains(&strict),
        "no tolerance should undershoot to 20 fps, got {strict:.1}"
    );
}

/// A target the grid divides is every Nth ask, and a burst of one gives exactly that.
///
/// Asked once per refresh — which is what a display does — 30 fps on a 60Hz panel is
/// phase-locked to every second ask and 60 fps to every ask, both with nothing between.
/// This test fixes that much, at the burst of one it is written with.
///
/// It is not the limit of the seam, though, and the test above is the correction: a
/// target the grid does *not* divide is reachable too, as a cycle rather than an every-
/// Nth, once there is an interval of slack to carry the remainder. Without that slack
/// 45 fps on 60Hz — 22.2ms against a 16.6ms grid — is not reachable at all, and the
/// throttle settles on every second ask and delivers 30.
///
/// What a vsync-counting pacer gives that this cannot is a *fixed* every-Nth boundary at
/// a chosen rate; it pays for the cadence with a rate as wrong as its rounding.
#[test]
fn the_grid_is_what_the_asks_are_made_of() {
    /// Asked once per 60Hz refresh, the asks that are admitted.
    fn admitted_at(target_fps: u32) -> Vec<u32> {
        let clock = FakeClock::default();
        let mut pipe = throttled(RatePolicy::at(fps(target_fps)), &clock);
        let metrics = metrics(true);
        let mut admitted = Vec::new();
        for ask in 0..120 {
            if ask > 0 {
                clock.advance(Duration::from_secs_f64(1.0 / 60.0));
            }
            if pipe.should_render(true, &metrics) {
                admitted.push(ask);
            }
        }
        admitted
    }

    // A rate the refresh period divides: every second ask, exactly.
    let half = admitted_at(30);
    assert!(
        half.windows(2).all(|w| w[1] - w[0] == 2),
        "30 fps on a 60Hz grid should be every second ask, got {half:?}"
    );

    // A rate it does not divide quantises down to one it does. 45 fps is 22.2ms on a
    // 16.6ms grid; the throttle delivers 30, not 45 and not an irregular mixture.
    let forty_five = admitted_at(45);
    let delivered = 60.0
        / (forty_five.windows(2).map(|w| w[1] - w[0]).sum::<u32>() as f64
            / (forty_five.len() - 1) as f64);
    assert!(
        (delivered - 30.0).abs() < 0.5,
        "45 fps asked on a 60Hz grid, {delivered:.1} delivered, from {forty_five:?}"
    );
}

/// Under a stream of asks as dense as the CPU can make them, the rate holds.
///
/// The fake clock cannot be used for this — it is the *real* clock's job to show
/// that the rate is a rate and not an arithmetic identity — so this one is the rare
/// test whose subject is wall-clock time. The band is wide on purpose: what is
/// being tested is that the throttle is not systematically over or under, not the
/// precision of a scheduler.
#[test]
fn the_rate_holds_under_a_dense_stream_of_asks() {
    let mut pipe = PassPipeline::new(
        StandardImmediatePipeline,
        RatePolicy::at(fps(30)),
        Ledger::shared(8),
    );
    let metrics = metrics(true);

    let window = Duration::from_millis(500);
    let start = Instant::now();
    let mut admitted = 0u32;
    while start.elapsed() < window {
        if pipe.should_render(true, &metrics) {
            admitted += 1;
        }
    }

    let rate = f64::from(admitted) / window.as_secs_f64();
    assert!(
        (27.0..=33.0).contains(&rate),
        "asked for 30 fps, got {rate:.1}"
    );
}
