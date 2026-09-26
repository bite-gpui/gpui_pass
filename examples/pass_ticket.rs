//! The ledger as a ticket, from the engine's own frame loop, with no display.
//!
//! `pass_demo` shows the rate in a window. This shows what the pass *accounted for*,
//! and it opens no window: `TestAppContext` drives the frame loop headlessly, so the
//! passes timed here are the passes the engine actually calls, on the real clock.
//!
//! The asks in the loop arrive far faster than any display's, which is the point of
//! running it this way — it is the situation a throttle exists for, and the ledger says
//! exactly what the throttle did about it.
//!
//! Run: `cargo run --example pass_ticket --features test-support`

#![cfg(feature = "test-support")]

use core::num::NonZeroU32;
use std::{rc::Rc, time::Duration};

use gpui::prelude::*;
use gpui::{
    App, Context, FramePipeline, Render, StandardImmediatePipeline, TestAppContext, Window, div,
    rgb,
};
use gpui_pass::{Ledger, PassPipeline, Phase, RatePolicy};

/// How many asks the loop makes. On a display this would be five seconds at 60Hz.
const ASKS: usize = 300;

/// The rate the window is asked to hold. 24 does not divide 60, which is the
/// interesting target: it is why the pass carries a schedule at all.
const RATE: u32 = 24;

/// The grid the view draws. A real UI is larger, but a single box paints in microseconds
/// and would make the ticket's "total" read like a frame time; this does enough work that
/// the phases cost something.
const ROWS: usize = 12;
const COLUMNS: usize = 16;

fn fps(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).expect("a rate above zero")
}

/// A grid of flat boxes — and it never asks for another frame, so the ask stream is the
/// loop's, not the view's.
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
    let mut cx = TestAppContext::single();
    let ledger = Ledger::shared(Ledger::DEFAULT_CAPACITY);

    cx.update({
        let ledger = ledger.clone();
        move |app: &mut App| {
            app.set_frame_pipeline_factory(Rc::new(move |_| {
                Box::new(PassPipeline::new(
                    StandardImmediatePipeline,
                    RatePolicy::at(fps(RATE)).allow_burst(fps(2)),
                    ledger.clone(),
                )) as Box<dyn FramePipeline>
            }));
        }
    });

    let (view, vcx) = cx.add_window_view(|_, _| Grid);

    for _ in 0..ASKS {
        // A drawn frame leaves the window clean, so each ask needs the mark the
        // platform would have left.
        view.update(&mut vcx.cx, |_, cx| cx.notify());
        vcx.run_until_parked();
    }

    print_ticket(&ledger.borrow());
}

/// Prints the ledger the way the ticket on the site draws it.
fn print_ticket(ledger: &Ledger) {
    let budget = Duration::from_nanos(1_000_000_000 / u64::from(RATE));
    let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;

    println!(
        "THE FRAME TICKET — window #1 · asked for {RATE} fps · budget {:.2} ms",
        ms(budget)
    );
    println!("(the passes only — the GPU submit and the swap are below the seam)");
    println!("{}", "-".repeat(66));

    match ledger.last() {
        Some(frame) => {
            println!("{:<24}{:>14}", "phase", "cost");
            for (phase, cost) in frame.iter() {
                println!("{:<24}{:>11.3} ms", phase.name(), ms(cost));
            }
            println!("{}", "-".repeat(66));
            println!("{:<24}{:>11.3} ms", "total", ms(frame.total()));
            println!(
                "{:<24}{:>11}",
                "asks stood in for",
                frame.deferred_since_previous()
            );
        }
        None => println!("no frame was drawn"),
    }

    println!("{}", "-".repeat(66));
    println!(
        "asks {} · drawn {} · passed over {} ({:.0}%)",
        ledger.asks(),
        ledger.drawn(),
        ledger.deferred(),
        ledger.deferral_ratio() * 100.0
    );
    println!(
        "paint p50 / p99: {:.3} / {:.3} ms, over {} of {} frames",
        ms(ledger.phase_percentile(Phase::Paint, 0.5)),
        ms(ledger.phase_percentile(Phase::Paint, 0.99)),
        ledger.over_budget(budget),
        ledger.frames()
    );
    println!(
        "{}",
        if ledger.frames() > 0 {
            "verdict: the drawn frames fit the interval the rate implies"
        } else {
            "verdict: nothing drew, so there is nothing to judge"
        }
    );
}
