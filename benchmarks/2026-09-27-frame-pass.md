# A frame pass: holding the rate, and saying what it cost — 2026-09-27

The frame-pipeline seam had no second implementation of its own. This is one, and it
answers the two questions a pipeline is in a position to answer: whether an ask does
the frame's work, and what the passes of the frames that got through actually cost.

It is a **wrap**, not a swap — it decorates whatever pipeline is already installed
rather than replacing the layout engine, the text system or anything else. It depends
on `bite-gp-authoring` at runtime and on nothing else, and its decision is measured
through a plain `WindowMetrics`, so nothing here needs a display.

## Commands

```sh
cargo run --release --example pass_cost
cargo bench --bench frame_pipeline

cargo test --test throttling -- --nocapture
cargo test --test ledger
cargo test --features test-support --test facade -- --nocapture
cargo test --features puffin,test-support --test puffin -- --nocapture
cargo run --example puffin_scopes --features puffin,test-support

# the windowed examples need a display to run, but compile without one
cargo check --examples --features demo
```

| | |
| --- | --- |
| toolchain | `rustc 1.98.1 (48a229cea 2026-09-01)`, this repository's own pin, the same one `bite-gpui/gpui_parley` and `bite-gpui/gpui_morphorm` declare |
| host | Intel i7-8750H, 12 threads, Linux 7.0.0 x86_64 — **not a controlled benchmarking host**: a laptop CPU, no pinning, no fixed clocks |
| samples | criterion defaults, 100 measurements per benchmark |
| crates | `bite-gp-authoring` `1.21.0` (the trait and the metrics), `bite-gp-runtime` `1.21.0` (the first column), `criterion` `0.5.1` |

## The rate, on a display's own grid

The asks arrive at the display's cadence, so a rate the refresh period divides is
every Nth ask. Five seconds of a 60 Hz grid is 300 asks:

| rule | drawn | delivered |
| --- | --- | --- |
| `PassPipeline`, 60 fps, burst 2 | 300 of 300 | 60.0 fps |
| `PassPipeline`, 30 fps, burst 2 | 61 of 120 (two seconds) | 30.0 fps |
| `PassPipeline`, 24 fps, burst 2 | **121** of 300 | 24.2 fps |
| the from-last rule, ¼-interval tolerance | 150 of 300 | 30.0 fps |
| the from-last rule, no tolerance | 100 of 300 | 20.0 fps |

The last three rows are the finding. 24 fps asks for 2.5 refreshes a frame, and a rule
that compares against the previous frame has nowhere to keep the half — its tolerance
rounds to two refreshes (30 fps) and no tolerance rounds to three (20 fps). The pass
carries the remainder, so the cadence it lands on is the 3:2 pulldown: holds of three
refreshes and two, alternating, for 121 frames where 120 is exact. All five rows are
assertions in `tests/throttling.rs` and `tests/facade.rs`, not one-off runs.

## The rate, under a synthetic dense stream

`examples/pass_cost.rs` asks both rules as fast as the CPU can — deliberately **not** a
display — and counts what each lets through:

| column | ns/decision | frames a sec |
| --- | --- | --- |
| `ThrottledPipeline` (shipped) | 23.7 | **40.00** |
| `PassPipeline` (this crate) | 27.8 | **30.00** |

Both are asked for 30 fps. The pass holds 30.00; the shipped cap lets 40.00 through,
because its tolerance — a quarter of the interval, unconditionally — admits a frame
25% early, and with asks this dense every early admission lands. **This row is a
synthetic stream, not a display**: on a 60 Hz grid the asks are quantised and the
shipped cap holds its divisor (the 30 fps row above), which is why the grid numbers
are the ones the tests assert and this one is labelled for what it is.

## The cost of the decision

The decision is one comparison against a carried schedule, and a refusal is the path a
throttled window spends its time in. Criterion, median with its 95% interval:

| group | `ThrottledPipeline` | `PassPipeline` |
| --- | --- | --- |
| `should_render` | 24.242–25.027 ns (24.613) | 27.175–27.988 ns (27.575) |
| `should_render_inactive` (the policy consulted) | — | 27.249–27.937 ns (27.543) |

One outlier in each of the second and third groups (1 of 100). So the pass costs
**roughly 3 ns, or 1.12×, more per decision** than the cap it replaces — and reading
the policy per frame, which is what lets one install pace two windows differently, is
inside the noise of not reading it at all.

## The scope emitter

A summary is one shape a measurement takes; a timeline is the other. `PuffinPipeline`
(behind the `puffin` feature, off by default) is a decorator that emits one
[puffin](https://github.com/EmbarkStudios/puffin) scope per pass it forwards and opens a
frame boundary at the top of each frame, so the same passes that fill the ledger can be
dragged across in a viewer. It is pure Rust, so no consumer gains a C++ toolchain, and
its scopes are recorded in-process — which is the property that makes it testable.

That is what `tests/puffin.rs` does and why it is a test at all. It turns scopes on,
drives real frames through the facade, and reads the frames back through
`puffin::GlobalFrameView`: the count per frame is asserted **exactly**, seven — one scope
per pass of `FramePipeline::draw` — with the decision deliberately unscoped, so a
forgotten pass and an extra scope each fail the count. A frame is only readable once
another has begun, which is puffin's boundary semantics, not this crate's.

`examples/puffin_scopes.rs` is the same data the other way round: still no display, but
it prints each frame's scopes with their spans rather than counting them — the waterfall a
viewer draws, read back in-process. Both are runnable here, which is the point.

The spans are the **passes** — on a dev build, over a view small enough to run in a test —
and never a frame time. A 60 fps frame is 16.67 ms; the passes fill a slice of it, and the
GPU submit and the swap that fill the rest live below the seam, where a pipeline cannot see
them. That boundary is the same one `budget()` is careful about, and the same one that makes
the crate a wrap rather than a pacer.

The number here is a count, not a duration. A scope's cost is puffin's to characterise
(and is a relaxed atomic load when the profiler is off, which is the default), and a test
fast enough to assert a scope's *duration* is a test that cannot see the frames — so this
file asserts structure and leaves timing to the profiler.

## What it says

**The rate is the right rate, not a quantised one.** A rate the refresh period does
not divide is deliverable, and it is the one capability the shipped cap provably lacks.

**A throttle and an instrumentation cell are cheaper together than apart to reason
about.** The deferral count is only available because the decision and the ledger are
the same object; a `PhaseMetrics` that counts drawn frames cannot see the asks that
were passed over.

**The overhead is a few nanoseconds per decision, on the deferred path.** It is not
free, and it is not the thing to optimise before the rate is right.

## What it does not say

- **Not a general claim.** One host, one clock, and a laptop CPU with no pinning.
- **`ns/decision` is a refusal-path measurement.** Almost every call after the first is
  a refusal, in the synthetic loop *and* in the criterion benchmark, so the number
  characterises the path a throttled window is in most of the time — not the admitted
  path, which is one comparison every interval.
- **Not a statement about presentation.** The seam can defer work; it cannot place a
  pixel on a vblank, and nothing here measures what reaches the display.

## Files

- `src/gpui_pass.rs` — `PassPipeline`: the decision, the pass timing, and the clocks.
- `src/policy.rs` — `Rate`, `RatePolicy`: a sustained rate and a burst allowance.
- `src/ledger.rs` — `Phase`, `FrameCost`, `Ledger`: the ring, the deferrals, the
  percentiles and the budget.
- `src/puffin_pipeline.rs` — `PuffinPipeline`: a scope per pass and a frame boundary
  between frames, behind the `puffin` feature.
- `tests/throttling.rs` — what each rule does to a stream of asks, on a clock the test
  owns.
- `tests/ledger.rs` — deferrals and the budget, with no window.
- `tests/facade.rs` — the engine drives it: frames drawn, passes timed.
- `tests/puffin.rs` — a scope per pass, read back in-process with the profiler on.
- `examples/pass_demo.rs` — a window, the rate, and the ledger live.
- `examples/pass_lab.rs` — two windows: one whose scene is dialled by knobs, one that draws what
  each pass of it cost. Needs a display to run.
- `examples/pass_ticket.rs` — the ledger as a ticket, driven through the engine's own
  frame loop headlessly, so it runs where the windowed demo cannot.
- `examples/puffin_scopes.rs` — the scopes as a waterfall, headless, behind the `puffin`
  feature: the same frames `tests/puffin.rs` counts, printed with their spans.
- `examples/pass_cost.rs` — the dense-stream comparison above.
- `benches/frame_pipeline.rs` — the criterion timings above.
