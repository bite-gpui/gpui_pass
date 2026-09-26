//! What the passes cost, and what the throttle did to the asks that reached them.
//!
//! The facade's own instrumentation accumulates totals across frames and has no
//! memory of one frame from another. A ledger keeps the frames themselves, in a
//! bounded ring, because the interesting question about a frame budget is not the
//! average but the tail — the p99 paint that hitches — and the interesting
//! question about a throttle is not how many frames it drew but how many asks it
//! passed over getting there.

use core::time::Duration;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

/// One pass of a frame, in the order a frame runs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Sampling the platform window and resetting the frame's scratch state.
    Begin,
    /// Gathering the roots.
    Evaluate,
    /// Laying out and prepainting the roots.
    Layout,
    /// Painting the roots.
    Paint,
    /// Recording the views the frame touched.
    Finish,
    /// Swapping the frame in and dispatching focus changes.
    Complete,
    /// Closing the frame out and marking it ready to present.
    End,
}

impl Phase {
    /// Every phase, in the order a frame runs them.
    pub const ALL: [Phase; 7] = [
        Phase::Begin,
        Phase::Evaluate,
        Phase::Layout,
        Phase::Paint,
        Phase::Finish,
        Phase::Complete,
        Phase::End,
    ];

    /// The phase, spelled as the trait method that runs it.
    pub fn name(self) -> &'static str {
        match self {
            Phase::Begin => "begin_frame",
            Phase::Evaluate => "evaluate_roots",
            Phase::Layout => "layout_roots",
            Phase::Paint => "paint_roots",
            Phase::Finish => "finish_frame",
            Phase::Complete => "complete_frame",
            Phase::End => "end_frame",
        }
    }

    /// The phase's slot in a [`FrameCost`].
    fn index(self) -> usize {
        match self {
            Phase::Begin => 0,
            Phase::Evaluate => 1,
            Phase::Layout => 2,
            Phase::Paint => 3,
            Phase::Finish => 4,
            Phase::Complete => 5,
            Phase::End => 6,
        }
    }
}

/// How many phases a frame has, which is the width of a [`FrameCost`].
const PHASES: usize = Phase::ALL.len();

/// What one frame cost, phase by phase, and how many asks were passed over
/// before it drew.
///
/// A phase a frame did not run — a pipeline that skips a pass, a frame cut short —
/// reads as [`Duration::ZERO`] rather than as missing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCost {
    costs: [Duration; PHASES],
    deferred_since_previous: usize,
}

impl FrameCost {
    /// What `phase` cost in this frame.
    pub fn phase(&self, phase: Phase) -> Duration {
        self.costs[phase.index()]
    }

    /// Every phase and its cost, in the order a frame runs them.
    pub fn iter(&self) -> impl Iterator<Item = (Phase, Duration)> + '_ {
        Phase::ALL
            .into_iter()
            .map(move |phase| (phase, self.phase(phase)))
    }

    /// What the frame's passes cost in total.
    ///
    /// This is the time inside the pipeline's passes, not the whole frame: the
    /// platform still has to present what the frame produced.
    pub fn total(&self) -> Duration {
        self.costs
            .iter()
            .fold(Duration::ZERO, |total, cost| total + *cost)
    }

    /// How many asks were passed over between the previous drawn frame and this
    /// one — the frames this one stood in for.
    pub fn deferred_since_previous(&self) -> usize {
        self.deferred_since_previous
    }
}

/// A bounded record of what recent frames cost and what the throttle did.
///
/// The ledger is shared with the [`PassPipeline`](crate::PassPipeline) that fills
/// it, the way the facade's instrumentation is: build one, hand a clone to the
/// pipeline, keep the original to read.
///
/// ```
/// use gpui_pass::Ledger;
///
/// let ledger = Ledger::shared(Ledger::DEFAULT_CAPACITY);
/// # let _ = ledger;
/// ```
///
/// Reading the ledger takes a `RefCell` borrow, so read it between frames — a
/// viewer that holds a borrow while the engine draws will find the pipeline
/// unable to record.
#[derive(Debug)]
pub struct Ledger {
    capacity: usize,
    frames: VecDeque<FrameCost>,
    current: FrameCost,
    open: bool,
    drawn: usize,
    admitted: usize,
    deferred: usize,
    /// Asks passed over since the last frame that drew, waiting to be attributed
    /// to it.
    skipped: usize,
    /// The asks the open frame stands in for: what `skipped` held when the frame
    /// was admitted.
    pending: usize,
}

impl Ledger {
    /// How many frames a ledger keeps when nothing else is asked for: four
    /// seconds at 60 fps, which is long enough for a tail and short enough to
    /// stay cache-resident.
    pub const DEFAULT_CAPACITY: usize = 240;

    /// A ledger that keeps the last `capacity` frames.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            frames: VecDeque::new(),
            current: FrameCost::default(),
            open: false,
            drawn: 0,
            admitted: 0,
            deferred: 0,
            skipped: 0,
            pending: 0,
        }
    }

    /// A ledger in the `Rc<RefCell<_>>` a pipeline is handed, so the caller can
    /// keep a handle to read it while the pipeline writes it.
    pub fn shared(capacity: usize) -> Rc<RefCell<Ledger>> {
        Rc::new(RefCell::new(Self::new(capacity)))
    }

    /// How many frames this ledger keeps.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many frames are in the ring, which is `capacity` once it is full.
    pub fn frames(&self) -> usize {
        self.frames.len()
    }

    /// How many asks the throttle admitted since the ledger was made.
    ///
    /// This is one ahead of [`drawn`](Self::drawn) only while a frame is in
    /// flight: an ask is admitted at
    /// [`should_render`](gpui_authoring::FramePipeline::should_render) and filed
    /// when the frame reaches
    /// [`end_frame`](gpui_authoring::FramePipeline::end_frame).
    pub fn admitted(&self) -> usize {
        self.admitted
    }

    /// How many frames have been filed into the ring since the ledger was made.
    pub fn drawn(&self) -> usize {
        self.drawn
    }

    /// How many asks were passed over since the ledger was made.
    pub fn deferred(&self) -> usize {
        self.deferred
    }

    /// How many asks the throttle has seen: the admitted ones and the passed-over
    /// ones together.
    pub fn asks(&self) -> usize {
        self.admitted + self.deferred
    }

    /// The share of asks that were passed over, from zero to one. Zero before the
    /// first ask, rather than a division by zero.
    pub fn deferral_ratio(&self) -> f64 {
        let asks = self.asks();
        if asks == 0 {
            0.0
        } else {
            self.deferred as f64 / asks as f64
        }
    }

    /// The most recent frame in the ring.
    pub fn last(&self) -> Option<FrameCost> {
        self.frames.back().copied()
    }

    /// The `quantile` of what `phase` cost across the frames in the ring, by
    /// nearest rank: `0.5` is the median, `0.99` the tail.
    ///
    /// Zero when the ring is empty.
    pub fn phase_percentile(&self, phase: Phase, quantile: f64) -> Duration {
        let mut costs: Vec<Duration> = self.frames.iter().map(|frame| frame.phase(phase)).collect();
        nearest_rank(&mut costs, quantile)
    }

    /// The `quantile` of the frames' total pass cost, by nearest rank.
    pub fn frame_percentile(&self, quantile: f64) -> Duration {
        let mut totals: Vec<Duration> = self.frames.iter().map(FrameCost::total).collect();
        nearest_rank(&mut totals, quantile)
    }

    /// How many frames in the ring cost more than `budget` in total.
    ///
    /// A frame whose passes alone overrun the interval its rate implies has no
    /// room left for the platform to present it, which is what a hitch is. The
    /// count is over the ring, not all time, so it answers "how is it going"
    /// rather than "how was it".
    pub fn over_budget(&self, budget: Duration) -> usize {
        self.frames
            .iter()
            .filter(|frame| frame.total() > budget)
            .count()
    }

    /// Records an ask the throttle passed over.
    pub(crate) fn defer(&mut self) {
        self.deferred += 1;
        self.skipped += 1;
    }

    /// Records an ask the throttle granted, holding the asks passed over since the
    /// last one for the frame about to be opened.
    pub(crate) fn admit(&mut self) {
        self.admitted += 1;
        self.pending = self.skipped;
        self.skipped = 0;
    }

    /// Starts a frame, attributing to it the asks passed over since the last one.
    ///
    /// A frame still open is abandoned: only a frame that reached
    /// [`close`](Self::close) is filed.
    pub(crate) fn open(&mut self) {
        self.current = FrameCost {
            deferred_since_previous: self.pending,
            ..FrameCost::default()
        };
        self.open = true;
    }

    /// Adds `cost` to the open frame's `phase`.
    pub(crate) fn record(&mut self, phase: Phase, cost: Duration) {
        let slot = &mut self.current.costs[phase.index()];
        *slot += cost;
    }

    /// Files the open frame into the ring, dropping the oldest if it is full.
    pub(crate) fn close(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        self.frames.push_back(self.current);
        if self.frames.len() > self.capacity {
            self.frames.pop_front();
        }
        self.drawn += 1;
    }
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

/// The nearest-rank `quantile` of `values`, which is sorted in place.
fn nearest_rank(values: &mut [Duration], quantile: f64) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    values.sort_unstable();
    let rank = (quantile.clamp(0.0, 1.0) * values.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(values.len() - 1);
    values[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(paint: u64) -> FrameCost {
        let mut frame = FrameCost::default();
        frame.costs[Phase::Paint.index()] = Duration::from_micros(paint);
        frame
    }

    fn ledger_with(paints: &[u64]) -> Ledger {
        let mut ledger = Ledger::new(Ledger::DEFAULT_CAPACITY);
        for paint in paints {
            ledger.open();
            ledger.record(Phase::Paint, Duration::from_micros(*paint));
            ledger.close();
        }
        ledger
    }

    #[test]
    fn a_phase_a_frame_did_not_run_reads_as_zero() {
        let frame = cost(1200);
        assert_eq!(frame.phase(Phase::Paint), Duration::from_micros(1200));
        assert_eq!(frame.phase(Phase::Layout), Duration::ZERO);
        assert_eq!(frame.total(), Duration::from_micros(1200));
    }

    #[test]
    fn the_ring_is_bounded_and_drops_the_oldest() {
        let mut ledger = Ledger::new(3);
        for paint in [10, 20, 30, 40] {
            ledger.open();
            ledger.record(Phase::Paint, Duration::from_micros(paint));
            ledger.close();
        }
        assert_eq!(ledger.frames(), 3);
        assert_eq!(ledger.drawn(), 4);
        assert_eq!(
            ledger.last().map(|frame| frame.phase(Phase::Paint)),
            Some(Duration::from_micros(40))
        );
    }

    #[test]
    fn a_frame_still_open_is_not_drawn() {
        let mut ledger = Ledger::new(8);
        ledger.open();
        ledger.record(Phase::Paint, Duration::from_micros(5));
        assert_eq!(ledger.drawn(), 0);
        assert_eq!(ledger.frames(), 0);
    }

    #[test]
    fn asks_passed_over_are_attributed_to_the_next_frame() {
        let mut ledger = Ledger::new(8);
        ledger.defer();
        ledger.defer();
        ledger.admit();
        ledger.open();
        ledger.record(Phase::Paint, Duration::from_micros(5));
        ledger.close();

        let frame = ledger.last().expect("a drawn frame");
        assert_eq!(frame.deferred_since_previous(), 2);
        assert_eq!(ledger.admitted(), 1);
        assert_eq!(ledger.deferred(), 2);
        assert_eq!(ledger.drawn(), 1);
        assert_eq!(ledger.asks(), 3);
    }

    #[test]
    fn the_deferral_ratio_is_zero_before_the_first_ask() {
        assert_eq!(Ledger::new(4).deferral_ratio(), 0.0);
    }

    #[test]
    fn percentiles_are_by_nearest_rank() {
        let ledger = ledger_with(&[10, 20, 30, 40]);
        assert_eq!(
            ledger.phase_percentile(Phase::Paint, 0.0),
            Duration::from_micros(10)
        );
        assert_eq!(
            ledger.phase_percentile(Phase::Paint, 0.5),
            Duration::from_micros(20)
        );
        assert_eq!(
            ledger.phase_percentile(Phase::Paint, 1.0),
            Duration::from_micros(40)
        );
        assert_eq!(ledger.frame_percentile(0.99), Duration::from_micros(40));
    }

    #[test]
    fn percentiles_of_an_empty_ring_are_zero() {
        let ledger = Ledger::new(4);
        assert_eq!(ledger.phase_percentile(Phase::Layout, 0.5), Duration::ZERO);
        assert_eq!(ledger.frame_percentile(0.5), Duration::ZERO);
    }

    #[test]
    fn over_budget_counts_the_frames_past_the_line() {
        let ledger = ledger_with(&[10, 20, 30, 40]);
        assert_eq!(ledger.over_budget(Duration::from_micros(25)), 2);
        assert_eq!(ledger.over_budget(Duration::from_micros(100)), 0);
    }
}
