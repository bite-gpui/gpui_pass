//! A `FramePipeline` that throttles a window's frame rate and accounts for what
//! the passes cost.
//!
//! # The two things a pass can actually do
//!
//! A pipeline sits at one seam: it is asked, before any of a frame's work has
//! started, whether this ask does the work — and, if it does, every pass of the
//! frame is handed through it in turn. That leaves exactly two things it is in a
//! position to do, and this crate does both:
//!
//! 1. **Decide.** [`should_render`](FramePipeline::should_render) is the throttle.
//!    A [`RatePolicy`] says how often an ask is allowed to spend itself, read per
//!    frame from the [`WindowMetrics`] the seam hands in, so one install can
//!    throttle a focused window and a backgrounded one differently.
//! 2. **Account.** Every pass is timed into a [`Ledger`], which keeps the recent
//!    frames rather than a running average, and counts the asks the throttle
//!    passed over to draw them.
//!
//! # Why one crate and not two decorators
//!
//! The facade already ships decorators for each half — `ThrottledPipeline` and
//! `InstrumentedPipeline` — and they are worth having. What neither can do is
//! *connect* them, and the connection is the interesting part:
//!
//! - **A frame that cost too much and a frame that was held back look identical
//!   to instrumentation that only sees drawn frames.** The facade's
//!   `PhaseMetrics` counts frames, not asks, so a throttle that is passing over
//!   half the display's refreshes is invisible to it. Here the throttle and the
//!   ledger are the same object, so a frame carries the count of asks it stood in
//!   for: *this frame drew after three asks were passed over.*
//! - **The average hides the thing worth seeing.** A mean paint time of 2 ms
//!   says nothing about the p99 that hitches. The ledger keeps frames, so a tail
//!   is a query, not a spike in a graph you had to be watching.
//! - **The budget falls out of the rate.** The interval a [`RatePolicy`] implies
//!   — 16.67 ms at 60 fps — is the yardstick the frames are measured against, so
//!   "how many frames overran the budget this rate implies" needs no second knob.
//! - **One clock drives both.** The same [`Clock`] that decides whether an ask is
//!   too soon times the passes, so the whole pass is a function of the clock it
//!   was given. With [`FakeClock`], that means the decisions *and* the recorded
//!   costs are the test's — which is what lets CI assert a frame budget, the one
//!   thing the telemetry swaps that need an external profiler's GUI cannot do.
//!
//! # What this does not do
//!
//! It does not pace presentation, and nothing at this seam can. The asks arrive at
//! the platform's cadence, a drawn frame is handed over at
//! [`end_frame`](FramePipeline::end_frame), and the trait names no swapchain, no
//! vblank and no timestamp — so what emerges is the right *rate* landing on the
//! display's grid, not a lock to the raster. Where a rigid cadence matters more
//! than the rate, the tool is the platform's: count refreshes and draw every
//! `round(refresh / target)`th.
//!
//! Nor is the ledger a profiler. It sees the passes and nothing between them —
//! not a syscall, not a GPU wait, not which view was slow. What it sees, it sees
//! in-process, without a C++ toolchain and without a GUI to attach, which is the
//! trade: less reach than Tracy, and exercisable in CI.
//!
//! ```
//! use core::num::NonZeroU32;
//! use gpui_authoring::StandardImmediatePipeline;
//! use gpui_pass::{FramePipelineExt, Ledger, RatePolicy};
//!
//! let ledger = Ledger::shared(Ledger::DEFAULT_CAPACITY);
//! let piped = StandardImmediatePipeline.pass(
//!     RatePolicy::at(NonZeroU32::new(60).expect("a rate above zero"))
//!         .inactive_at(NonZeroU32::new(15).expect("a rate above zero")),
//!     ledger.clone(),
//! );
//! # let _ = (piped, ledger);
//! ```
//!
//! [`WindowMetrics`]: gpui_authoring::WindowMetrics

mod ledger;
mod policy;

pub use ledger::{FrameCost, Ledger, Phase};
pub use policy::RatePolicy;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_authoring::{App, FocusId, FramePipeline, PreparedRoots, Window, WindowMetrics};

use policy::Rate;

/// The clock a pass measures itself against.
///
/// A pass's decision is a question about time and its ledger is a record of time,
/// so a pipeline that answers both can be asked them with time under the caller's
/// control: a pass built on [`FakeClock`] steps through the frames of a test
/// rather than through the frames a display would have drawn.
pub trait Clock: Clone + 'static {
    /// Monotonic time since an origin of the clock's choosing. Only differences
    /// between two readings mean anything.
    fn now(&self) -> Duration;
}

/// The clock a window runs on: `Instant`, in the shape the pass wants.
#[derive(Clone)]
pub struct RealClock {
    origin: Instant,
}

impl RealClock {
    /// A clock whose zero is now.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for RealClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// A clock that stands still until it is told to move.
///
/// Cloning shares the reading, so a pipeline built with one advances when the test
/// that made it advances the original.
#[derive(Clone, Default)]
pub struct FakeClock {
    now: Rc<Cell<Duration>>,
}

impl FakeClock {
    /// Moves the clock forward by `by`.
    pub fn advance(&self, by: Duration) {
        self.now.set(self.now.get() + by);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        self.now.get()
    }
}

/// A pass: it throttles the pipeline it wraps by a [`RatePolicy`], and times every
/// pass of the frames that get through into a [`Ledger`].
///
/// Changes only [`should_render`](FramePipeline::should_render) and forwards every
/// other pass, so the pipeline underneath draws exactly as it would have.
///
/// A deferred frame is not a dropped one. `should_render` is asked again while the
/// window stays dirty, and a refusal *leaves the schedule where it was*, so a
/// deferral does not move the time the next frame is due. The schedule is the only
/// state the decision keeps, so it allocates nothing and its cost does not grow
/// with the number of frames drawn; the ledger's ring is a fixed allocation, made
/// once.
///
/// The clock is a type parameter with the real one as its default: the decision
/// and the timings are both questions about time, and a pipeline that answers them
/// can be asked those questions with time under the caller's control.
/// `PassPipeline<P>` is what an application installs; `PassPipeline<P, FakeClock>`
/// is what a test reasons with.
pub struct PassPipeline<P, C: Clock = RealClock> {
    inner: P,
    policy: RatePolicy,
    clock: C,
    /// The rate in force, so a policy that has not moved a rate can be told apart
    /// from one that has.
    rate: Rate,
    /// The time the next frame is due. A refusal leaves it exactly as it was, and an
    /// admission moves it on by one interval from the later of it and now — which is
    /// where a remainder shorter than an interval is carried instead of lost.
    next_allowed: Duration,
    ledger: Rc<RefCell<Ledger>>,
}

impl<P, C: Clock> PassPipeline<P, C> {
    /// Passes `inner` by `policy`, recording into `ledger`, against `clock`.
    ///
    /// The schedule starts empty, so the first ask is admitted: a window is drawn at
    /// the rate it was written for before anything has been measured against it.
    pub fn with_clock(inner: P, policy: RatePolicy, ledger: Rc<RefCell<Ledger>>, clock: C) -> Self {
        Self {
            inner,
            policy,
            clock,
            rate: policy.active_rate(),
            next_allowed: Duration::ZERO,
            ledger,
        }
    }

    /// The pipeline this one wraps.
    pub fn inner(&self) -> &P {
        &self.inner
    }

    /// The policy this pass is currently throttling by.
    pub fn policy(&self) -> &RatePolicy {
        &self.policy
    }

    /// The ledger this pass records into.
    pub fn ledger(&self) -> &Rc<RefCell<Ledger>> {
        &self.ledger
    }

    /// Replaces the policy, applying it from the next frame.
    ///
    /// A policy is data, so a window can be re-throttled while it runs — the reason
    /// the rate is a schedule and not an interval: a new rate takes effect without
    /// restarting the window. A change of rate starts the schedule again, so a window
    /// handed a new rate gets a full burst; a rate that has not moved is left alone
    /// precisely so that setting the same policy repeatedly costs nothing.
    pub fn set_policy(&mut self, policy: RatePolicy) {
        self.apply(policy.active_rate());
        self.policy = policy;
    }

    /// The rate the pass is holding right now, in whole frames a second.
    ///
    /// This is the active rate until a frame reports the window inactive, and the
    /// inactive rate from then on.
    pub fn rate(&self) -> u32 {
        self.rate.fps.get()
    }

    /// The interval the rate in force implies, which is the yardstick the ledger's
    /// frames are measured against.
    ///
    /// It is the budget for the *passes*, not for the whole frame: the platform
    /// still has to present what the passes produced, so a frame whose passes fill
    /// this has none of it left.
    pub fn budget(&self) -> Duration {
        self.rate.interval
    }

    /// Puts `rate` in force, starting the schedule again if it moved.
    ///
    /// Restarting from zero is what hands a window a full burst after a re-pace: the
    /// next ask is due immediately, and the ones after it are measured from there.
    fn apply(&mut self, rate: Rate) {
        if rate != self.rate {
            self.rate = rate;
            self.next_allowed = Duration::ZERO;
        }
    }

    /// Times `phase` from `start`, recording it into the open frame.
    ///
    /// Saturating, so a clock that somehow reads earlier than it did cannot take
    /// the process down over a measurement.
    fn record(&mut self, phase: Phase, start: Duration) {
        let cost = self.clock.now().saturating_sub(start);
        self.ledger.borrow_mut().record(phase, cost);
    }
}

impl<P> PassPipeline<P, RealClock> {
    /// Passes `inner` by `policy`, recording into `ledger`, against the real clock.
    pub fn new(inner: P, policy: RatePolicy, ledger: Rc<RefCell<Ledger>>) -> Self {
        Self::with_clock(inner, policy, ledger, RealClock::new())
    }
}

impl<P: FramePipeline, C: Clock> FramePipeline for PassPipeline<P, C> {
    fn should_render(&mut self, is_dirty: bool, metrics: &WindowMetrics) -> bool {
        // Ask the pipeline underneath first: it may have its own reason to defer,
        // and a frame it defers should not spend this one's slot.
        if !self.inner.should_render(is_dirty, metrics) {
            self.ledger.borrow_mut().defer();
            return false;
        }

        // The policy is read per frame rather than fixed at install time, which is
        // the point: `is_active` is the platform's own answer about this window, and
        // honouring it costs the application nothing.
        let rate = self.policy.rate_for(metrics);
        self.apply(rate);

        let now = self.clock.now();
        // The arrival check. An ask within the burst's tolerance is early enough and
        // takes its slot; one that is too soon changes nothing at all, so asking
        // again — which a dirty window does — is how a frame is deferred rather than
        // lost.
        if now + self.rate.tolerance() < self.next_allowed {
            self.ledger.borrow_mut().defer();
            return false;
        }

        self.next_allowed = self.next_allowed.max(now) + self.rate.interval;
        // The ask is granted, and the asks passed over before it are the frames it
        // stands in for.
        self.ledger.borrow_mut().admit();
        true
    }

    fn begin_frame(&mut self, window: &mut Window<'_>, cx: &mut App) {
        // The first pass of a frame, so it is where the previous frame is filed and
        // this one is opened — which is what attributes the asks passed over in
        // between to the frame they were passed over for.
        self.ledger.borrow_mut().open();
        let start = self.clock.now();
        self.inner.begin_frame(window, cx);
        self.record(Phase::Begin, start);
    }

    fn evaluate_roots(&mut self, window: &mut Window<'_>, cx: &mut App) -> PreparedRoots {
        let start = self.clock.now();
        let roots = self.inner.evaluate_roots(window, cx);
        self.record(Phase::Evaluate, start);
        roots
    }

    fn layout_roots(&mut self, window: &mut Window<'_>, roots: &mut PreparedRoots, cx: &mut App) {
        let start = self.clock.now();
        self.inner.layout_roots(window, roots, cx);
        self.record(Phase::Layout, start);
    }

    fn paint_roots(&mut self, window: &mut Window<'_>, roots: PreparedRoots, cx: &mut App) {
        let start = self.clock.now();
        self.inner.paint_roots(window, roots, cx);
        self.record(Phase::Paint, start);
    }

    fn finish_frame(&mut self, window: &mut Window<'_>, cx: &mut App) {
        let start = self.clock.now();
        self.inner.finish_frame(window, cx);
        self.record(Phase::Finish, start);
    }

    fn complete_frame(&mut self, window: &mut Window<'_>, cx: &mut App) -> Option<FocusId> {
        let start = self.clock.now();
        let focus = self.inner.complete_frame(window, cx);
        self.record(Phase::Complete, start);
        focus
    }

    fn end_frame(
        &mut self,
        window: &mut Window<'_>,
        cx: &mut App,
        focus_before_listeners: Option<FocusId>,
    ) {
        let start = self.clock.now();
        self.inner.end_frame(window, cx, focus_before_listeners);
        self.record(Phase::End, start);
        // The last pass of a frame, so the frame is complete and goes in the ring.
        self.ledger.borrow_mut().close();
    }
}

/// Builder methods that stack this crate's pass onto any pipeline.
///
/// The trait's name is `gpui_runtime`'s, deliberately: a decorator is a decorator,
/// and an application that stacks both crates' decorators is stacking them the same
/// way. Importing both traits at once needs one of them spelled out at the call
/// site, which is a fair price for the two reading identically at every other call
/// site.
pub trait FramePipelineExt: FramePipeline + Sized {
    /// Passes this pipeline by `policy`, recording into `ledger`. See
    /// [`PassPipeline`].
    fn pass(self, policy: RatePolicy, ledger: Rc<RefCell<Ledger>>) -> PassPipeline<Self> {
        PassPipeline::new(self, policy, ledger)
    }
}

impl<P: FramePipeline> FramePipelineExt for P {}
