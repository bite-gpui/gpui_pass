# gpui_pass

A **wrap**: a `FramePipeline` decorator for GPUI that throttles a window's frame
rate by a policy and keeps a ledger of what each pass cost.

`gpui_authoring::FramePipeline` is the seam. A window hands its frame to one, and
every pass — `begin_frame`, `evaluate_roots`, `layout_roots`, `paint_roots`, and the
three that close the frame out — goes through it in turn. Before any of that work
starts, the pipeline is asked one question: does this ask do the frame, or does the
window stay dirty and the work stay pending? Those are the only two things a
pipeline is in a position to do, and this crate does both.

- **Decide.** `should_render` is the throttle. A `RatePolicy` is read per frame from
  the `WindowMetrics` the seam already hands in, so one install can run a focused
  window at 60 and a backgrounded one at 15.
- **Account.** Every pass is timed into a `Ledger`, which keeps the recent frames
  rather than a running average, counts the asks the throttle passed over to draw
  them, and answers percentiles and a budget derived from the rate.

Both read the same injected `Clock`, and that is the point: install `RealClock` and
the numbers are the machine's; install `FakeClock` and they are a test's.

## A wrap, not a swap

The project extends the engine two ways. A **swap** replaces what does the work —
one implementation of a seam, chosen at startup, exclusive. A **wrap** decorates how
the work is run: it is handed every pass, forwards the ones it does not change, and
stacks under anything else that wraps a pipeline. This crate is a wrap. It never
replaces the layout engine, the text system or anything else; it wraps whatever
pipeline you already have.

## Package and library names

The **package** is `bite-gp-pass`, the name the distribution's rule gives it
(`gpui_X` → `bite-gp-X`), and the **library** is `gpui_pass`, which is what code
names. They differ because a package identifier carries its namespace while a
library target keeps the upstream name — so the manifest key is the package name and
`use gpui_pass::…` compiles unchanged, with no rename required. That is invariant 2
of the distribution, applied to a crate that lives outside it.

## Using it

```toml
[dependencies]
bite-gp-pass = "1.21"
```

```rust
use core::num::NonZeroU32;
use gpui_pass::{FramePipelineExt, Ledger, RatePolicy};

let ledger = Ledger::shared(Ledger::DEFAULT_CAPACITY); // 240 frames, allocated once

gpui::application()
    .with_frame_pipeline({
        let ledger = ledger.clone();
        move |_window_id| {
            Box::new(StandardImmediatePipeline.pass(
                RatePolicy::at(NonZeroU32::new(60).unwrap())
                    .inactive_at(NonZeroU32::new(15).unwrap()),
                ledger.clone(),
            ))
        }
    })
    .run(...);
```

Read the ledger between frames — it takes a `RefCell` borrow:

```rust
let ledger = ledger.borrow();
let passed_over = ledger.deferral_ratio();                  // what the throttle turned away
let p99_paint   = ledger.phase_percentile(Phase::Paint, 0.99); // the tail, not the mean
let over_budget = ledger.over_budget(budget);               // frames past the rate's interval
```

`budget()` is the interval the policy implies — 16,666,666 ns at 60 fps — and it is a
yardstick for **the passes**, not for the whole frame: the platform still has to
present what the passes produced.

## What it gives you that timing alone cannot

`gpui_runtime` ships `ThrottledPipeline` and `InstrumentedPipeline`, and both are
worth having. Neither can connect the two halves, and the connection is the
substance:

- **A throttled frame and a slow frame look identical to instrumentation that only
  sees drawn frames.** `PhaseMetrics` counts frames, not asks, so a throttle passing
  over half the display's refreshes is invisible to it. Here a frame carries the
  count of asks it stood in for (`FrameCost::deferred_since_previous`).
- **The average hides the tail.** `Ledger::phase_percentile` reads a p99 out of the
  ring; a mean cannot.
- **The budget falls out of the rate.** No second knob to keep in step with the
  policy.
- **It is exercisable in CI.** A fake clock drives the decisions *and* the recorded
  costs, so a frame budget is an assertion.

## What it is not

- **Not a pacer.** The seam names no swapchain, no vblank and no timestamp, and
  `WindowMetrics` carries no refresh rate — so what emerges is the right *rate*
  landing on the display's grid, never a lock to the raster. Where a rigid cadence
  matters, count refreshes in the platform.
- **Not a timeline.** The ledger keeps a summary — what each pass cost, how many asks
  the throttle passed over, where the recent frames sit against the budget — and nothing
  *between* the passes: not a syscall, not a GPU wait, not which view was slow. A scope
  per pass on a flamegraph is the other shape a measurement takes, and it is a separate
  decorator behind the `puffin` feature (below).
- **Not a general frame-rate cap only.** A cap that compares against the last frame
  cannot express a rate the refresh period does not divide; a carried schedule can,
  because the remainder shorter than an interval is kept rather than rounded. 24 fps
  on a 60 Hz grid comes out as the 3:2 pulldown: **121 frames in five seconds**,
  where the from-last rule lands on 150 (30 fps) or 100 (20 fps) — neither of which
  is 24.

## The `puffin` feature (a timeline, off by default)

The ledger summarises. A timeline is the other shape a measurement takes, and it is
`PuffinPipeline`, behind the `puffin` feature: a decorator that emits one
[puffin](https://github.com/EmbarkStudios/puffin) scope for every pass it forwards —
`gpui::begin_frame`, `gpui::evaluate_roots`, `gpui::layout_roots`, `gpui::paint_roots`,
and the three that close the frame out — and opens a frame boundary at the top of each
frame. Wrap whatever pipeline you already have:

```rust
use gpui_pass::PuffinPipeline;

let pipeline = PuffinPipeline::new(
    StandardImmediatePipeline.pass(policy, ledger.clone()),
);
```

It composes the way any decorator does: a scope measures whatever it encloses, so wrap
the pass in the emitter to see the throttle's frames on the timeline, and stack the two
in either order. `should_render` is deliberately left unscoped — it is asked once per
*ask*, not once per drawn frame, and a scope around it would report a per-ask duration as
if it were part of a frame.

What the scopes measure is the **passes**. The GPU submit, the swap and the vblank are
below the seam — `SceneRenderer` is reached only through the platform window — so a scope
is a pass's CPU cost, never a frame time. A 60 fps frame is 16.67 ms; the passes fill a
slice of it and the platform presents the rest. `examples/puffin_scopes.rs` prints exactly
that, labelled.

Two things made puffin the choice over a C++ profiler client, and the second is the
load-bearing one. It is pure Rust, so a consumer links nothing it was not already
building — no C++ toolchain dragged into every build. And it records scopes
**in-process**, so a test can turn them on, drive real frames through the pipeline, and
read the frames back: `tests/puffin.rs` does exactly that and asserts one scope per pass.
A profiler whose data exists only inside an external GUI cannot be checked in CI, and
neither could the crate that would have carried one.

Emitting the scopes is this crate's half. Getting them to a viewer is the caller's:
`puffin_http` serves them over TCP for `puffin_viewer`, or `puffin_egui` draws them
inside the application. The sink is whatever `puffin::GlobalProfiler::lock().add_sink`
was handed, and this crate depends on none of them.

## Examples and tests

```sh
cargo test                                    # the ledger's arithmetic, and the schedule
cargo test --test ledger                      # deferrals and the budget, no window needed
cargo test --test throttling                  # the admission rule, on a clock the test owns
cargo test --features test-support --test facade   # ...and that the engine can drive it
cargo test --features puffin,test-support --test puffin  # one scope per pass, read back headlessly

cargo run --example pass_demo --features demo        # a window; `-- naive` for the shipped cap
cargo run --example pass_lab --features demo         # two windows: a scene you dial, and its pass costs
cargo run --example pass_ticket --features test-support  # the ledger as a ticket, headless
cargo run --example puffin_scopes --features puffin,test-support  # one scope per pass, printed headlessly
cargo run --release --example pass_cost              # what each throttle costs per decision
```

`test-support` pulls in the facade and moves gpui's frame drawing onto its test path,
which is why it is a feature rather than a dev-dependency: a dev-dependency's
features are unified into every dev-context build, and a build that is not running
tests should not be able to pick that path up by accident.

A recorded run, with the numbers these print and what they mean, is in
[`benchmarks/2026-09-27-frame-pass.md`](benchmarks/2026-09-27-frame-pass.md).

## Where this code came from

The crate was built outside the tree for the frame-pipeline seam, and it changed
shape twice before it was published. It began as `gpui_governor`, a rate limiter
built on `governor`'s GCRA, on the theory that frame pacing is a rate-limiting
problem; the limiter turned out to be the wrong tool for the seam, so it was dropped
and the schedule carried in the crate. It was `gpui_throttle` while it was only a
throttle, and became `gpui_pass` when it took on the ledger — because what it does
is not pacing and not only throttling, it is a *pass*: one place every stage of a
frame goes through, deciding and accounting.

Nothing in it needs privileged access to the engine. It depends on exactly one crate
at runtime — `bite-gp-authoring`, for the trait, the metrics and the passes it
forwards — and its decision is measured through a plain `WindowMetrics`, which is
why a benchmark needs no display.

## Versioning

The distribution's scheme, `major.minor.(patch * 100 + amendment)`, puts `1.21.1` at
`bite-gp-parley` and `1.21.2` at `bite-gp-morphorm`; this crate holds the next slot,
`1.21.3`.

**On crates.io as `1.21.3`.** The manifest carries the last released version, and the
release workflow bumps *from* it: `1.21.3` now, one `amendment` away from `1.21.4`. A
first release needs the base rather than the target, which is why the manifest read
`1.21.2` until the release commit wrote `1.21.3`.

## Publishing

One dispatch does the whole release — the bump, the tag and the upload — from
`.github/workflows/release.yml`. Nothing is written to the repository until the
publishing job runs, and that job sits behind the `crates-io` environment, so a dispatch
does not touch the branch or the registry on its own.

```sh
# what would be released, without releasing it (dry_run defaults to true)
gh workflow run release.yml -f bump=amendment

# the release: repeat the tag the run logged to confirm it
git pull && gh workflow run release.yml -f bump=amendment -f dry_run=false -f confirm=v1.21.4
```

`bump` is one of `amendment`, `patch`, `minor` or `none`. The first three move the slot
they name and refuse a version that is already tagged; `none` re-releases the version the
manifest already carries, which is the recovery for an upload that failed after the tag
was pushed.

**The environment has no required reviewers configured yet**, so `dry_run=false` goes
straight to the upload — see `gpui_parley`'s README for what that gate is meant to be.
And a token for this crate has to be scoped to a name prefix that covers `bite-gp-pass`:
`bite_*` does not, because of the hyphen.

## Licence

Apache-2.0 — see `LICENSE-APACHE`.
