//! The jobs pane: the background jobs a session started, and one job's output.
//!
//! Every field is the daemon's answer; the widget decides colour and layout and nothing else — no join
//! against the current turn, because the process table is not a thing a head reconstructs.

use crate::agent::pane::{self, PaneLines};
use crate::agent::text::{bytes_human, clean, clean_line};
use crate::agent::tree_list::{Action, Cell, Row, TreeList};
use crate::render::Line;
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
    /// The one enumeration of what the cursor can rest on — running jobs, then one `finished` group
    /// row, then (unfolded) the settled jobs. The arrows, Enter and the drawn `▸` all read it, so the
    /// row the cursor is drawn on is the row Enter takes.
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

    /// How many jobs are running, for the composer's top edge; `None` at zero, because a count that is
    /// always there is furniture. A job whose output goes to a file is counted apart, since its window
    /// will be empty however long it runs, and a settled job's redirect is not counted.
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

    /// The daemon's job table, as rows — running first, then one folded `finished (N)` row. Each stop's
    /// row is recorded in the result, which is what the arrows scroll to and a click hits.
    pub fn content(&self, w: usize) -> PaneLines {
        self.tree().content(w)
    }

    /// The hint row [`JobsPane::content`] draws — the selected row's own keys, then the pane's — so a
    /// host's own bar carries it rather than a second copy of the same words.
    pub fn hint_line(&self) -> Option<String> {
        self.tree().hint_line()
    }

    /// The same sentence without the pane's left margin, which is what a bar takes.
    pub fn hint_text(&self) -> Option<String> {
        self.tree().hint_text()
    }

    /// The pane, as data: the same enumeration the arrows read, one [`Row`] each. Its second line sits
    /// at 9 under a job and at 7 under the group, so the row says which column it uses.
    fn tree(&self) -> TreeList {
        TreeList {
            title: "background jobs".into(),
            rows: self.stops().into_iter().map(|s| self.row(s)).collect(),
            selected: Some(self.selected),
            empty: "none. The model backgrounds a command with bash's `background: true`; \
                    ctrl-o moves the running one."
                .into(),
            fold_column: false,
            detail_column: 9,
            margin: 4,
            hint: vec!["↑↓ moves".into(), "esc closes this".into()],
            // A job still shows running until the daemon says it settled, which is a sentence about the
            // table rather than about a row.
            notes: vec![
                "a job still shows running until the daemon says it settled — between turns, \
                 that saying is the daemon's alone."
                    .into(),
            ],
        }
    }

    /// One stop, as a row: a job, or the `finished` group.
    fn row(&self, stop: JobStop) -> Row {
        let i = match stop {
            // The `finished` fold: a group row and not a job — there is nothing to read, and Enter
            // folds or unfolds the settled ones.
            JobStop::Finished => {
                let n = self.jobs.iter().filter(|j| !j.running).count();
                let fold = if self.finished_open { "[-]" } else { "[+]" };
                return Row {
                    id: Some("finished".into()),
                    cells: vec![Cell::new(fold), Cell::new(format!("finished ({n})"))],
                    detail: vec![
                        if self.finished_open {
                            "the ones that have settled · enter folds them away"
                        } else {
                            "enter shows the ones that have settled"
                        }
                        .into(),
                    ],
                    detail_column: Some(7),
                    actions: vec![Action::new(
                        "enter",
                        if self.finished_open {
                            "folds them away"
                        } else {
                            "shows the ones that have settled"
                        },
                    )],
                    ..Row::default()
                };
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
        // A job that never ran has no duration, and the row must not claim one: the state word denies
        // `ran` two fields before the row would say it. The byte count stays — nothing was produced, and
        // that is a measurement.
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
        let mut detail = vec![format!("{} · {tail}", clean_line(&j.how))];
        // And where the output actually went, on a line of its own so the file is readable rather than
        // another clause on a long row — before the reader Enters and finds an empty window.
        if let Some(path) = &j.redirect {
            detail.push(format!(
                "→ {} (its output is there, not in the window)",
                clean_line(path)
            ));
        }
        Row {
            id: Some(j.id.clone()),
            cells: vec![
                Cell::role(mark, role),
                Cell::new(format!("{} {}", clean_line(&j.id), clean_line(&j.command))),
            ],
            detail,
            actions: vec![Action::new("enter", "reads it")],
            ..Row::default()
        }
    }
}

/// One job's output window, as the daemon sent it: offsets rather than a sentence.
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
    /// The view, exactly `room` rows or fewer, and the scroll actually used. The tail shows by default,
    /// the arrows walk back toward the beginning, and the footer says how much further the reader can go.
    pub fn lines(&self, room: usize) -> (Vec<Line>, usize) {
        let mut out = vec![pane::title(format!(
            "job output — {}",
            clean_line(&self.job)
        ))];
        // A refusal is not a window: the daemon could not answer, so the pane says what it said rather
        // than drawing an empty log the operator would read as "the job wrote nothing".
        if let Some(err) = &self.error {
            out.push(pane::faint("    the daemon refused this read:"));
            out.push(Line::default());
            out.extend(clean(err).lines().map(Line::raw));
            out.push(Line::default());
            out.push(pane::faint("    Esc back to jobs"));
            out.truncate(room);
            return (out, 0);
        }
        // The state and the measurement on one line, because they are one fact. A `dropped` count is
        // said here rather than in the footer: a window that begins mid-log must not read as the job's
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
            // An empty window is several cases, chosen by the daemon's `never_ran` and not by the
            // emptiness alone: a job whose scope could not be joined never ran, and a redirected job's
            // window is empty by construction because the bytes went to a file. `running` is the
            // daemon's own word, tested literally.
            let said = if self.never_ran {
                // Written in full so two heads cannot hold two different sentences about one state.
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

    #[test]
    fn the_jobs_pane_joins_the_command_and_marks_a_running_job() {
        let p = JobsPane {
            jobs: vec![
                job("j1", "cargo test --workspace", true),
                job("j2", "sleep 30", false),
            ],
            // Both jobs, so the settled one is unfolded to be drawn: it lives under the `finished`
            // group by default, and this test is about the row.
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

    /// The count of running jobs, and which of them cannot be watched.
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

    /// The `finished` group: folded by default, a stop of its own, and the cursor's row is the one
    /// recorded.
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

    /// The hint under the list is the selected row's own keys, and the sentence about the table sits
    /// under both as a note of its own.
    #[test]
    fn the_hint_is_the_selected_rows_own_keys_and_the_table_keeps_its_own_sentence() {
        let p = JobsPane {
            jobs: vec![job("j1", "cargo build", true), job("j2", "sleep 30", false)],
            finished_open: false,
            selected: 0,
        };
        let drawn = plain(&p.content(400).lines);
        let hint = |sel: usize| {
            plain(
                &JobsPane {
                    selected: sel,
                    ..p.clone()
                }
                .content(400)
                .lines,
            )
            .into_iter()
            .find(|l| l.contains("↑↓ moves"))
            .expect("a hint line")
        };
        assert_eq!(hint(0), "    enter reads it · ↑↓ moves · esc closes this");
        assert_eq!(
            hint(1),
            "    enter shows the ones that have settled · ↑↓ moves · esc closes this"
        );
        assert!(
            drawn
                .iter()
                .any(|l| l.contains("a job still shows running until the daemon says it settled")),
            "{drawn:#?}"
        );
    }

    /// The pane's hint leaves as data: a host's bar reads the sentence the pane draws.
    #[test]
    fn the_panes_hint_is_the_hint_it_draws() {
        let p = JobsPane {
            jobs: vec![job("j1", "cargo build", true), job("j2", "sleep 30", false)],
            finished_open: false,
            selected: 1,
        };
        let hint_row = plain(&p.content(400).lines)
            .into_iter()
            .find(|l| l.contains("↑↓ moves"))
            .expect("a hint row");
        assert_eq!(p.hint_line().as_deref(), Some(hint_row.as_str()));
        assert_eq!(
            p.hint_text().as_deref(),
            Some(hint_row.trim_start()),
            "the bar takes the words, not the pane's margin"
        );
        assert!(
            p.hint_text()
                .unwrap()
                .starts_with("enter shows the ones that have settled ·"),
            "{:?}",
            p.hint_text()
        );
    }

    /// The fixture the golden pin below draws: one running job, and two settled — one redirected.
    fn golden_fixture() -> JobsPane {
        let mut redirected = job("j3", "cargo build > /tmp/build.log 2>&1", false);
        redirected.redirect = Some("/tmp/build.log".into());
        redirected.elapsed_ms = 4_000;
        let mut running = job("j1", "cargo test --workspace", true);
        running.produced = 1024;
        JobsPane {
            jobs: vec![running, job("j2", "sleep 30", false), redirected],
            finished_open: false,
            selected: 0,
        }
    }

    /// The body the pane drew before the tree-list port, pinned first: the frame's rows up to the last
    /// blank. The hint under it now follows the cursor.
    const GOLDEN_BODY: &[&str] = &[
        "background jobs",
        "",
        "▸ [~] j1 cargo test --workspace",
        "         asked · running · 1.0 KB out so far",
        "  [+] finished (2)",
        "       enter shows the ones that have settled",
    ];

    #[test]
    fn the_body_the_pane_drew_before_the_port_is_the_body_it_draws_after_it() {
        let drawn = plain(&golden_fixture().content(100).lines);
        let end = drawn
            .iter()
            .rposition(|l| l.is_empty())
            .expect("the frame's blank row before the hint");
        let body: Vec<&str> = drawn[..end].iter().map(String::as_str).collect();
        assert_eq!(body, GOLDEN_BODY);
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

    /// The drawing half: the offsets as sent, and the footer naming the keys that work.
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
