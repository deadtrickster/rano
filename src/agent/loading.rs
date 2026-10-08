//! **The waits that have a size**: a snapshot's rows filling in, a compaction reading its
//! half of the conversation — a bar, the counts, and the cat that says the head is alive.
//!
//! Ported from letibot's `ui/loading.rs`. The bar is the prefill bar
//! ([`super::turn_status::bar`]): one instrument for every wait the head can measure.

use crate::render::{Line, Span};
use crate::style::Role;

use super::header::thousands;
use super::turn_status::{Prefill, bar};

/// **The cat**, one frame every 120 ms. Eight frames, two of them with a tail: a thing on
/// the screen that moves is the difference between *it is working* and *it is stuck*, and
/// a spinner beside a bar that is not moving reads as the second.
pub const CAT_FRAMES: [&str; 8] = [
    "(=^.^=) ", "(=^-^=) ", "(=^o^=) ", "(=^-^=) ", "(=^.^=)~", "(=^-^=)~", "(=^o^=)~", "(=^-^=)~",
];

/// The widest frame: the slot the cat is drawn in, so the counts beside it do not move.
pub const CAT_SLOT: usize = {
    let mut w = 0;
    let mut i = 0;
    while i < CAT_FRAMES.len() {
        let n = CAT_FRAMES[i].len();
        if n > w {
            w = n;
        }
        i += 1;
    }
    w
};

/// The cat's frame at `elapsed_ms`.
pub fn cat_frame(elapsed_ms: u64) -> &'static str {
    CAT_FRAMES[((elapsed_ms / 120) % CAT_FRAMES.len() as u64) as usize]
}

/// `text`, faint, centred in `w` columns (left as it is when it does not fit).
pub fn centred(text: &str, w: usize) -> Line {
    let taken = super::text::visible_width(text);
    let mut l = Line::default();
    if taken < w {
        l.push(Span::raw(" ".repeat((w - taken) / 2)));
    }
    l.push(Span::role(text.to_string(), Role::Faint));
    l
}

/// The bar row: two columns in, the bar, the counts and the cat after it — the bar as wide
/// as what is left, between eight and forty columns.
fn bar_row(p: &Prefill, counts: String, now_ms: u64, w: usize) -> Line {
    let used = CAT_SLOT + counts.chars().count() + 6;
    let bar_cols = w.saturating_sub(used).clamp(8, 40);
    let mut l = Line::raw("  ");
    l.spans.extend(bar(p, bar_cols));
    l.push(Span::raw(" "));
    l.push(Span::role(counts, Role::Faint));
    l.push(Span::raw("  "));
    l.push(Span::role(
        format!("{cat:<CAT_SLOT$}", cat = cat_frame(now_ms)),
        Role::Faint,
    ));
    l
}

/// **A snapshot's rows filling in**: `done` of `total` `unit`, what is filling, and the cat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filling {
    pub what: String,
    pub unit: String,
    pub done: u64,
    pub total: u64,
    pub now_ms: u64,
}

impl Filling {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let p = Prefill {
            total: self.total,
            cache: self.done,
            processed: self.done,
            time_ms: 0,
        };
        let total_s = thousands(self.total);
        let counts = format!(
            "{:>n$} of {total_s} {}",
            thousands(self.done),
            self.unit,
            n = total_s.chars().count()
        );
        vec![
            Line::default(),
            bar_row(&p, counts, self.now_ms, w),
            Line::new(vec![Span::role(format!("  {}", self.what), Role::Faint)]),
        ]
    }
}

/// **A compaction under way**: which half, how far it has read, or how much it has written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compacting {
    pub half: u64,
    pub halves: u64,
    /// The half's prompt, and how much of it the model has read.
    pub prompt_tokens: u64,
    pub processed: u64,
    /// What it has written so far, once it is writing, in `unit`.
    pub written: u64,
    pub unit: String,
    pub now_ms: u64,
}

impl Compacting {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let where_ = format!("half {} of {}", self.half, self.halves);
        // **Reading, then writing**: while the half's prompt is being read the bar measures
        // it; once the model writes, the count of what it has written is the only number
        // there is, and a bar over it would be a bar to a total nobody knows.
        if self.processed > 0 && self.processed < self.prompt_tokens {
            let p = Prefill {
                total: self.prompt_tokens,
                cache: 0,
                processed: self.processed,
                time_ms: 0,
            };
            let total_s = thousands(self.prompt_tokens);
            let counts = format!(
                "{:>n$} of {total_s} tokens read",
                thousands(self.processed),
                n = total_s.chars().count()
            );
            return vec![
                Line::default(),
                bar_row(&p, counts, self.now_ms, w),
                Line::new(vec![Span::role(
                    format!(
                        "  compacting {where_} — nothing shows on the transcript until it \
                         lands, and the conversation is kept either way"
                    ),
                    Role::Faint,
                )]),
            ];
        }
        vec![
            Line::default(),
            Line::new(vec![
                Span::raw("  "),
                Span::role(
                    format!("{cat:<CAT_SLOT$}", cat = cat_frame(self.now_ms)),
                    Role::Faint,
                ),
                Span::raw(" "),
                Span::role(
                    format!(
                        "compacting {where_} — {} {} written so far",
                        thousands(self.written),
                        self.unit
                    ),
                    Role::Faint,
                ),
            ]),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::plain;

    /// The counts keep their column as the number grows, and the bar stays in its bounds.
    #[test]
    fn a_filling_snapshot_keeps_its_counts_in_one_place() {
        let at = |done| {
            Filling {
                what: "rows of the conversation".into(),
                unit: "rows".into(),
                done,
                total: 1_200,
                now_ms: 0,
            }
            .lines(80)
        };
        let a = plain(&at(5));
        let b = plain(&at(1_100));
        assert_eq!(a[1].find(" of "), b[1].find(" of "), "{a:?} {b:?}");
        assert!(a[1].starts_with("  ▐"), "{a:?}");
        assert_eq!(a[2], "  rows of the conversation");
    }

    /// Writing is a count, not a bar.
    #[test]
    fn a_compaction_that_is_writing_says_how_much() {
        let c = Compacting {
            half: 1,
            halves: 2,
            written: 2_400,
            unit: "tokens".into(),
            ..Compacting::default()
        };
        let l = plain(&c.lines(80));
        assert!(
            l[1].ends_with("compacting half 1 of 2 — 2400 tokens written so far"),
            "{l:?}"
        );
        assert!(!l[1].contains('▐'));
    }

    #[test]
    fn the_cat_moves_and_a_centred_row_is_centred() {
        assert_ne!(cat_frame(0), cat_frame(120));
        assert_eq!(cat_frame(0), cat_frame(960));
        assert_eq!(centred("abc", 9).plain(), "   abc");
    }
}
