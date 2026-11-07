//! Timing assertions, on a machine that is not ours alone.
//!
//! The gate runs several suites at once, so a single sample of anything can be many times its
//! honest cost, and a test that asserts SPEED goes red naming the code when the answer was the
//! machine. What makes that fixable rather than merely inconvenient: contention only ever makes a
//! sample SLOWER, never faster. So the smallest of a few samples is the one closest to what the
//! code costs, and that is the sample the assertion judges.
//!
//! A test that asserts a deadline FIRED is the other shape and stays tight — there the slow answer
//! is the one under test, and a generous bound would stop testing anything.

use std::time::Duration;

/// Run `f` up to `tries` times and keep the least contended sample: its value, and its time.
///
/// `f` runs to completion each time, so it has to be safe to repeat — a read, a scan, a
/// conversion. The value comes back with the time so a caller that also checks the result is
/// checking the sample it is asserting on.
pub(crate) fn best<T>(tries: usize, mut f: impl FnMut() -> (T, Duration)) -> (T, Duration) {
    let mut best: Option<(T, Duration)> = None;
    for _ in 0..tries.max(1) {
        let sample = f();
        if best.as_ref().is_none_or(|(_, t)| sample.1 < *t) {
            best = Some(sample);
        }
    }
    best.expect("tries.max(1) runs at least once")
}

/// [`best`] for a measurement that compares two costs: the sample with the smallest ratio wins.
///
/// A stall that lands on one side of a ratio moves the ratio even though neither number is the
/// code's, which is how a speed-up test goes red on a loaded machine. The ratio is in thousandths
/// so it stays integer; `denominator` is the side that only ever inflates.
pub(crate) fn best_ratio(tries: usize, mut f: impl FnMut() -> (u128, u128)) -> (u128, u128) {
    let mut best: Option<((u128, u128), u128)> = None;
    for _ in 0..tries.max(1) {
        let (numerator, denominator) = f();
        let ratio = numerator.saturating_mul(1000) / denominator.max(1);
        if best.as_ref().is_none_or(|(_, r)| ratio < *r) {
            best = Some(((numerator, denominator), ratio));
        }
    }
    best.expect("tries.max(1) runs at least once").0
}
