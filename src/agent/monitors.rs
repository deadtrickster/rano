//! **The monitors pane**: what this session is watching across turns, and what it stopped
//! watching and why.
//!
//! A monitor is a watch the model armed with `monitor` — a timer, a job, a file, a port, a
//! process or a command — that wakes the session when it fires. Until this pane one was seen
//! only when it fired: a watch that was still waiting was on no screen at all, while it held
//! the session's daemon up. Every field is the daemon's answer; the widget decides colour and
//! layout and nothing else.

use crate::agent::pane::{self, PaneLines};
use crate::agent::text::{clean_line, duration, wrap};
use crate::render::{Line, Span};
use crate::style::Role;

/// One monitor, as the daemon's table reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MonitorRow {
    /// The model's name for it — the handle it renews or retires it by.
    pub name: String,
    /// What it watches, in the daemon's words: `job j4 leaving the running state`.
    pub watching: String,
    /// Which scope reaps it: `session`, `turn`, or a named scope.
    pub owner: String,
    /// Who armed it: `turn 12`.
    pub declared_by: String,
    /// Continuous: it re-arms after each firing rather than settling.
    pub repeat: bool,
    /// Still watching. A row that is not live has ended, and `ended` says how.
    pub live: bool,
    /// When its time runs out, in unix milliseconds — `0` when the daemon did not say.
    pub ends_at_ms: u64,
    /// How many times it has fired.
    pub fired: u32,
    /// How it ended, in the daemon's words: `FIRED after 3.0s — …`, `expired after 300s
    /// WITHOUT firing; …`, `retired by the operator after 12s`. Empty while live.
    pub ended: String,
}

/// The monitors pane's view model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MonitorsPane {
    pub monitors: Vec<MonitorRow>,
    /// Now, in unix milliseconds — what the time left is counted against. The host's clock,
    /// passed in so the drawing is a function of its inputs.
    pub now_ms: u64,
    /// The cursor, an index into [`MonitorsPane::stops`]; clamped when drawn.
    pub selected: usize,
}

impl MonitorsPane {
    /// **What the cursor can rest on**: the live monitors, by index into
    /// [`MonitorsPane::monitors`]. An ended one is a record to read, not a thing to act on.
    pub fn stops(&self) -> Vec<usize> {
        (0..self.monitors.len())
            .filter(|&i| self.monitors[i].live)
            .collect()
    }

    /// The live monitor under the cursor, if there is one.
    pub fn selected_monitor(&self) -> Option<&MonitorRow> {
        let stops = self.stops();
        let i = *stops.get(self.selected.min(stops.len().saturating_sub(1)))?;
        self.monitors.get(i)
    }

    /// How many are still watching — the composer edge's `N monitors watching`.
    pub fn watching(&self) -> usize {
        self.monitors.iter().filter(|m| m.live).count()
    }

    pub fn content(&self, w: usize) -> PaneLines {
        let mut out = PaneLines::new();
        out.push(pane::title("monitors"));
        out.blank();
        if self.monitors.is_empty() {
            out.push(pane::faint(
                "    none. The model arms one with `monitor` — a timer, a job, a file, a port, \
                 a process or a command — and it wakes the session when it fires.",
            ));
        }
        let stops = self.stops();
        let cursor = self.selected.min(stops.len().saturating_sub(1));
        for (k, &i) in stops.iter().enumerate() {
            let picked = k == cursor;
            let m = &self.monitors[i];
            let left = Line::new(vec![
                Span::raw(format!("{} ", pane::mark(picked))),
                Span::role("[~]", Role::Pending),
                Span::raw(format!(" {}", clean_line(&m.name))),
            ]);
            out.push_stop(pane::picked(left, picked));
            // **What it watches, whole** — the operator: *"i want to see monitor command in the
            // details"*. A command is the one thing a person reads a monitor for, and it used to
            // ride the row's first line, cut at the pane's edge.
            for l in watching_lines(&m.watching, "watching", w) {
                out.push(Line::raw(l));
            }
            let mut facts: Vec<String> = Vec::new();
            facts.push(if m.repeat { "repeats" } else { "once" }.into());
            if m.fired > 0 {
                facts.push(format!("fired {}×", m.fired));
            }
            if m.ends_at_ms > 0 {
                facts.push(format!(
                    "{} left",
                    duration(m.ends_at_ms.saturating_sub(self.now_ms))
                ));
            }
            if !m.declared_by.is_empty() {
                facts.push(format!("declared by {}", clean_line(&m.declared_by)));
            }
            if !m.owner.is_empty() {
                facts.push(format!("ends with its {}", clean_line(&m.owner)));
            }
            out.push(pane::faint(format!("         {}", facts.join(" · "))));
        }
        let ended: Vec<&MonitorRow> = self.monitors.iter().filter(|m| !m.live).collect();
        if !ended.is_empty() {
            out.blank();
            out.push(pane::faint(format!("    ended ({})", ended.len())));
            for m in ended {
                let (mark, role) = if m.ended.starts_with("FIRED") {
                    ("[x]", Role::Success)
                } else {
                    ("[-]", Role::Faint)
                };
                out.push(Line::new(vec![
                    Span::raw("    "),
                    Span::role(mark, role),
                    Span::role(
                        format!(" {} — {}", clean_line(&m.name), clean_line(&m.ended)),
                        Role::Faint,
                    ),
                ]));
                if !m.watching.is_empty() {
                    for l in watching_lines(&m.watching, "was watching", w) {
                        out.push(pane::faint(l));
                    }
                }
            }
        }
        out.blank();
        out.push(pane::faint(if stops.is_empty() {
            "    a monitor shows here from the moment it is armed until it ends."
        } else {
            "    x retires the one under the cursor; the model is told it was retired."
        }));
        out.trimmed(w)
    }
}

/// `{what} {watching}`, wrapped under a row at the detail column — never cut, so a long command
/// reads whole.
fn watching_lines(watching: &str, what: &str, w: usize) -> Vec<String> {
    const INDENT: &str = "         ";
    let room = w.saturating_sub(INDENT.len()).max(20);
    wrap(&format!("{what} {}", clean_line(watching)), room)
        .into_iter()
        .map(|l| format!("{INDENT}{l}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::plain;

    fn live(name: &str) -> MonitorRow {
        MonitorRow {
            name: name.into(),
            watching: "a timer of 60s".into(),
            owner: "session".into(),
            declared_by: "turn 4".into(),
            repeat: true,
            live: true,
            ends_at_ms: 1_000_000 + 252_000,
            fired: 3,
            ended: String::new(),
        }
    }

    /// **A waiting monitor is a row, with what it watches and how long it has** — the thing that
    /// was on no screen before; an ended one is listed below with how it ended, and is not a
    /// stop.
    #[test]
    fn live_monitors_are_rows_and_ended_ones_say_how_they_ended() {
        let p = MonitorsPane {
            monitors: vec![
                live("tick"),
                MonitorRow {
                    name: "build".into(),
                    watching: "job j4".into(),
                    live: false,
                    ended: "FIRED after 3.0s — it exited 0".into(),
                    ..MonitorRow::default()
                },
                MonitorRow {
                    name: "port".into(),
                    live: false,
                    ended: "expired after 300s WITHOUT firing; at the last look, closed".into(),
                    ..MonitorRow::default()
                },
            ],
            now_ms: 1_000_000,
            selected: 0,
        };
        assert_eq!(p.stops(), vec![0]);
        assert_eq!(p.watching(), 1);
        assert_eq!(p.selected_monitor().map(|m| m.name.as_str()), Some("tick"));
        let rows = plain(&p.content(120).lines);
        assert_eq!(rows[0], "monitors");
        assert_eq!(rows[2], "▸ [~] tick");
        assert_eq!(rows[3], "         watching a timer of 60s");
        assert_eq!(
            rows[4],
            "         repeats · fired 3× · 4m12s left · declared by turn 4 · ends with its session"
        );
        assert!(rows.contains(&"    ended (2)".to_string()), "{rows:#?}");
        assert!(
            rows.contains(&"    [x] build — FIRED after 3.0s — it exited 0".to_string()),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|r| r.starts_with("    [-] port — expired")),
            "{rows:#?}"
        );
        assert!(rows.last().unwrap().contains("x retires"), "{rows:#?}");
    }

    /// **A long command reads whole, wrapped under its row** — not cut at the pane's edge.
    #[test]
    fn a_monitors_command_is_shown_whole() {
        let cmd = "command `curl -sf http://127.0.0.1:8080/health --max-time 5 -o /dev/null -w %{http_code}` exiting";
        let p = MonitorsPane {
            monitors: vec![MonitorRow {
                watching: cmd.into(),
                ..live("health")
            }],
            now_ms: 1_000_000,
            selected: 0,
        };
        let rows = plain(&p.content(60).lines);
        let shown: String = rows[3..]
            .iter()
            .take_while(|r| !r.contains("repeats"))
            .map(|r| r.trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(shown, format!("watching {cmd}"), "{rows:#?}");
        assert!(rows.iter().all(|r| r.chars().count() <= 60), "{rows:#?}");
    }

    #[test]
    fn an_empty_pane_says_what_a_monitor_is() {
        let p = MonitorsPane::default();
        assert!(p.stops().is_empty() && p.selected_monitor().is_none());
        let rows = plain(&p.content(200).lines);
        assert!(
            rows[2].contains("none. The model arms one with `monitor`"),
            "{rows:#?}"
        );
    }
}
