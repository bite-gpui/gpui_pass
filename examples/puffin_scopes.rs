//! The scopes a window's passes emit, printed as a waterfall — with no display.
//!
//! `pass_demo` shows the rate in a window and `pass_ticket` prints the ledger. This is
//! the third shape a measurement takes: with the `puffin` feature on,
//! [`PuffinPipeline`](gpui_pass::PuffinPipeline) emits one profiler scope per pass, and
//! this drives real frames headlessly and prints each frame's spans straight out of the
//! recorded data — the same spans a viewer draws, read back in-process instead of streamed
//! to one over TCP.
//!
//! That is the point the crate exists to make: telemetry does not need a display to be
//! verified. `TestAppContext` draws the frames, puffin records the scopes, and this prints
//! them where `tests/puffin.rs` asserts the same structure.
//!
//! # What the numbers are, and are not
//!
//! The scopes sit at the `FramePipeline` seam, so they time the CPU passes a frame is made of
//! — evaluate, layout, paint, and the bookkeeping that opens and closes them. They do **not**
//! time the GPU submit, the swap or the vblank: those live below the seam, in the platform,
//! and no pipeline can see them. A 60 fps frame is 16.67 ms of budget; the passes fill a slice
//! of it and the platform presents the rest.
//!
//! Two more things make these spans smaller than a shipping application's: the view below is
//! a grid of flat boxes rather than a real UI, and this is an unoptimised `dev` build. Read
//! them as *what the passes cost for this workload*, never as a frame time.
//!
//! To watch a live application instead, attach a sink of your own: `puffin_http` serves the
//! scopes over TCP for `puffin_viewer`, or `puffin_egui` draws them in the window. This
//! example installs `GlobalFrameView`, the in-process sibling both are built on.
//!
//! Run: `cargo run --example puffin_scopes --features puffin,test-support`

#![cfg(all(feature = "puffin", feature = "test-support"))]

use core::num::NonZeroU32;
use std::{rc::Rc, time::Duration};

use gpui::prelude::*;
use gpui::{
    App, Context, FramePipeline, Render, StandardImmediatePipeline, TestAppContext, Window, div,
    rgb,
};
use gpui_pass::{FakeClock, Ledger, PassPipeline, PuffinPipeline, RatePolicy};
use puffin::{GlobalFrameView, Reader, ScopeCollection, UnpackedFrameData};

/// The rate the window is asked to hold. 24 does not divide 60, which is the interesting
/// target — and it needs a schedule, so the throttle is part of the composition.
const RATE: u32 = 24;

/// How many frames to drive. A frame's scopes are collected when the next frame opens its
/// boundary, so one draw reads back nothing and two read back one.
const DRAWS: usize = 8;

/// How many of the most recent frames to print.
const PRINT: usize = 2;

/// The dimensions of the grid the view draws. A real UI is larger; this is enough that layout
/// and paint do visible work instead of rounding to zero.
const ROWS: usize = 12;
const COLUMNS: usize = 16;

fn fps(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("a rate above zero")
}

/// A grid of flat boxes — enough nodes that the passes cost something measurable, and never
/// asking for another frame, so the ask stream is the test's.
struct Grid;

impl Render for Grid {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let rows = (0..ROWS).map(|row| {
            let cells = (0..COLUMNS).map(move |column| {
                let shade = (row * COLUMNS + column) as u32;
                div().flex_1().bg(rgb(0x112233 + shade * 0x000101))
            });
            div().flex_1().flex().children(cells)
        });
        div().size_full().flex().flex_col().children(rows)
    }
}

fn main() {
    puffin::set_scopes_on(true);
    let view = GlobalFrameView::default();

    let mut cx = TestAppContext::single();
    let clock = FakeClock::default();
    let ledger = Ledger::shared(Ledger::DEFAULT_CAPACITY);

    cx.update({
        let clock = clock.clone();
        let ledger = ledger.clone();
        move |app: &mut App| {
            app.set_frame_pipeline_factory(Rc::new(move |_| {
                let throttled = PassPipeline::with_clock(
                    StandardImmediatePipeline,
                    RatePolicy::at(fps(RATE)).allow_burst(fps(2)),
                    ledger.clone(),
                    clock.clone(),
                );
                // The composition an application installs: the throttle inside, the scope
                // emitter outside it, so each scope measures a pass the throttle admitted.
                Box::new(PuffinPipeline::new(throttled)) as Box<dyn FramePipeline>
            }));
        }
    });

    let (view_handle, vcx) = cx.add_window_view(|_, _| Grid);
    // Space the asks a whole second apart so the throttle admits every one and the run
    // draws a stream of real frames. Only the *admission* is the fake clock's; puffin times
    // the scopes with its own clock.
    for _ in 0..DRAWS {
        clock.advance(Duration::from_secs(1));
        view_handle.update(&mut vcx.cx, |_, cx| cx.notify());
        vcx.run_until_parked();
    }

    println!("puffin scopes · the passes of a frame, not the present below the seam");
    println!(
        "workload: {} x {} boxes · dev build · passes only (no GPU, no swap, no vblank)",
        ROWS, COLUMNS
    );
    println!(
        "a 60 fps frame is 16.67 ms; these spans fill a slice of it and the platform presents the rest\n"
    );

    print_scopes(&view);
}

/// Prints the most recent frames, one waterfall each.
fn print_scopes(view: &GlobalFrameView) {
    let lock = view.lock();
    let collection = lock.scope_collection();
    let frames: Vec<_> = lock.latest_frames(PRINT).cloned().collect();

    if frames.is_empty() {
        println!("no complete frame was recorded — a frame is only readable once the next begins");
        return;
    }

    for frame in &frames {
        println!(
            "frame {:>4} · passes {:>8.3} ms · {} scopes",
            frame.frame_index(),
            ms(frame.duration_ns()),
            frame.meta().num_scopes
        );
        println!("{}", "-".repeat(42));
        let unpacked = match frame.unpacked() {
            Ok(unpacked) => unpacked,
            // `Never` is uninhabited: with puffin's `packing` feature off — its default — a
            // frame's data is always unpacked, so there is no error arm to carry.
            Err(never) => match never {},
        };
        print_streams(&unpacked, collection);
        println!();
    }
}

/// Prints the top-level scopes of every thread in one frame, in the order they ran.
fn print_streams(unpacked: &UnpackedFrameData, collection: &ScopeCollection) {
    for (thread, stream_info) in &unpacked.thread_streams {
        if !thread.name.is_empty() {
            println!("  [{}]", thread.name);
        }
        let scopes = match Reader::from_start(&stream_info.stream).read_top_scopes() {
            Ok(scopes) => scopes,
            Err(error) => {
                println!("  unreadable scope stream: {error:?}");
                continue;
            }
        };
        for scope in scopes {
            let name = collection
                .fetch_by_id(&scope.id)
                .map(|details| details.name().to_string())
                .unwrap_or_else(|| "<unregistered>".to_owned());
            println!("  {name:<22}{:>8.3} ms", ms(scope.record.duration_ns));
        }
    }
}

fn ms(nanoseconds: i64) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}
