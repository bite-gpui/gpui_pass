//! What one `should_render` costs, for each of the two throttle rules.
//!
//! Two implementations of one trait, through the same call, on the same
//! `WindowMetrics` — the two columns `.uses/README.md` asks of a number. Almost
//! every call after the first is a refusal, so what this times is the *deferred*
//! path, which is the one a throttled window spends its time in.
//!
//! It needs no platform: `WindowMetrics` is a plain value with public fields, so the
//! decision is measurable on a build machine. That also keeps this benchmark light
//! enough to run where the full stack cannot be built.
//!
//! Run: `cargo bench --bench frame_pipeline`

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};

use gpui_authoring::{FramePipeline, StandardImmediatePipeline, WindowMetrics};
use gpui_pass::{Ledger, PassPipeline, RatePolicy};
use gpui_runtime::ThrottledPipeline;
use gpui_types::{Bounds, Pixels, Point, Size};

/// The rate both columns are asked for.
const CAP: u32 = 30;

fn fps(n: u32) -> core::num::NonZeroU32 {
    core::num::NonZeroU32::new(n).expect("a rate above zero")
}

/// A metrics value for an active window, which is what both columns read here.
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

fn bench_decision(c: &mut Criterion) {
    let metrics = metrics();
    let mut group = c.benchmark_group("should_render");

    let mut shipped: Box<dyn FramePipeline> =
        Box::new(ThrottledPipeline::new(StandardImmediatePipeline, CAP));
    group.bench_function("ThrottledPipeline", |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            for _ in 0..iters {
                black_box(shipped.should_render(true, &metrics));
            }
            start.elapsed()
        })
    });

    let mut throttled: Box<dyn FramePipeline> = Box::new(PassPipeline::new(
        StandardImmediatePipeline,
        RatePolicy::at(fps(CAP)),
        Ledger::shared(Ledger::DEFAULT_CAPACITY),
    ));
    group.bench_function("PassPipeline", |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            for _ in 0..iters {
                black_box(throttled.should_render(true, &metrics));
            }
            start.elapsed()
        })
    });

    group.finish();
}

/// The policy is read per frame, so the throttled decision is measured with a policy
/// that has to be consulted rather than one that is skipped: an inactive window gets
/// the other quota.
fn bench_decision_with_a_policy(c: &mut Criterion) {
    let inactive = WindowMetrics {
        is_active: false,
        ..metrics()
    };
    let mut group = c.benchmark_group("should_render_inactive");

    let mut throttled: Box<dyn FramePipeline> = Box::new(PassPipeline::new(
        StandardImmediatePipeline,
        RatePolicy::at(fps(60)).inactive_at(fps(15)),
        Ledger::shared(Ledger::DEFAULT_CAPACITY),
    ));
    group.bench_function("PassPipeline", |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            for _ in 0..iters {
                black_box(throttled.should_render(true, &inactive));
            }
            start.elapsed()
        })
    });

    group.finish();
}

criterion_group!(benches, bench_decision, bench_decision_with_a_policy);
criterion_main!(benches);
