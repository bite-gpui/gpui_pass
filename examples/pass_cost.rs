//! What each throttle costs per decision, and what rate each one actually holds.
//!
//! Two implementations of one trait, asked the same two questions through the same
//! call — which is the shape `.uses/README.md` requires of a number: one
//! implementation measured alone is a property of that library, and two through the
//! same boundary on the same input is a property of the architecture.
//!
//! It needs no window and no display. `should_render` is handed a `WindowMetrics`,
//! which is a plain value with public fields, so the decision itself is measurable
//! on a build machine — and so is what each throttle does with a stream of asks.
//!
//! Run: `cargo run --release --example throttle_cost`

use std::hint::black_box;
use std::time::{Duration, Instant};

use gpui_authoring::{FramePipeline, StandardImmediatePipeline, WindowMetrics};
use gpui_pass::{Ledger, PassPipeline, RatePolicy};
use gpui_runtime::ThrottledPipeline;
use gpui_types::{Bounds, Pixels, Point, Size};

/// The rate both columns are asked for. 30 is the interesting one: it is an exact
/// half of a 60Hz display's frame period and a quarter of a 120Hz one, which is
/// where an interval-based cap has to decide what to do about the boundary.
const FPS: u32 = 30;

/// How long each column is driven at, for the rate it holds.
const WINDOW: Duration = Duration::from_secs(1);

/// How many decisions the cost measurement makes per column.
const DECISIONS: u32 = 2_000_000;

/// A metrics value for a window in the given state.
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

fn rate(fps: u32) -> core::num::NonZeroU32 {
    core::num::NonZeroU32::new(fps).expect("a rate above zero")
}

/// The two columns, each as the facade installs it: a pipeline behind the trait.
fn columns() -> Vec<(&'static str, Box<dyn FramePipeline>)> {
    vec![
        (
            "ThrottledPipeline (shipped)",
            Box::new(ThrottledPipeline::new(StandardImmediatePipeline, FPS)),
        ),
        (
            "PassPipeline (this crate)",
            Box::new(PassPipeline::new(
                StandardImmediatePipeline,
                RatePolicy::at(rate(FPS)),
                Ledger::shared(Ledger::DEFAULT_CAPACITY),
            )),
        ),
    ]
}

/// Nanoseconds per `should_render` call, in a loop that asks as fast as it can.
///
/// The loop's own cost is in here — it is the same loop for both columns, which is
/// what makes the difference between them the finding. Almost every call after the
/// first is a refusal, so this is the cost of the *deferred* path, which is the one
/// a throttled window spends its time in.
fn decision_cost(pipe: &mut dyn FramePipeline) -> f64 {
    let metrics = metrics(true);
    // Warm up, so the first column measured does not carry the cache's filling.
    for _ in 0..100_000 {
        black_box(pipe.should_render(true, &metrics));
    }

    let start = Instant::now();
    for _ in 0..DECISIONS {
        black_box(pipe.should_render(true, &metrics));
    }
    start.elapsed().as_nanos() as f64 / f64::from(DECISIONS)
}

/// The frames a second the column lets through when it is asked far more often
/// than it can draw.
///
/// This is a display's own situation: the platform asks at its refresh rate
/// whatever the pipeline does, so a rate narrower than the refresh rate is holding
/// frames back on purpose — and a window with an animation is dirty on every ask,
/// which is what this loop is.
fn held_rate(pipe: &mut dyn FramePipeline) -> f64 {
    let metrics = metrics(true);
    let start = Instant::now();
    let mut admitted = 0u32;
    while start.elapsed() < WINDOW {
        if pipe.should_render(true, &metrics) {
            admitted += 1;
        }
    }
    f64::from(admitted) / start.elapsed().as_secs_f64()
}

fn main() {
    println!("asked for {FPS} fps, over {DECISIONS} decisions and {WINDOW:?} of asks\n");
    println!(
        "{:<32} {:>14} {:>14}",
        "column", "ns/decision", "frames a sec"
    );
    println!("{}", "-".repeat(62));

    for (name, mut pipe) in columns() {
        let ns = decision_cost(pipe.as_mut());
        let held = held_rate(pipe.as_mut());
        println!("{name:<32} {ns:>14.1} {held:>14.2}");
    }

    println!(
        "\nns/decision is dominated by the refusal path; the admitted path is one check\n\
         every interval. frames a sec is what the column actually lets through when the\n\
         asks are denser than the rate it was asked for."
    );
}
