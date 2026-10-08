//! **The jobs pane**: the background jobs a session started, and one job's output.
//!
//! Ported from letibot's `ui/panes/jobs.rs` (`jobs_lines`, `job_out_lines`, `jobs_line`)
//! and `app/panes.rs` (`job_stops`). Every field here is the daemon's answer; the widget
//! decides colour and layout and nothing else — no join against the current turn, because
//! the process table is not a thing a head reconstructs.

use crate::agent::pane::{self, PaneLines};
use crate::agent::text::{bytes_human, clean, clean_line};
use crate::render::{Line, Span};
use crate::style::Role;

/// One background job, as the daemon's job table reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobRow {
    /// The handle: `j4`.
    pub id: String,
    /// The command line, as the daemon sent it.
    pub command: String,
    /// How it came to be in the background — `asked`, `promoted`, `promoted by dead`.
    pub how: String,
    /// The daemon's state word: `running`, `exited 0`, `signalled 9`, `not run (…)`.
    pub state: String,
    pub running: bool,
    /// The job never started (its scope could not be joined): it has no duration.
    pub never_ran: bool,
    /// The file its output goes to instead of the window, when the command redirected it.
    pub redirect: Option<String>,
    /// Bytes it has written.
    pub produced: u64,
    pub elapsed_ms: u64,
}

/// What the cursor can rest on in the jobs pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStop {
    /// A job, by its index in [`JobsPane::jobs`].
    Job(usize),
    /// The `finished (N)` group row, which Enter folds and unfolds.
    Finished,
}

/// The jobs pane's view model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobsPane {
    pub jobs: Vec<JobRow>,
    /// Whether the `finished (N)` group is unfolded.
    pub finished_open: bool,
    /// The cursor, an index into [`JobsPane::stops`]; clamped when drawn.
    pub selected: usize,
}

impl JobsPane {
    /// **The one enumeration** of what the cursor can rest on — running jobs, then one
    /// `finished` group row, then (unfolded) the settled jobs. The arrows, Enter and the
    /// drawn `▸` all read this, so the row the cursor is drawn on is the row Enter takes.
    pub fn stops(&self) -> Vec<JobStop> {
        let mut out = Vec::with_capacity(self.jobs.len() + 1);
        for (i, j) in self.jobs.iter().enumerate() {
            if j.running {
                out.push(JobStop::Job(i));
            }
        }
        if self.jobs.iter().any(|j| !j.running) {
            out.push(JobStop::Finished);
            if self.finished_open {
                for (i, j) in self.jobs.iter().enumerate() {
                    if !j.running {
                        out.push(JobStop::Job(i));
                    }
                }
            }
        }
        out
    }

    /// **How many jobs are running, for the composer's top edge** — R51 item 5.
    ///
    /// `None` when the answer is zero, and absent rather than `0 jobs`: a count that is
    /// always there is furniture, and the edge it sits on is spent on facts that are true
    /// only while they are.
    ///
    /// **And the redirected half.** A job whose output goes to a file has a window that will
    /// be EMPTY however long it runs (R41), so *"3 jobs running"* and *"3 jobs running, one
    /// of which you cannot watch"* are different answers:
    ///
    /// ```text
    /// 3 jobs running · 1 to a file
    /// ```
    ///
    /// A settled job's redirect is not counted: the row is about what is running now.
    pub fn running_line(&self) -> Option<String> {
        let running = self.jobs.iter().filter(|j| j.running).count();
        if running == 0 {
            return None;
        }
        let mut out = format!(
            "{running} job{} running",
            if running == 1 { "" } else { "s" }
        );
        let to_a_file = self
            .jobs
            .iter()
            .filter(|j| j.running && j.redirect.is_some())
            .count();
        if to_a_file > 0 {
            out.push_str(&format!(" · {to_a_file} to a file"));
        }
        Some(out)
    }

    /// **The daemon's job table, as rows** — running first, then one folded `finished (N)`
    /// row. The operator's own ask: *"jobs panel - same as subagents - show list of running,
    /// group finished"*. Each stop's row is recorded in the result, which is what the arrows
    /// scroll to and a click hits.
    pub fn content(&self, w: usize) -> PaneLines {
        let mut out = PaneLines::new();
        out.push(pane::title("background jobs"));
        out.blank();
        if self.jobs.is_empty() {
            out.push(pane::faint(
                "    none. The model backgrounds a command with bash's `background: \
                 true`; ctrl-o moves the running one.",
            ));
        }
        let stops = self.stops();
        let cursor = self.selected.min(stops.len().saturating_sub(1));
        for (k, stop) in stops.iter().enumerate() {
            let picked = k == cursor;
            let i = match *stop {
                // **The `finished` fold, when the cursor is on it.** A group row and not a job:
                // there is nothing to read, and Enter folds or unfolds the settled ones.
                JobStop::Finished => {
                    let n = self.jobs.iter().filter(|j| !j.running).count();
                    let fold = if self.finished_open { "[-]" } else { "[+]" };
                    let left = Line::raw(format!("{} {fold} finished ({n})", pane::mark(picked)));
                    out.push_stop(pane::picked(left, picked));
                    out.push(pane::faint(if self.finished_open {
                        "       the ones that have settled · enter folds them away"
                    } else {
                        "       enter shows the ones that have settled"
                    }));
                    continue;
                }
                JobStop::Job(i) => i,
            };
            let j = &self.jobs[i];
            let (mark, role) = if j.running {
                ("[~]", Role::Pending)
            } else if j.state.starts_with("exited 0") {
                ("[x]", Role::Success)
            } else {
                ("[!]", Role::Failure)
            };
            out.push_stop(Line::new(vec![
                Span::raw(format!("{} ", pane::mark(picked))),
                Span::role(mark, role),
                Span::raw(format!(" {} {}", clean_line(&j.id), clean_line(&j.command))),
            ]));
            // **A job that never ran has no duration, and the row must not claim one**
            // (A.2, §11.6). It read
            //
            //     not run (could not join its scope) · 0 B out · ran 0.0s
            //
            // — the state word denying *ran* two fields before the row said it. The byte
            // count stays: it is a measurement that exists (nothing was produced), and the
            // word beside it is what says why.
            let state = clean_line(&j.state);
            let tail = if j.running {
                format!("running · {} out so far", bytes_human(j.produced))
            } else if j.never_ran {
                format!("{state} · {} out", bytes_human(j.produced))
            } else {
                format!(
                    "{state} · {} out · ran {}.{:01}s",
                    bytes_human(j.produced),
                    j.elapsed_ms / 1000,
                    (j.elapsed_ms % 1000) / 100,
                )
            };
            out.push(pane::faint(format!(
                "         {} · {tail}",
                clean_line(&j.how)
            )));
            // **And where the output actually went, when it did not come here** (R41), on a
            // line of its own so the file is readable rather than another clause on a row
            // that is already long. The operator's own question — *"im not sure it lets me to
            // see that in the jobs details, when i 'enter' a job"* — answered in the list
            // they are looking at, before they Enter and find an empty window.
            if let Some(path) = &j.redirect {
                out.push(pane::faint(format!(
                    "         → {} (its output is there, not in the window)",
                    clean_line(path)
                )));
            }
        }
        out.blank();
        out.push(pane::faint(
            "    a job still shows running until the daemon says it settled — between \
             turns, that saying is the daemon's alone.",
        ));
        out.trimmed(w)
    }
}

/// One job's output window, as the daemon sent it: **offsets** rather than a sentence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobOutput {
    pub job: String,
    /// The daemon refused the read; its sentence.
    pub error: Option<String>,
    /// A read is in flight.
    pub loading: bool,
    /// The daemon's state word for the job, empty until the first answer.
    pub state: String,
    pub never_ran: bool,
    pub redirect: Option<String>,
    /// The window's byte offsets and the job's total.
    pub from: u64,
    pub to: u64,
    pub produced: u64,
    /// Bytes gone off the front of the daemon's retained log.
    pub dropped: u64,
    /// The window's lines.
    pub lines: Vec<String>,
    /// Lines scrolled back from the tail (the host's state; clamped when drawn).
    pub scroll: usize,
    /// The daemon named a next page.
    pub has_next: bool,
    /// The host holds a page to go back to.
    pub has_back: bool,
}

impl JobOutput {
    /// The view, exactly `room` rows or fewer, and the scroll actually used.
    ///
    /// The tail shows by default; the arrows walk back toward the beginning of the loaded
    /// window; the footer, pinned to the bottom row, says how much further the reader can go.
    pub fn lines(&self, room: usize) -> (Vec<Line>, usize) {
        let mut out = vec![pane::title(format!(
            "job output — {}",
            clean_line(&self.job)
        ))];
        // A refusal is not a window: the daemon could not answer, so the pane says what it
        // said rather than drawing an empty log the operator would read as "the job wrote
        // nothing".
        if let Some(err) = &self.error {
            out.push(pane::faint("    the daemon refused this read:"));
            out.push(Line::default());
            out.extend(clean(err).lines().map(Line::raw));
            out.push(Line::default());
            out.push(pane::faint("    Esc back to jobs"));
            out.truncate(room);
            return (out, 0);
        }
        // The state and the measurement on one line, because they are one fact: what the job
        // is, and what window of how much is on screen. A `dropped` count is said here rather
        // than in the footer — a window that begins mid-log must not be read as the job's
        // beginning.
        let state = clean_line(&self.state);
        if self.loading && self.state.is_empty() {
            out.push(pane::faint("    reading…"));
        } else {
            let mut meta = format!(
                "    {state} — bytes {}..{} of {}",
                self.from, self.to, self.produced
            );
            if self.dropped > 0 {
                meta.push_str(&format!(
                    " ({} earlier byte{} gone off the front)",
                    self.dropped,
                    if self.dropped == 1 { "" } else { "s" }
                ));
            }
            out.push(pane::faint(meta));
        }
        out.push(Line::default());
        let footer = 1;
        let visible = room.saturating_sub(out.len() + footer).max(1);
        if self.lines.is_empty() && !self.loading {
            // **§11.6 — an empty window is several cases, and one was an inversion of the
            // operator's own rule.** A job whose scope could not be joined *never ran*, so its
            // window is empty because there is no process behind it, and the card said
            //
            //     not run (could not join its scope)      ← the header
            //     it wrote nothing at all.                ← and it never started
            //
            // which is R17 read backwards: *a row with no output must not look like a row
            // whose output is empty*. The case is chosen by the **state** — the daemon's
            // `never_ran` — and not by the window's emptiness alone.
            //
            // **A redirected job is another case.** Its window is empty BY CONSTRUCTION (R41):
            // the daemon gave the bytes to a file, so `it wrote nothing at all` describes a job
            // that wrote a build log. The operator: *"entering a job never shows me its output
            // - whether it went to file or not"*. The name of the file is all this pane can
            // honestly say, and it is said where the reader is looking.
            //
            // `running` is the daemon's own state word, tested literally: the widget renders
            // the daemon's vocabulary and keeps no second copy of its enum.
            let said = if self.never_ran {
                // Written in full so two heads cannot hold two different sentences about one
                // state: *A rules the words; both heads render the same string*.
                "    it never ran, so there is nothing it could have written.".to_string()
            } else if let Some(path) = self.redirect.as_deref() {
                let path = clean_line(path);
                if state == "running" {
                    format!("    it is running and writing to {path} — not to this window.")
                } else {
                    format!("    it wrote nothing HERE — its output went to {path}.")
                }
            } else if state == "running" {
                "    it is running and has written nothing yet.".to_string()
            } else {
                "    it wrote nothing at all.".to_string()
            };
            out.push(pane::faint(said));
        }
        let scroll = self.scroll.min(self.lines.len().saturating_sub(visible));
        let end = self.lines.len() - scroll;
        let start = end.saturating_sub(visible);
        for l in &self.lines[start..end] {
            out.push(Line::raw(clean_line(l)));
        }
        while out.len() < room.saturating_sub(footer) {
            out.push(Line::default());
        }
        let hint = match (self.has_next, self.has_back) {
            (true, true) => "arrows scroll · → next page · ← back · Esc to jobs",
            (true, false) => "arrows scroll · → next page · Esc to jobs",
            (false, true) => "arrows scroll · ← back · Esc to jobs",
            (false, false) => "arrows scroll · Esc to jobs",
        };
        out.push(pane::faint(format!("    {hint}")));
        out.truncate(room);
        (out, scroll)
    }
}

impl crate::render::Widget for JobOutput {
    fn render(&self, area: crate::render::Rect, buf: &mut crate::render::Buffer) {
        crate::agent::draw_lines(&self.lines(area.height as usize).0, area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{drawn, plain, role_of};

    fn job(id: &str, command: &str, running: bool) -> JobRow {
        JobRow {
            id: id.into(),
            command: command.into(),
            how: "asked".into(),
            state: if running { "running" } else { "exited 0" }.into(),
            running,
            elapsed_ms: 1_200,
            ..JobRow::default()
        }
    }

    fn text(p: &JobsPane, w: usize) -> String {
        plain(&p.content(w).lines).join("\n")
    }

    /// letibot `app/tests/jobs.rs::the_jobs_pane_joins_the_command_and_marks_a_running_job`.
    #[test]
    fn the_jobs_pane_joins_the_command_and_marks_a_running_job() {
        let p = JobsPane {
            jobs: vec![
                job("j1", "cargo test --workspace", true),
                job("j2", "sleep 30", false),
            ],
            // **Both jobs, so the settled one is unfolded to be drawn** — it lives under the
            // `finished` group by default, and this test is about the ROW.
            finished_open: true,
            selected: 0,
        };
        let lines = text(&p, 100);
        assert!(lines.contains("cargo test --workspace"), "{lines}");
        assert!(
            lines.contains("[~]"),
            "a running job is marked running: {lines}"
        );
        assert!(
            lines.contains("[x]"),
            "a clean exit is marked done: {lines}"
        );
        // The marks carry the state's register too.
        let c = p.content(100);
        assert_eq!(
            role_of(&c.lines[c.stop_rows[0]], "[~]"),
            Some(Role::Pending)
        );
        assert_eq!(
            role_of(&c.lines[c.stop_rows[2]], "[x]"),
            Some(Role::Success)
        );
    }

    /// letibot `app/tests/jobs.rs::the_jobs_pane_names_the_file_a_redirected_job_writes_to`.
    #[test]
    fn the_jobs_pane_names_the_file_a_redirected_job_writes_to() {
        let mut j = job("j1", "cargo test > /tmp/build.log 2>&1", true);
        j.redirect = Some("/tmp/build.log".into());
        j.elapsed_ms = 4_000;
        let lines = text(
            &JobsPane {
                jobs: vec![j],
                ..JobsPane::default()
            },
            110,
        );
        assert!(
            lines.contains("\u{2192} /tmp/build.log"),
            "the pane must name the file the job writes to:\n{lines}"
        );
        assert!(
            lines.contains("not in the window"),
            "and say why Entering it shows nothing:\n{lines}"
        );
    }

    /// letibot `app/tests/jobs.rs::the_top_edge_counts_running_jobs_and_says_which_cannot_be_watched`
    /// (the count; where the edge puts it is the composer's).
    #[test]
    fn the_top_edge_counts_running_jobs_and_says_which_cannot_be_watched() {
        let pane = |jobs: Vec<JobRow>| JobsPane {
            jobs,
            ..JobsPane::default()
        };
        let redirected = |id: &str, running: bool| JobRow {
            redirect: Some("/tmp/build.log".into()),
            ..job(id, "cargo build", running)
        };
        // Zero jobs is no line at all — a count that is always there is furniture.
        assert_eq!(pane(vec![job("j1", "x", false)]).running_line(), None);
        assert_eq!(
            pane(vec![job("j1", "x", true)]).running_line().as_deref(),
            Some("1 job running")
        );
        assert_eq!(
            pane(vec![job("j1", "x", true), job("j2", "x", true)])
                .running_line()
                .as_deref(),
            Some("2 jobs running")
        );
        assert_eq!(
            pane(vec![redirected("j1", true), job("j2", "x", true)])
                .running_line()
                .as_deref(),
            Some("2 jobs running · 1 to a file"),
            "the unwatchable one is named before the reader opens the pane"
        );
        // A redirected job that has SETTLED is not counted.
        assert_eq!(
            pane(vec![job("j1", "x", true), redirected("j2", false)])
                .running_line()
                .as_deref(),
            Some("1 job running")
        );
    }

    /// The `finished` group: folded by default, a stop of its own, and the cursor's row is
    /// the one recorded — letibot's `jobs_row_of` reading `jobs_stop_rows`.
    #[test]
    fn settled_jobs_fold_under_one_group_row_that_is_a_stop() {
        let mut p = JobsPane {
            jobs: vec![job("j1", "cargo build", true), job("j2", "sleep 30", false)],
            finished_open: false,
            selected: 1,
        };
        assert_eq!(p.stops(), vec![JobStop::Job(0), JobStop::Finished]);
        let c = p.content(100);
        let rows = plain(&c.lines);
        let group = &rows[c.row_of(1)];
        assert_eq!(group, "▸ [+] finished (1)");
        assert!(!rows.iter().any(|l| l.contains("sleep 30")), "{rows:?}");
        assert!(
            c.lines[c.row_of(1)]
                .style
                .attrs
                .contains(crate::style::Attrs::REVERSE),
            "the picked row is inverse"
        );
        p.finished_open = true;
        let rows = plain(&p.content(100).lines);
        assert!(rows.iter().any(|l| l.contains("[-] finished (1)")));
        assert!(rows.iter().any(|l| l.contains("sleep 30")));
        assert!(
            rows.iter()
                .any(|l| l.ends_with("asked · exited 0 · 0 B out · ran 1.2s"))
        );
    }

    /// **A job that never ran has no duration** (A.2, §11.6).
    #[test]
    fn a_job_that_never_ran_claims_no_duration() {
        let p = JobsPane {
            jobs: vec![JobRow {
                state: "not run (could not join its scope)".into(),
                never_ran: true,
                ..job("j3", "cargo build", false)
            }],
            finished_open: true,
            selected: 0,
        };
        let lines = text(&p, 100);
        assert!(
            lines.contains("not run (could not join its scope) · 0 B out"),
            "{lines}"
        );
        assert!(!lines.contains("ran 0.0s"), "{lines}");
    }

    #[test]
    fn an_empty_table_says_how_a_job_gets_here() {
        let lines = text(&JobsPane::default(), 120);
        assert!(lines.starts_with("background jobs"), "{lines}");
        assert!(
            lines.contains("none. The model backgrounds a command"),
            "{lines}"
        );
    }

    fn out(state: &str, never_ran: bool) -> JobOutput {
        JobOutput {
            job: "j3".into(),
            state: state.into(),
            never_ran,
            ..JobOutput::default()
        }
    }

    /// letibot `app/tests/jobs.rs::a_job_that_never_ran_does_not_read_as_one_that_wrote_nothing`.
    #[test]
    fn a_job_that_never_ran_does_not_read_as_one_that_wrote_nothing() {
        let cases: [(&str, bool, &str); 6] = [
            (
                "running",
                false,
                "it is running and has written nothing yet.",
            ),
            ("exited 0", false, "it wrote nothing at all."),
            ("exited 1", false, "it wrote nothing at all."),
            ("signalled 9", false, "it wrote nothing at all."),
            ("killed by job_kill", false, "it wrote nothing at all."),
            (
                "not run (could not join its scope)",
                true,
                "it never ran, so there is nothing it could have written.",
            ),
        ];
        for (state, never_ran, want) in cases {
            let drawn = drawn(&out(state, never_ran), 100, 24).join("\n");
            assert!(drawn.contains(want), "{state}: want {want:?} in\n{drawn}");
            if never_ran {
                assert!(!drawn.contains("wrote nothing"), "{state}:\n{drawn}");
            }
        }
    }

    /// letibot `app/tests/jobs.rs::the_job_output_pane_names_the_file_a_redirected_job_wrote_to`.
    #[test]
    fn the_job_output_pane_names_the_file_a_redirected_job_wrote_to() {
        let v = JobOutput {
            redirect: Some("/tmp/build.log".into()),
            ..out("exited 0", false)
        };
        let screen = drawn(&v, 100, 24).join("\n");
        assert!(screen.contains("/tmp/build.log"), "{screen}");
        assert!(
            !screen.contains("it wrote nothing at all."),
            "a job that wrote a build log is described as having written nothing:\n{screen}"
        );
    }

    /// letibot `app/tests/jobs.rs::the_job_output_event_fills_the_overlay_and_it_pages` (the
    /// drawing half: the offsets as sent, and the footer naming the keys that work).
    #[test]
    fn the_job_output_draws_the_offsets_it_was_sent() {
        let v = JobOutput {
            from: 0,
            to: 10,
            produced: 30,
            lines: vec!["line one".into(), "line two".into()],
            has_next: true,
            ..out("exited 0", false)
        };
        let rows = drawn(&v, 100, 24);
        let screen = rows.join("\n");
        assert!(screen.contains("job output — j3"), "{screen}");
        assert!(
            screen.contains("exited 0 — bytes 0..10 of 30"),
            "the pane draws the offsets it was sent, not a parsed sentence: {screen}"
        );
        assert!(screen.contains("line one\nline two"), "{screen}");
        assert_eq!(
            rows[23].trim(),
            "arrows scroll · → next page · Esc to jobs",
            "the footer is pinned to the bottom row"
        );
    }

    /// The tail shows by default, and scrolling back stops at the window's beginning.
    #[test]
    fn the_output_shows_its_tail_and_scrolls_back_to_its_beginning() {
        let v = JobOutput {
            lines: (0..50).map(|i| format!("line {i}")).collect(),
            ..out("running", false)
        };
        let (rows, _) = v.lines(12);
        let t = plain(&rows).join("\n");
        assert!(t.contains("line 49") && !t.contains("line 0\n"), "{t}");
        let (rows, scroll) = JobOutput { scroll: 999, ..v }.lines(12);
        assert_eq!(scroll, 50 - 8, "clamped at the beginning");
        assert!(plain(&rows).iter().any(|l| l == "line 0"));
    }

    /// letibot `app/tests/jobs.rs::a_refused_job_output_read_lands_in_the_pane`.
    #[test]
    fn a_refused_job_output_read_lands_in_the_pane() {
        let v = JobOutput {
            error: Some("no job `j4` here; `/job` with no argument lists them".into()),
            ..out("", false)
        };
        let screen = drawn(&v, 100, 24).join("\n");
        assert!(screen.contains("the daemon refused this read"), "{screen}");
        assert!(screen.contains("no job `j4` here"), "{screen}");
    }

    #[test]
    fn a_dropped_front_is_said_beside_the_offsets() {
        let v = JobOutput {
            from: 100,
            to: 200,
            produced: 200,
            dropped: 1,
            lines: vec!["x".into()],
            ..out("running", false)
        };
        let t = plain(&v.lines(10).0).join("\n");
        assert!(
            t.contains("running — bytes 100..200 of 200 (1 earlier byte gone off the front)"),
            "{t}"
        );
        let loading = JobOutput {
            loading: true,
            ..out("", false)
        };
        assert!(
            plain(&loading.lines(10).0)
                .iter()
                .any(|l| l.trim() == "reading…")
        );
    }
}
