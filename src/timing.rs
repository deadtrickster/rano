//! Timing assertions, on a machine that is not ours alone.
//!
//! The gate runs several suites at once, so a single sample of anything can be many times its
//! honest cost, and a test that asserts SPEED goes red naming the code when the answer was the
//! machine. What makes that fixable rather than merely inconvenient: contention only ever makes a
//! sample SLOWER, never faster. So the smallest of a few samples is the one closest to what the
//! code costs, and that is the sample the assertion judges.
//!
//! A test that asserts a deadline FIRED is the other shape and stays tight — there the slow answer
//! is the one under test, and a generous bound would stop testing anything. And a test that can
//! assert a COUNT — bytes read, rows returned, work done — needs no clock at all.

/// The least contended of `tries` samples of a comparison between two costs: the pair whose ratio
/// is smallest, in thousandths.
///
/// `f` runs to completion each time, so it has to be safe to repeat. A stall that lands on one side
/// of a ratio moves the ratio even though neither number is the code's, which is how a speed-up
/// test goes red on a loaded machine; two sides that inflate together cannot answer it, and that is
/// the whole point of judging a ratio rather than an absolute.
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
