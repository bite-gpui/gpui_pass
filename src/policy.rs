//! What rate a window is throttled to, and what changes it.

use core::{num::NonZeroU32, time::Duration};

use gpui_authoring::WindowMetrics;

/// One frame back to back: the least a burst can be, and what a rate starts at.
pub(crate) const ONE: NonZeroU32 = NonZeroU32::MIN;

/// A sustained rate, and the burst allowance it is measured with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rate {
    /// The rate as it was asked for, in whole frames a second.
    pub(crate) fps: NonZeroU32,
    /// The same rate as the interval between frames, which is what a decision is
    /// made against: a second divided by `fps`, truncated to whole nanoseconds, so
    /// 60 fps is 16,666,666ns and not the 16,666,666.67ns a second actually holds.
    /// Truncating leaves the interval a fraction of a nanosecond short of the true
    /// period, which errs toward admitting a frame rather than holding one back.
    pub(crate) interval: Duration,
    /// How many frames may be drawn back to back before `interval` applies.
    pub(crate) burst: NonZeroU32,
}

impl Rate {
    pub(crate) fn new(fps: NonZeroU32, burst: NonZeroU32) -> Self {
        Self {
            fps,
            interval: Duration::from_nanos(1_000_000_000 / u64::from(fps.get())),
            burst,
        }
    }

    pub(crate) fn with_burst(self, burst: NonZeroU32) -> Self {
        Self { burst, ..self }
    }

    /// How early a frame may be admitted: one interval per frame of burst beyond
    /// the first, and nothing at all at a burst of one.
    ///
    /// This is the whole of the burst rule. An ask is admitted when it lands no
    /// earlier than `interval * (burst - 1)` before the time the frame is due, so
    /// the first `burst` asks are each early enough and every ask after them is
    /// measured against the schedule those asks pushed forward.
    pub(crate) fn tolerance(self) -> Duration {
        self.interval * (self.burst.get() - 1)
    }
}

/// The rate a window is throttled to, stated separately for the two states the
/// seam can see it in.
///
/// A policy is data, not a constant: it is read once per frame from the
/// [`WindowMetrics`] the pipeline is handed, which is what lets one install
/// throttle two windows differently without either of them knowing. The one state
/// this crate acts on is [`is_active`](WindowMetrics::is_active) — the platform's
/// own answer about which window the person is looking at. A window the platform
/// reports as inactive does not need the rate of the one they are using, and it is
/// the only window state the seam carries that bears on frame throttling.
///
/// **The rate is not a cap on how fast the application runs.** It is a cap on how
/// often an ask to draw is allowed to spend itself doing the work of a frame. A
/// pipeline cannot slow an application down; it can only decline to draw, leaving
/// the work pending for the next ask.
///
/// Each rate carries a *sustained cadence* and a *burst allowance*: how many
/// frames may be drawn back to back before the cadence applies. That is the thing
/// an interval comparison cannot state, and the reason a rate here is a schedule
/// rather than a `Duration` a caller compares against.
///
/// ```
/// use core::num::NonZeroU32;
/// use gpui_pass::RatePolicy;
///
/// // 60 fps while the window is the active one, 15 while it is not, and up to
/// // three frames back to back either way.
/// let policy = RatePolicy::at(NonZeroU32::new(60).expect("a rate above zero"))
///     .inactive_at(NonZeroU32::new(15).expect("a rate above zero"))
///     .allow_burst(NonZeroU32::new(3).expect("a rate above zero"));
///
/// assert_eq!(policy.rates(), (60, 15));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatePolicy {
    active: Rate,
    inactive: Rate,
}

impl RatePolicy {
    /// Throttles a window at `fps` frames a second, whether or not it is the
    /// active one.
    ///
    /// The burst starts at one, which is the strictest the throttle can be: at a
    /// rate *equal to* the display's refresh period — 60 fps on a 60Hz panel — one
    /// is too strict to hold the rate, and [`allow_burst`](Self::allow_burst) should
    /// be raised to two. A rate below the refresh does hold with a burst of one, but
    /// on a margin thin enough to be a rounding artefact rather than slack (30 fps
    /// on 60Hz clears its boundary by one nanosecond; see `tests/pacing.rs`), so two
    /// is the value to reach for either way. See that method for why.
    ///
    /// `fps` is a [`NonZeroU32`], so "no frames a second" is not representable here:
    /// a window that should never draw is what answering `false` from
    /// [`should_render`](gpui_authoring::FramePipeline::should_render) is for, and a
    /// pipeline that never draws is not a frame rate. It is also what keeps the
    /// interval non-zero — a rate is a second divided by this number, so the only
    /// input that could reach a zero interval is the one the type excludes.
    pub fn at(fps: NonZeroU32) -> Self {
        let rate = Rate::new(fps, ONE);
        Self {
            active: rate,
            inactive: rate,
        }
    }

    /// Throttles a window the platform reports as inactive at `fps` instead.
    ///
    /// This is the whole policy: everything else about the window is left to the
    /// pipeline underneath, and a policy that never calls this throttles both states
    /// the same.
    pub fn inactive_at(self, fps: NonZeroU32) -> Self {
        Self {
            inactive: Rate::new(fps, self.inactive.burst),
            ..self
        }
    }

    /// Allows up to `frames` frames back to back, in either state, before the
    /// sustained rate applies.
    ///
    /// One is the least this can be, and is what a throttle bursts to on its own. A
    /// burst above one is for the frames that must not wait: the first frames of a
    /// drag, a keystroke's echo, a resize. Over any window of time longer than the
    /// burst, the sustained rate still holds — which is what makes it a burst and
    /// not a raised rate.
    ///
    /// **At a rate equal to the display's refresh period, a burst of one does not
    /// hold the rate.** The schedule is anchored to the instant an ask was admitted,
    /// which is a hair after the boundary the frame was asked on, and a target equal
    /// to the period leaves nothing to absorb that hair: with 60 fps asked for on a
    /// 60Hz panel and a burst of one, the throttle answers every second ask and
    /// delivers 30. The facade's own cap carries a quarter of an interval as
    /// tolerance for exactly this, and its source says so. Two here is the
    /// equivalent of that tolerance: one interval of slack, which is the smallest
    /// burst that holds a rate at or near the refresh rate — and the smallest step
    /// the knob has, since slack is counted in whole intervals.
    pub fn allow_burst(self, frames: NonZeroU32) -> Self {
        Self {
            active: self.active.with_burst(frames),
            inactive: self.inactive.with_burst(frames),
        }
    }

    /// The active and inactive rates, in whole frames a second.
    pub fn rates(&self) -> (u32, u32) {
        (self.active.fps.get(), self.inactive.fps.get())
    }

    /// The burst allowance, in frames, which both states share.
    pub fn burst(&self) -> u32 {
        self.active.burst.get()
    }

    /// The rate to throttle the window `metrics` describes by.
    pub(crate) fn rate_for(&self, metrics: &WindowMetrics) -> Rate {
        if metrics.is_active {
            self.active
        } else {
            self.inactive
        }
    }

    /// The rate a window that is the active one is throttled by.
    pub(crate) fn active_rate(&self) -> Rate {
        self.active
    }
}
