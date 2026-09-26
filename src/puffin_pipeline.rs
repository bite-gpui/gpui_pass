//! The same passes, as scopes on puffin's timeline.
//!
//! [`PassPipeline`](crate::PassPipeline) accounts for a window in-process: what each pass
//! cost, how many asks the throttle passed over, where the recent frames sit against the
//! budget. That is a summary, and it answers "how are the last few seconds going". This
//! is the other shape a measurement takes — a scope per pass, and a frame boundary
//! between frames — which answers "what did *this* frame do", with a bar you can drag a
//! cursor across.
//!
//! # Why puffin and not a C++ client
//!
//! Two reasons, and the second is the load-bearing one. It is pure Rust, so it links
//! nothing a consumer does not already have. And it is **in-process**: scopes are
//! recorded into thread-local buffers that a test can turn on, drive real frames
//! through, and read back. `tests/puffin.rs` asserts that structure and
//! `examples/puffin_scopes.rs` prints it, both without a display. A profiler whose data
//! only exists inside an external GUI cannot be checked in CI, and the crate that would
//! have carried one could not have been either.
//!
//! # What it emits
//!
//! One scope per pass, named for the trait method that runs it — `gpui::begin_frame`,
//! `gpui::evaluate_roots`, `gpui::layout_roots`, `gpui::paint_roots`, `gpui::finish_frame`,
//! `gpui::complete_frame`, `gpui::end_frame` — and a frame boundary at the start of every
//! frame, which is where puffin's own documentation puts it. Every pass is forwarded
//! unchanged, so the pipeline underneath draws exactly as it would have.
//!
//! `should_render` is deliberately not scoped. It is asked once per ask, not once per
//! drawn frame, and a scope around it would report a per-ask duration as if it were part
//! of a frame — which is the confusion this crate exists to avoid.
//!
//! With the profiler off — the default — a scope costs one relaxed atomic load, and the
//! frame boundary is behind the same check, so a window nobody is profiling pays for
//! none of this.
//!
//! # Seeing it
//!
//! Emitting the scopes is this crate's half. The other half is a sink: `puffin_http`
//! serves them over TCP and `puffin_viewer` attaches to that socket, or `puffin_egui`
//! draws the flamegraph inside the application. `PuffinPipeline` neither starts nor
//! depends on either — `puffin::GlobalProfiler::lock().add_sink` is what takes them.

use gpui_authoring::{App, FocusId, FramePipeline, PreparedRoots, Window, WindowMetrics};

/// Wraps the pipeline it decorates, emitting a puffin scope for every pass it forwards.
///
/// It composes like any other decorator: wrap a
/// [`PassPipeline`](crate::PassPipeline) to get the throttle and the ledger around the
/// same passes, or wrap this one — a scope measures whatever it encloses.
pub struct PuffinPipeline<P> {
    inner: P,
}

impl<P> PuffinPipeline<P> {
    /// Wraps `inner`.
    pub fn new(inner: P) -> Self {
        Self { inner }
    }

    /// The pipeline this one wraps.
    pub fn inner(&self) -> &P {
        &self.inner
    }
}

impl<P: FramePipeline> FramePipeline for PuffinPipeline<P> {
    fn should_render(&mut self, is_dirty: bool, metrics: &WindowMetrics) -> bool {
        self.inner.should_render(is_dirty, metrics)
    }

    fn begin_frame(&mut self, window: &mut Window<'_>, cx: &mut App) {
        // The boundary comes first, because of what it means: starting a frame is what
        // collects the *previous* one. Puffin's own documentation puts the call here —
        // "once at the start of every frame" — so the frame this closes out is the one
        // that finished, not the one beginning.
        if puffin::are_scopes_on() {
            puffin::GlobalProfiler::lock().new_frame();
        }
        puffin::profile_scope!("gpui::begin_frame");
        self.inner.begin_frame(window, cx);
    }

    fn evaluate_roots(&mut self, window: &mut Window<'_>, cx: &mut App) -> PreparedRoots {
        puffin::profile_scope!("gpui::evaluate_roots");
        self.inner.evaluate_roots(window, cx)
    }

    fn layout_roots(&mut self, window: &mut Window<'_>, roots: &mut PreparedRoots, cx: &mut App) {
        puffin::profile_scope!("gpui::layout_roots");
        self.inner.layout_roots(window, roots, cx);
    }

    fn paint_roots(&mut self, window: &mut Window<'_>, roots: PreparedRoots, cx: &mut App) {
        puffin::profile_scope!("gpui::paint_roots");
        self.inner.paint_roots(window, roots, cx);
    }

    fn finish_frame(&mut self, window: &mut Window<'_>, cx: &mut App) {
        puffin::profile_scope!("gpui::finish_frame");
        self.inner.finish_frame(window, cx);
    }

    fn complete_frame(&mut self, window: &mut Window<'_>, cx: &mut App) -> Option<FocusId> {
        puffin::profile_scope!("gpui::complete_frame");
        self.inner.complete_frame(window, cx)
    }

    fn end_frame(
        &mut self,
        window: &mut Window<'_>,
        cx: &mut App,
        focus_before_listeners: Option<FocusId>,
    ) {
        puffin::profile_scope!("gpui::end_frame");
        self.inner.end_frame(window, cx, focus_before_listeners);
    }
}
