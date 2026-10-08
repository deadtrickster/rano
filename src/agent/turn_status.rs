//! **The turn's status row**: what the model is doing — green while it goes, yellow when slow.
//!
//! Ported from letibot's `crates/tui/src/ui/turn_status.rs` (`turn_status`) and
//! the prefill half of `letibot_ui::progress` (`Prefill`, `bar`, `prefill_line`), which nothing
//! else draws. Whether a turn is busy or generating, and the clocks, are the host's; this row
//! is a pure function of them — no clock is read here, so a replay draws what the live head did.

use crate::render::{Line, Span, Style};
use crate::style::Role;

use super::header::thousands;
use super::text::{duration, spinner, trim_to};

/// The server's prefill progress, as it arrives.
///
/// # What the three numbers mean, because getting this wrong is easy
///
/// - `total` — tokens in the prompt.
/// - `cache` — tokens reused from the slot's KV cache. **Free.**
/// - `processed` — tokens now resident in the slot, **including** `cache`. So the fraction
///   complete is `processed / total`, and the work actually done this turn is
///   `processed - cache`.
/// - `time_ms` — cumulative prefill milliseconds.
///
/// Reading `processed` as "processed *since* the cache" would show a 90%-cached prompt as 10%
/// done and then jump to 100%, which is the classic progress-bar lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Prefill {
    pub total: u64,
    pub cache: u64,
    pub processed: u64,
    pub time_ms: u64,
}

impl Prefill {
    /// Fraction of the prompt resident, 0.0 to 1.0.
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.processed.min(self.total) as f64) / (self.total as f64)
    }

    /// Fraction of the prompt that cost nothing.
    pub fn cached_fraction(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.cache.min(self.total) as f64) / (self.total as f64)
    }

    /// Tokens actually computed this turn.
    pub fn computed(&self) -> u64 {
        self.processed.saturating_sub(self.cache)
    }

    /// Prefill throughput in tokens per second, or `None` before there is enough to divide by.
    ///
    /// Measured over `computed()`, not `processed`: dividing the cache hit by the wall clock
    /// produces a number in the hundreds of thousands and it is not a speed, it is an artefact.
    pub fn rate(&self) -> Option<f64> {
        if self.time_ms < 50 || self.computed() == 0 {
            return None;
        }
        Some(self.computed() as f64 * 1000.0 / self.time_ms as f64)
    }

    /// Estimated milliseconds to the end of the prefill; everything past `processed` is by
    /// definition not in the cache, so it all has to be computed.
    pub fn eta_ms(&self) -> Option<u64> {
        let r = self.rate()?;
        let left = self.total.saturating_sub(self.processed);
        if left == 0 {
            return Some(0);
        }
        Some((left as f64 * 1000.0 / r) as u64)
    }
}

/// Eighth-of-a-cell fill, from empty to full (grok-build's `progress_bar.rs`). A 20-column bar
/// over a 40,000-token prompt advances one cell per 2,000 tokens, so at whole-cell resolution a
/// bar that is genuinely moving looks frozen for seconds at a time.
const EIGHTHS: [&str; 9] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];

/// A three-segment bar `cols` columns wide: cached, computed this turn, remaining.
///
/// ```text
/// ▐████████▓▓▓▓▍░░░░░▌
/// ```
///
/// The cached run is drawn differently from the computed run on purpose: a two-colour bar
/// answers "how far along"; this one also answers "how much of this did the prefix cache save
/// me". With no colour the three runs are still three different glyphs.
pub fn bar(p: &Prefill, cols: usize) -> Vec<Span> {
    let cols = cols.max(4);
    let inner = cols.saturating_sub(2);
    if p.total == 0 || inner == 0 {
        return vec![Span::raw(format!("▐{}▌", "░".repeat(inner)))];
    }
    let frac = |n: u64| (n.min(p.total) as f64) / (p.total as f64);
    // The moving edge, in eighths of a cell.
    let done_e = ((frac(p.processed) * (inner * 8) as f64).round() as usize).min(inner * 8);
    // The cache boundary, in whole cells, never past the moving edge.
    let cached = ((frac(p.cache) * inner as f64).floor() as usize).min(done_e / 8);
    let full = done_e / 8 - cached;
    let rem = done_e % 8;
    let partial = usize::from(rem > 0);
    let rest = inner - cached - full - partial;
    let mut out = vec![Span::raw("▐")];
    if cached > 0 {
        out.push(Span::role("█".repeat(cached), Role::Success));
    }
    if full > 0 {
        out.push(Span::role("▓".repeat(full), Role::Pending));
    }
    if partial == 1 {
        out.push(Span::role(EIGHTHS[rem], Role::Pending));
    }
    if rest > 0 {
        out.push(Span::role("░".repeat(rest), Role::Faint));
    }
    out.push(Span::raw("▌"));
    out
}

/// The whole prefill line: bar, percentage, rate, estimate.
///
/// The raw counts are deliberately not here: the header carries `ctx` and `cached%`. What the
/// header cannot show — how fast the expansion runs and how long is left — is what this keeps.
/// Degrades by dropping the *least* useful field first: the estimate, then the rate, then the
/// bar — leaving `prefill 61%`, which is still true. It never wraps.
pub fn prefill_line(p: &Prefill, cols: usize) -> Line {
    let pct = (p.fraction() * 100.0).round() as u64;
    let head = format!("prefill {pct}%");
    let rate = p.rate().map(|r| format!("{} tok/s", thousands(r as u64)));
    let eta = p
        .eta_ms()
        .filter(|_| p.processed < p.total)
        .map(|ms| format!("~{} left", duration(ms)));
    let barw = 20usize.min(cols / 3);
    let with = |extra: &[&Option<String>]| {
        let mut l = Line::raw(format!("{head} "));
        l.spans.extend(bar(p, barw));
        for e in extra.iter().filter_map(|e| e.as_ref()) {
            l.push(Span::raw(format!(" · {e}")));
        }
        l
    };
    for c in [
        with(&[&rate, &eta]),
        with(&[&rate]),
        with(&[]),
        Line::raw(head.clone()),
    ] {
        if c.width() <= cols {
            return c;
        }
    }
    Line::raw(trim_to(&head, cols))
}

/// The turn row's view model.
///
/// letibot fills it from: `busy` ← `turn.is_some() && turn_busy()`; `prefill` ←
/// `turn.progress` while `total > 0 && processed < total`; `elapsed_ms` ←
/// `now_ms - turn.started_ms`, `None` when `started_ms == 0` (a turn out of a snapshot);
/// `now_ms` ← the head's clock, for the spinner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TurnStatus {
    /// **Busy, not the state name.** Gated on `Running` the row was not drawn AT ALL during a
    /// tool call — indistinguishable from a head that has stopped (R51 item 3).
    pub busy: bool,
    pub prefill: Option<Prefill>,
    pub elapsed_ms: Option<u64>,
    /// # The spinner runs on the head's clock
    ///
    /// It used to be keyed off the last event's timestamp, and a spinner that only moves when a
    /// token arrives is a snapshot of one — a tool running thirty silent seconds froze it.
    pub now_ms: u64,
    /// **The turn is slow**: generating and silent past [`SLOW_AFTER_MS`]. The host decides —
    /// only it knows whether the turn should be emitting (a silent `cargo test` is a call, not
    /// a stall). It turns the row yellow; it adds no words.
    pub slow: bool,
}

/// How long a generating turn may be silent before its row turns yellow.
pub const SLOW_AFTER_MS: u64 = 15_000;

impl TurnStatus {
    /// The row; empty when nothing is running (the row is reserved by the fit ladder either way).
    ///
    /// **The word is `Responding` for the whole turn** — including the wait on a call, and the
    /// silence before the first token. Ruled by the operator, 2026-09-27: *"responding spans
    /// entire turn"*. What says *which* call is running is the transcript, not this line.
    ///
    /// **No count on this row.** *"i dont care about those chars"* / *"just dont show me
    /// them"*: a row whose job is to say the turn is alive needs the spinner and the clock, and
    /// a figure the reader has to interpret is the row asking to be studied.
    ///
    /// **Green while it is going, yellow when it is slow** — and nothing more. Ruled by the
    /// operator, 2026-10-08: *"we have Responding in yellow which is a warning color … I dont
    /// want notification that it is slow yet we continue … let usual Responding be green and
    /// when we detect delays - yellow it"*. A turn that has really failed ends with
    /// `TurnFailed` and stops this row; a slow one only changes its colour, so there is no
    /// sentence about it (the `stuck_line` that said *"nothing received for 17s"* is gone).
    pub fn line(&self, w: usize) -> Line {
        if !self.busy {
            return Line::default();
        }
        let tone = if self.slow {
            Role::Pending
        } else {
            Role::Success
        };
        let spin = Span::role(spinner(self.now_ms).to_string(), tone);
        match self.prefill {
            Some(pp) if pp.total > 0 && pp.processed < pp.total => {
                let mut l = Line::new(vec![spin, Span::raw(" ")]);
                l.spans.extend(prefill_line(&pp, w.saturating_sub(6)).spans);
                crate::render::text::truncate_owned(l, w)
            }
            _ => {
                // `started_ms == 0` means the turn came out of a snapshot, which has no
                // timestamps — the line once read `Responding · 496940h16m`. Never a number
                // nobody took.
                let since = match self.elapsed_ms {
                    None => " · started before this head attached".to_string(),
                    Some(ms) => format!(" · {}", duration(ms)),
                };
                // **The spinner inside the pending row**, as letibot drew it: the row painted
                // pending with a painted spinner in it. The cells are one pending run either
                // way; the stack is what lets a host that prints strings write the row as
                // the nesting it always was.
                let l = Line::new(vec![
                    Span::styled(spinner(self.now_ms).to_string(), Style::of(tone).role(tone)),
                    Span::role(format!(" Responding{since}"), tone),
                ]);
                crate::render::text::truncate_owned(l, w)
            }
        }
    }
}

impl crate::render::Widget for TurnStatus {
    fn render(&self, area: crate::render::Rect, buf: &mut crate::render::Buffer) {
        if !area.is_empty() {
            let w = area.width as usize;
            buf.set_line(area.x, area.y, &self.line(w), w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::role_of;

    fn bar_text(p: &Prefill, cols: usize) -> String {
        Line::new(bar(p, cols)).plain()
    }

    /// letibot `progress::there_is_no_rate_before_there_is_evidence_for_one`.
    #[test]
    fn there_is_no_rate_before_there_is_evidence_for_one() {
        assert!(
            Prefill {
                total: 100,
                cache: 100,
                processed: 100,
                time_ms: 0
            }
            .rate()
            .is_none()
        );
        assert!(
            Prefill {
                total: 100,
                cache: 0,
                processed: 4,
                time_ms: 10
            }
            .rate()
            .is_none()
        );
    }

    /// letibot `progress::the_moving_edge_has_sub_cell_resolution`: a 20-cell bar over a 40k
    /// prompt moves one cell per 2k tokens, and at whole-cell resolution a bar that is
    /// genuinely advancing looks frozen.
    #[test]
    fn the_moving_edge_has_sub_cell_resolution() {
        let mk = |processed| Prefill {
            total: 40_000,
            cache: 0,
            processed,
            time_ms: 1_000,
        };
        let a = bar_text(&mk(10_000), 22);
        let b = bar_text(&mk(10_300), 22);
        assert_ne!(a, b, "300 tokens of a 40k prompt must move the bar");
        assert_eq!(visible_width_of(&a), 22);
        assert_eq!(visible_width_of(&b), 22);
    }

    /// letibot `progress::the_bar_is_exactly_the_requested_width_at_every_fraction`.
    #[test]
    fn the_bar_is_exactly_the_requested_width_at_every_fraction() {
        for cache in [0u64, 1, 37, 99, 100] {
            for processed in cache..=100 {
                for w in [6usize, 10, 21, 40] {
                    let p = Prefill {
                        total: 100,
                        cache,
                        processed,
                        time_ms: 100,
                    };
                    assert_eq!(
                        Line::new(bar(&p, w)).width(),
                        w,
                        "cache {cache} processed {processed} w {w}"
                    );
                }
            }
        }
    }

    /// letibot `progress::the_status_line_never_exceeds_its_width`.
    #[test]
    fn the_status_line_never_exceeds_its_width() {
        let p = Prefill {
            total: 41_233,
            cache: 38_100,
            processed: 39_900,
            time_ms: 1_240,
        };
        for w in [12usize, 20, 30, 50, 80, 120, 200] {
            let l = prefill_line(&p, w);
            assert!(l.width() <= w, "{w}: {} cols {:?}", l.width(), l.plain());
            assert!(l.plain().contains("prefill"), "{w}: {:?}", l.plain());
        }
    }

    fn visible_width_of(s: &str) -> usize {
        super::super::text::visible_width(s)
    }

    fn busy(elapsed: u64, now: u64) -> TurnStatus {
        TurnStatus {
            busy: true,
            prefill: None,
            elapsed_ms: Some(elapsed),
            now_ms: now,
            slow: false,
        }
    }

    /// letibot `app/tests/turn.rs::the_spinner_spins_on_the_heads_clock_not_on_the_daemons_events`
    /// and `a_turn_that_only_thinks_and_writes_calls_still_draws_its_row`.
    #[test]
    fn the_spinner_and_the_clock_move_on_the_heads_clock_and_nothing_is_counted() {
        let first = busy(0, 1_000).line(120);
        assert!(first.plain().contains("Responding"), "{}", first.plain());
        assert!(first.plain().contains("0ms"), "{}", first.plain());
        let second = busy(160, 1_160).line(120);
        assert!(second.plain().contains("160ms"));
        let glyph = |s: &str| s.chars().find(|c| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(*c));
        assert_ne!(glyph(&first.plain()), glyph(&second.plain()));
        assert!(busy(3_000, 4_000).line(160).plain().contains("3.0s"));
        assert!(!second.plain().contains("chars") && !second.plain().contains(" tok"));
        assert_eq!(role_of(&second, "Responding"), Some(Role::Success));
    }

    /// letibot `app/tests/turn.rs` — a busy turn counts from the prompt through its calls, and
    /// a turn that is not busy draws nothing (the row stands down with the work).
    #[test]
    fn the_row_stands_down_with_the_work() {
        assert!(busy(4_000, 5_000).line(120).plain().contains("4.0s"));
        assert_eq!(TurnStatus::default().line(120).width(), 0);
    }

    /// A turn out of a snapshot has no start: no invented duration.
    #[test]
    fn a_snapshot_turn_does_not_invent_a_duration() {
        let t = TurnStatus {
            busy: true,
            elapsed_ms: None,
            ..TurnStatus::default()
        };
        assert!(
            t.line(120)
                .plain()
                .contains("started before this head attached")
        );
    }

    /// letibot `app/tests/turn.rs::the_prefill_line_reads_as_nearly_done_when_the_prompt_was_mostly_cached`.
    #[test]
    fn the_prefill_line_reads_as_nearly_done_when_the_prompt_was_mostly_cached() {
        let mut t = busy(0, 0);
        t.prefill = Some(Prefill {
            total: 41_233,
            cache: 38_100,
            processed: 39_900,
            time_ms: 900,
        });
        let line = t.line(120).plain();
        assert!(line.contains("prefill 97%"), "{line}");
        // 1,800 computed tokens in 900 ms.
        assert!(line.contains("2000 tok/s"), "{line}");
        assert!(!line.contains("cached"), "{line}");
        for w in [24usize, 40, 60, 80, 120, 200] {
            assert!(t.line(w).width() <= w, "w={w}");
        }
    }

    /// letibot `letibot_ui::progress` tests, on lines.
    #[test]
    fn a_mostly_cached_prompt_reads_as_nearly_done_not_nearly_undone() {
        let p = Prefill {
            total: 20_000,
            cache: 18_000,
            processed: 19_000,
            time_ms: 400,
        };
        assert_eq!((p.fraction() * 100.0).round() as u64, 95);
        assert_eq!(p.computed(), 1_000);
        assert_eq!((p.cached_fraction() * 100.0).round() as u64, 90);
    }

    #[test]
    fn the_rate_is_computed_over_work_done_not_over_the_cache_hit() {
        let p = Prefill {
            total: 20_000,
            cache: 18_000,
            processed: 19_000,
            time_ms: 500,
        };
        assert_eq!(p.rate().unwrap().round() as u64, 2000);
        assert!(p.rate().unwrap() < 5000.0);
        let none = Prefill {
            total: 100,
            cache: 0,
            processed: 4,
            time_ms: 10,
        };
        assert!(none.rate().is_none());
    }

    #[test]
    fn the_bar_shows_three_segments_and_the_cache_is_one_of_them() {
        let p = Prefill {
            total: 100,
            cache: 50,
            processed: 75,
            time_ms: 100,
        };
        let b = Line::new(bar(&p, 22));
        assert_eq!(b.width(), 22);
        let s = b.plain();
        assert_eq!(s.matches('█').count(), 10, "cached run");
        assert_eq!(s.matches('▓').count(), 5, "computed run");
        assert_eq!(s.matches('░').count(), 5, "remaining run");
        assert_eq!(role_of(&b, "█"), Some(Role::Success));
    }

    #[test]
    fn the_bar_is_exactly_the_requested_width_and_moves_by_eighths() {
        for cache in [0u64, 1, 37, 99, 100] {
            for processed in cache..=100 {
                for w in [6usize, 10, 21, 40] {
                    let p = Prefill {
                        total: 100,
                        cache,
                        processed,
                        time_ms: 100,
                    };
                    assert_eq!(Line::new(bar(&p, w)).width(), w);
                }
            }
        }
        let mk = |processed| Prefill {
            total: 40_000,
            cache: 0,
            processed,
            time_ms: 1_000,
        };
        assert_ne!(
            Line::new(bar(&mk(10_000), 22)).plain(),
            Line::new(bar(&mk(10_300), 22)).plain()
        );
    }

    #[test]
    fn a_narrow_terminal_drops_the_estimate_before_the_percentage() {
        let p = Prefill {
            total: 41_233,
            cache: 3_100,
            processed: 20_000,
            time_ms: 5_000,
        };
        assert!(prefill_line(&p, 120).plain().contains("left"));
        let narrow = prefill_line(&p, 24).plain();
        assert!(!narrow.contains("left") && narrow.contains('%'), "{narrow}");
        for w in [12usize, 20, 30, 50, 80] {
            assert!(prefill_line(&p, w).width() <= w);
        }
    }

    /// letibot's bytes: the spinner painted inside the row, in the row's own colour.
    #[test]
    fn the_spinner_is_stacked_inside_the_row() {
        let t = TurnStatus {
            busy: true,
            elapsed_ms: Some(3_000),
            ..TurnStatus::default()
        };
        let l = t.line(80);
        assert_eq!(
            l.spans[0].style.roles().collect::<Vec<_>>(),
            vec![Role::Success, Role::Success]
        );
        assert_eq!(l.plain(), "⠋ Responding · 3.0s");
    }

    /// **Green while going, yellow when slow, and the same words either way**: a slow turn
    /// changes the row's colour and says nothing more about it.
    #[test]
    fn a_slow_turn_is_yellow_and_says_nothing_else() {
        let going = busy(20_000, 21_000);
        let slow = TurnStatus {
            slow: true,
            ..going
        };
        assert_eq!(role_of(&going.line(120), "Responding"), Some(Role::Success));
        assert_eq!(role_of(&slow.line(120), "Responding"), Some(Role::Pending));
        assert_eq!(going.line(120).plain(), slow.line(120).plain());
    }
}
