//! **What the head says about itself**: the `/status` page of its counters, each with what
//! it means, and the three rows that interrupt the frame when something is wrong — the link
//! down, a stop being waited on, an alarm.
//!
//! Ported from letibot's `ui/status.rs`. The facts are the host's; this lays them out.

use crate::render::{Line, Span};
use crate::style::Role;

use super::text::{one, trim_to, wrap};

/// One counter: its name, its value now, and what it means.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fact {
    pub key: String,
    pub value: String,
    pub why: String,
}

/// **A page of facts**: a title, an optional note under it, and each fact as its name in a
/// twelve-column faint field, its value at full strength, and its meaning wrapped faint under
/// it — **every counter says what it counts**, because a number a reader has to look up is a
/// number they will not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactsPane {
    pub title: String,
    /// A sentence under the title (the alarm's acknowledgement), with air under it.
    pub note: Option<String>,
    pub facts: Vec<Fact>,
    pub footer: String,
}

impl FactsPane {
    pub fn lines(&self, w: usize) -> Vec<Line> {
        let mut out = vec![one(self.title.clone(), Role::Strong), Line::default()];
        if let Some(n) = &self.note {
            out.push(one(format!("  {n}"), Role::Faint));
            out.push(Line::default());
        }
        for f in &self.facts {
            out.push(Line::new(vec![
                Span::role(format!("  {:<12}", f.key), Role::Faint),
                Span::raw(f.value.clone()),
            ]));
            for l in wrap(&f.why, w.saturating_sub(16)) {
                out.push(Line::new(vec![
                    Span::raw(" ".repeat(14)),
                    Span::role(l, Role::Faint),
                ]));
            }
            out.push(Line::default());
        }
        out.push(one(format!("  {}", self.footer), Role::Faint));
        out
    }
}

super::lines_widget!(FactsPane);

/// **A warning that takes rows of the frame** — the daemon connection down, a stop still
/// being waited on: `⚠` and the sentence, wrapped, in the notice register.
pub fn warning(text: &str, w: usize) -> Vec<Line> {
    wrap(&format!("⚠ {text}"), w)
        .into_iter()
        .map(|l| one(l, Role::Pending))
        .collect()
}

/// **The alarm row**: the counters that moved, in the attention register, on one row cut to
/// the width.
pub fn alarm(text: &str, w: usize) -> Line {
    Line::new(vec![Span::role(trim_to(text, w), Role::Attention)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::plain;
    use crate::style::Palette;

    /// Each fact says what it counts, wrapped under its value.
    #[test]
    fn every_counter_says_what_it_counts() {
        let p = FactsPane {
            title: "this head".into(),
            note: None,
            facts: vec![Fact {
                key: "dropped".into(),
                value: "0".into(),
                why: "Events the daemon's bounded scrollback threw away before this head asked \
                      for them."
                    .into(),
            }],
            footer: "/status or esc closes this".into(),
        };
        let rows = plain(&p.lines(60));
        assert_eq!(rows[0], "this head");
        assert_eq!(rows[2], "  dropped     0");
        assert!(rows[3].starts_with(&" ".repeat(14)), "{rows:#?}");
        assert_eq!(rows.last().unwrap(), "  /status or esc closes this");
        assert_eq!(
            p.lines(60)[2].to_ansi(Palette::Colour),
            "\x1b[2m  dropped     \x1b[0m0"
        );
    }

    #[test]
    fn a_warning_wraps_in_the_notice_register() {
        let l = warning("the daemon connection is down — reconnecting.", 20);
        assert!(l.len() > 1);
        assert!(l[0].plain().starts_with("⚠ the"));
        assert_eq!(l[0].spans[0].style.top(), Role::Pending);
        assert_eq!(alarm("⚠ dropped 3 · /status", 10).width(), 10);
    }
}
