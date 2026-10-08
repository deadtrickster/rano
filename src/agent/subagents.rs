//! **The subagents pane**: the children a session spawned, their state and what each was
//! asked — and the frame of a child's output view.
//!
//! Ported from letibot's `ui/panes/subagents.rs` and the enumeration in its `app/panes.rs`.
//!
//! # One enumeration, two groups
//!
//! The rows come from [`SubagentsPane::stops`] — the same list a host's arrows, Enter and
//! `p` read — so the drawn `▸` and the key that acts cannot disagree about which row is
//! selected (the defect leticl's `todos-stops` docstring names). The children still going
//! are drawn first; the finished ones live under a `finished (N)` row that stays folded
//! unless the person unfolded it. The operator, 2026-10-06: *"i went to subagents panel and
//! dont see it here"* — a child just started, and the pane drew the finished ones and pushed
//! the running one off the bottom; and then *"please group finished separately in the
//! finished group which will be collapsed"*.

use crate::render::{Line, Span};
use crate::style::Role;

use super::pane::{self, PaneLines};
use super::text::clean_line;

/// One child, as the pane draws it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentRow {
    /// The child's own word — `opening`, `running`, `done`, `failed` — or empty for a row
    /// rebuilt from the daemon's session list, which says nothing about how a child ended.
    pub state: String,
    /// What it was asked, as one line (letibot's `subagent_asked`).
    pub asked: String,
    /// The child's session, as the host shortens an id.
    pub session: String,
    pub role: String,
    /// The child's own model; empty when it inherited its parent's.
    pub model: String,
    /// A turn is generating in it this instant (the daemon's own list measured that).
    pub generating: bool,
    /// Its answer, shown once it is `done`.
    pub answer: Option<String>,
}

impl SubagentRow {
    /// Settled: anything but `running` and `opening`.
    pub fn is_finished(&self) -> bool {
        !matches!(self.state.as_str(), "running" | "opening")
    }
}

/// **One row of the subagents pane** — the one enumeration a host's keys and the pane both
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubStop {
    /// A child, by index.
    Agent(usize),
    /// **The `finished` group row.** The finished children live under it, collapsed by
    /// default; Enter unfolds them.
    Finished,
}

/// The subagents pane's facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentsPane {
    pub agents: Vec<SubagentRow>,
    pub finished_open: bool,
    pub selected: usize,
}

impl SubagentsPane {
    /// **The children still going first, then one `finished (N)` row, then — only when it
    /// is unfolded — the finished children themselves.** The active half is never empty for
    /// a live spawn, which is the whole point: the row the person opened the pane to see is
    /// at the top.
    pub fn stops(&self) -> Vec<SubStop> {
        let mut out = Vec::with_capacity(self.agents.len() + 1);
        for (i, s) in self.agents.iter().enumerate() {
            if !s.is_finished() {
                out.push(SubStop::Agent(i));
            }
        }
        if self.agents.iter().any(SubagentRow::is_finished) {
            out.push(SubStop::Finished);
            if self.finished_open {
                for (i, s) in self.agents.iter().enumerate() {
                    if s.is_finished() {
                        out.push(SubStop::Agent(i));
                    }
                }
            }
        }
        out
    }

    /// Every row of the pane, each stop's first row recorded, cut to `w`.
    pub fn content(&self, w: usize) -> PaneLines {
        let mut out = PaneLines::new();
        out.push(pane::title("subagents"));
        out.blank();
        if self.agents.is_empty() {
            out.push(pane::faint(
                "    none spawned yet. The model spawns them with the task tool.",
            ));
        }
        let stops = self.stops();
        let cursor = self.selected.min(stops.len().saturating_sub(1));
        for (k, stop) in stops.iter().enumerate() {
            let picked = k == cursor;
            let i = match *stop {
                // **The `finished` fold, when the cursor is on it.** A group row and not a
                // child: there is nobody to switch into, and Enter folds or unfolds.
                SubStop::Finished => {
                    let n = self.agents.iter().filter(|s| s.is_finished()).count();
                    let fold = if self.finished_open { "[-]" } else { "[+]" };
                    let left = Line::raw(format!("{} {fold} finished ({n})", pane::mark(picked)));
                    out.push_stop(pane::picked(left, picked));
                    out.push(pane::faint(if self.finished_open {
                        "       the ones that have ended · enter folds them away"
                    } else {
                        "       enter shows the ones that have ended"
                    }));
                    continue;
                }
                SubStop::Agent(i) => i,
            };
            let s = &self.agents[i];
            let (mark, role) = match s.state.as_str() {
                // Not a session yet: the child is copying its workspace or booting. Enter
                // does nothing here, and the row says so below.
                "opening" => ("[…]", Role::Plain),
                "running" => ("[~]", Role::Pending),
                "done" => ("[x]", Role::Success),
                "failed" => ("[!]", Role::Failure),
                // **No state word, which is the honest reading for a row rebuilt from the
                // daemon's session list.** That list says whether a turn is generating and
                // nothing about how a settled child ended, so `[?]` means *the host was not
                // watching when it happened* — `done` or `failed` would be an invention.
                _ => ("[?]", Role::Plain),
            };
            // **The task, drawn whole**, the row's question.
            let left = Line::new(vec![
                Span::raw(format!("{} ", pane::mark(picked))),
                Span::role(mark, role),
                Span::raw(format!(" {}", clean_line(&s.asked))),
            ]);
            out.push_stop(pane::picked(left, picked));
            // **The row's own facts, as clauses that VANISH when the daemon did not say
            // them.** A row rebuilt from the session list carries a child's name and its
            // model and no role and no state, so a fixed `role {role} · {state}` would draw
            // `role · ` with two holes in it on every rebuilt row.
            let mut facts: Vec<String> = vec![clean_line(&s.session)];
            if !s.role.is_empty() {
                facts.push(format!("role {}", clean_line(&s.role)));
            }
            // **The child's own model, when it has one** — the operator's ask, 2026-10-05: a
            // tree of children on different models is a fact the pane has to show.
            if !s.model.is_empty() {
                facts.push(format!("on {}", clean_line(&s.model)));
            }
            // **The state word, or the word for not having one**, and the second is a fact
            // about the host rather than about the child — so it says which.
            if s.state.is_empty() {
                facts.push("state unknown".into());
            } else if s.state == "running" && !s.generating {
                // **A child that is up with no turn generating this instant** — parked on its
                // own background job, or between two rounds: alive, and not generating, and
                // the row says both rather than `running` for a child that is not.
                facts.push("waiting".into());
            } else {
                facts.push(clean_line(&s.state));
            }
            // **The answer as the subtitle, where it belongs**: the row is the question.
            if let (Some(a), "done") = (&s.answer, s.state.as_str()) {
                facts.push(clean_line(a));
            }
            if s.state == "opening" {
                facts.push("not attachable yet".into());
            }
            out.push(pane::faint(format!("       {}", facts.join(" · "))));
        }
        out.blank();
        out.push(pane::faint(
            "    arrows move · enter switches into the subagent (o does the same), or folds the \
             finished group · p reads its output · esc closes this, and from inside a subagent \
             esc goes up to the parent",
        ));
        out.trimmed(w)
    }
}

/// **The frame of a child's output view**: its title and notes above, the footer below,
/// and which of the host's rows show between them.
///
/// The rows are the host's, already drawn (they are the child's own transcript rows, through
/// the host's renderer), so this lays out the window over them rather than drawing them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentOutput {
    /// The child's session, as the host shortens an id.
    pub session: String,
    /// Earlier events that fell off the daemon's scrollback before this read.
    pub dropped: u64,
    /// **The daemon answered with its event ring, not the session's rows**, so what follows
    /// is drawn plainly. A reader who cannot tell the two apart cannot tell a session from a
    /// list of its events, and the sentence names the way to get the real one.
    pub degraded: bool,
    /// Where the full output was written, if it was.
    pub spill: Option<String>,
}

/// Where the host's rows go in [`SubagentOutput`]'s frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputLayout {
    /// The rows above the window.
    pub head: Vec<Line>,
    /// The host's rows to show, `start..end`, the tail by default.
    pub start: usize,
    pub end: usize,
    /// Blank rows under them, so the footer sits on the last row of the room.
    pub pad: usize,
    pub footer: Line,
    /// The scroll actually used, clamped: store it back.
    pub scroll: usize,
}

impl SubagentOutput {
    /// The frame for `len` host rows in `room` rows, scrolled `scroll` rows up from the end.
    pub fn layout(&self, room: usize, len: usize, scroll: usize) -> OutputLayout {
        let mut head = vec![pane::title(format!(
            "subagent output — {}",
            clean_line(&self.session)
        ))];
        if self.dropped > 0 {
            head.push(pane::faint(format!(
                "    {} earlier event{} fell off the daemon's scrollback before this read",
                self.dropped,
                if self.dropped == 1 { "" } else { "s" }
            )));
        }
        if self.degraded {
            head.push(pane::faint(
                "    this daemon answered with its event ring, not this session's rows — what \
                 follows is drawn plainly, without the tool cards or the markdown. A daemon \
                 built with `Peeked::snapshot` draws it as a session.",
            ));
        }
        head.push(Line::default());
        let footer_rows = 1;
        let visible = room.saturating_sub(head.len() + footer_rows).max(1);
        let scroll = scroll.min(len.saturating_sub(visible));
        let end = len - scroll;
        let start = end.saturating_sub(visible);
        let pad = room
            .saturating_sub(footer_rows)
            .saturating_sub(head.len() + (end - start));
        let spill = self.spill.as_deref().unwrap_or("not written");
        OutputLayout {
            head,
            start,
            end,
            pad,
            footer: pane::faint(format!(
                "    arrows scroll, Enter re-reads, Esc back — full: {}",
                clean_line(spill)
            )),
            scroll,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{plain, role_of};
    use crate::style::Palette;

    fn child(state: &str, asked: &str) -> SubagentRow {
        SubagentRow {
            state: state.into(),
            asked: asked.into(),
            session: "s-1a2b".into(),
            generating: true,
            ..SubagentRow::default()
        }
    }

    /// The running children first, the finished ones folded under one row.
    #[test]
    fn the_running_children_come_first_and_the_finished_ones_fold() {
        let mut p = SubagentsPane {
            agents: vec![
                child("done", "scout the docs"),
                child("running", "port the widgets"),
            ],
            finished_open: false,
            selected: 0,
        };
        assert_eq!(p.stops(), vec![SubStop::Agent(1), SubStop::Finished]);
        let rows = plain(&p.content(100).lines);
        assert_eq!(rows[2], "▸ [~] port the widgets");
        assert!(
            rows.iter().any(|l| l.contains("[+] finished (1)")),
            "{rows:#?}"
        );
        assert!(!rows.iter().any(|l| l.contains("scout the docs")));
        p.finished_open = true;
        assert_eq!(
            p.stops(),
            vec![SubStop::Agent(1), SubStop::Finished, SubStop::Agent(0)]
        );
    }

    /// A rebuilt row says the head does not know its state, and a parked child says it is
    /// waiting rather than running.
    #[test]
    fn a_row_says_only_what_the_daemon_said() {
        let mut parked = child("running", "wait on the build");
        parked.generating = false;
        let rebuilt = child("", "a child from the list");
        let p = SubagentsPane {
            agents: vec![parked, rebuilt],
            finished_open: true,
            selected: 0,
        };
        let rows = plain(&p.content(100).lines);
        assert!(
            rows.iter().any(|l| l == "       s-1a2b · waiting"),
            "{rows:#?}"
        );
        assert!(rows.iter().any(|l| l.contains("[?] a child from the list")));
        assert!(rows.iter().any(|l| l == "       s-1a2b · state unknown"));
    }

    /// The highlighted child is inverse with its state mark painted inside it.
    #[test]
    fn the_highlighted_child_is_inverse() {
        let p = SubagentsPane {
            agents: vec![child("running", "port")],
            finished_open: false,
            selected: 0,
        };
        let l = &p.content(100).lines[2];
        assert_eq!(role_of(l, "[~]"), Some(Role::Pending));
        assert!(
            l.to_ansi(Palette::Colour).starts_with("\x1b[7m▸ \x1b[0m"),
            "{:?}",
            l.to_ansi(Palette::Colour)
        );
    }

    /// The output frame shows the tail, pads to the room, and clamps the scroll.
    #[test]
    fn the_output_frame_shows_the_tail_and_clamps_the_scroll() {
        let o = SubagentOutput {
            session: "s-1".into(),
            dropped: 2,
            ..SubagentOutput::default()
        };
        let l = o.layout(10, 30, 0);
        assert_eq!(l.head.len(), 3);
        assert_eq!((l.start, l.end), (24, 30));
        assert_eq!(l.pad, 0);
        let l = o.layout(10, 30, 999);
        assert_eq!((l.start, l.end, l.scroll), (0, 6, 24));
        let short = o.layout(10, 2, 0);
        assert_eq!((short.start, short.end, short.pad), (0, 2, 4));
        assert!(short.footer.plain().ends_with("full: not written"));
    }
}
