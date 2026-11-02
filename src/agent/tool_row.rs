//! A tool's result row in the transcript: the header, the diff or the payload's window, the decision
//! that gated it, and the picture it returned.

use crate::render::text::truncate_owned;
use crate::render::{Line, Span, Style};
use crate::style::Role;
use crate::term::links::file_url;

use super::card::{EditDiff, Verb, indent};
use super::decision::{SettledDecision, decision_lines};
use super::outcome::Outcome;
use super::text::{
    clean, clean_line, duration, first_sentence, is_envelope, one, shorten_subject, step_in,
    strip_gutter, visible_width, wrap,
};
use super::{Fold, lines_widget};

/// One settled tool call's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRow {
    /// The tool's name: `bash`, `read`, `edit`.
    pub name: String,
    pub call_id: String,
    /// The display target the call was proposed with — the path, the pattern, the command. Empty
    /// draws `(call_id)` rather than borrowing a neighbour's path, which would read as a fact.
    pub target: String,
    pub outcome: Outcome,
    /// The payload, one [`Line`] per line of it, as the tool wrote it, envelope markers dropped.
    pub payload: Vec<Line>,
    /// How long it took, when the host watched it run. `None` for a row read out of a snapshot.
    pub elapsed_ms: Option<u64>,
    /// The person at the keyboard ran this (`! ls`), not the model: drawn as the `▌` bar in
    /// [`Role::UserAccent`].
    pub operator: bool,
    /// The before/after view, when the host holds both sides of an edit.
    pub diff: Option<EditDiff>,
    /// The decision this call was gated by.
    pub decision: Option<SettledDecision>,
    /// The picture the tool returned, where the terminal can draw one.
    pub picture: Vec<Line>,
    /// The directory a relative target is resolved against, when the terminal speaks OSC 8:
    /// the subject of a read/edit/write/list is then a link to the file it names.
    pub link_root: Option<String>,
    /// The conversation's tool fold (`/t`).
    pub fold: Fold,
    /// Some(page) when this row's payload window is open (`ctrl-v`), at that offset: the host owns
    /// the offset and clamps it with [`ToolRow::layout`]'s furthest useful one.
    pub window: Option<usize>,
    /// Rows the window may take on this screen, so an open window fits.
    pub window_rows: usize,
    /// The body budget an unfolded row may draw.
    pub body_lines: usize,
    /// Whether this is the newest long result — the one `ctrl-v` reaches.
    pub newest: bool,
    /// Columns the whole row is stepped in by: a turn's activity indent.
    pub indent: usize,
}

/// What [`ToolRow::layout`] drew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowLayout {
    pub lines: Vec<Line>,
    /// With the window open: the furthest offset worth paging to, so a host clamping its stored
    /// offset makes the first Up after paging past the end move at once.
    pub max_page: Option<usize>,
}

/// A payload's text as plain lines: one per line, foreign text cleaned — which also removes a
/// coloured command's SGR, so a host that wants those colours maps them to spans itself.
pub fn payload_lines(text: &str) -> Vec<Line> {
    text.lines().map(|l| Line::raw(clean_line(l))).collect()
}

/// A size that is worth a fold: the line count turns [`Role::Strong`] from here.
const BIG: usize = 40;

impl ToolRow {
    /// A row with the defaults the rest of the fields usually take: folded, no window, a 40-row body
    /// budget.
    pub fn new(name: &str, call_id: &str, outcome: Outcome, payload: Vec<Line>) -> ToolRow {
        ToolRow {
            name: name.into(),
            call_id: call_id.into(),
            target: String::new(),
            outcome,
            payload,
            elapsed_ms: None,
            operator: false,
            diff: None,
            decision: None,
            picture: Vec::new(),
            link_root: None,
            fold: Fold::Folded,
            window: None,
            window_rows: 40,
            body_lines: 40,
            newest: false,
            indent: 0,
        }
    }

    pub fn lines(&self, width: usize) -> Vec<Line> {
        self.layout(width).lines
    }

    pub fn layout(&self, width: usize) -> RowLayout {
        let ind = self.indent;
        let p = &self.picture;
        let picture = || p.iter().cloned().map(|l| indent(l, 2));
        // The envelope markers are addressed to the model, not to the operator: a marker with a
        // per-call nonce, which on screen is a line of noise in a folded row. The bytes are the
        // command's too, so the text is cleaned before the fold is counted.
        let texts: Vec<String> = self
            .payload
            .iter()
            .map(|l| clean_line(&l.plain()))
            .collect();
        let kept: Vec<(&Line, &str)> = self
            .payload
            .iter()
            .zip(texts.iter())
            .filter(|(_, t)| !is_envelope(t))
            .map(|(l, t)| (l, t.as_str()))
            .collect();
        let lines: Vec<&str> = kept.iter().map(|(_, t)| *t).collect();
        let bad = !matches!(self.outcome, Outcome::Ok);
        let mark = if self.fold.is_open() { "▾" } else { "▸" };
        // The verb and the target are the two words a person scans a settled call for; the id takes
        // their place only when the target is not known.
        let verb_kind = Verb::of(&self.name);
        let verb = verb_kind.label(false).to_string();
        let full_subject = if self.target.is_empty() {
            format!("({})", clean_line(&self.call_id))
        } else {
            clean_line(&self.target)
        };
        let took = match self.elapsed_ms {
            Some(ms) => format!(" · {}", duration(ms)),
            None => String::new(),
        };
        // The header is built out of roles so the scan works without reading: the subject plain (it
        // is what a person looks for), everything structural faint, `ok` faint because it is the
        // boring case, and the line count strong once the output is big enough to fold — an
        // attribute rather than a second colour, so it survives a terminal-native theme.
        let w = width.saturating_sub(ind).max(20);
        let outcome_role = self.outcome.row_role();
        let size_role = if lines.len() >= BIG {
            Role::Strong
        } else {
            Role::Faint
        };
        // It degrades by shortening the subject, never by losing the tail: left to right and trimmed
        // at the right, a long target ate the outcome, and `not run` cut off the end is a failed call
        // wearing the shape of a successful one.
        let word = self.outcome.word();
        let tail_cols =
            3 + visible_width(word) + visible_width(&took) + 3 + 6 + lines.len().to_string().len();
        // A call the person ran says so, in the glyph and role the operator's own words already
        // wear — the palette has one meaning for the person at the keyboard — and in a glyph as well
        // as a colour, so the provenance survives `Palette::None`. Nothing else changes.
        let provenance = if self.operator { "▌ " } else { "" };
        let lead = format!("{provenance}{mark} {verb} ");
        let subject = shorten_subject(
            &full_subject,
            w.saturating_sub(visible_width(&lead) + tail_cols).max(8),
        );
        let mut head = Line::default();
        if self.operator {
            head.spans.push(Span::role("▌", Role::UserAccent));
            head.spans.push(Span::raw(" "));
        }
        head.spans.push(Span::role(mark, outcome_role));
        head.spans
            .push(Span::role(format!(" {verb} "), Role::Faint));
        // The file as a link (OSC 8) where the terminal speaks it: the full target the shortened
        // subject stands for.
        let mut subject_style = Style::new();
        if let Some(root) = &self.link_root
            && verb_kind.names_a_file()
            && let Some(url) = file_url(root, &self.target)
        {
            subject_style = subject_style.link(url);
        }
        head.spans.push(Span::styled(subject, subject_style));
        head.spans
            .push(Span::role(format!(" · {word}"), outcome_role));
        // Pushed even when empty: a host printing strings writes the faint run's open and close
        // around nothing.
        head.spans.push(Span::role(took.clone(), Role::Faint));

        // A result of one line goes on the header, where a second row would carry a count for a fold
        // with nothing to fold — at 34 rows that halving is four calls fitting or eight. An edit with
        // an excerpt is excluded: its diff is the content.
        let inline = (!bad
            && lines.len() == 1
            // An edit with an excerpt draws its diff, not the tool's prose — the rule the folded arm
            // below follows.
            && self.diff.is_none())
        .then(|| strip_gutter(lines[0]))
        .filter(|l| !l.is_empty())
        .filter(|l| head.width() + 3 + visible_width(l) <= w);
        if let Some(l) = inline {
            head.spans.push(Span::role(" · ", Role::Faint));
            head.spans.push(Span::raw(l));
            let mut out = vec![truncate_owned(head, w)];
            // The approval rides the one-line form too.
            if let Some(d) = &self.decision {
                out.extend(decision_lines(d, self.fold, w));
            }
            out.extend(picture());
            return RowLayout {
                lines: step_in(out, ind),
                max_page: None,
            };
        }

        head.spans.push(Span::role(
            format!(
                " · {} line{}",
                lines.len(),
                if lines.len() == 1 { "" } else { "s" }
            ),
            size_role,
        ));
        // No chord on the header: it belongs on the elision row below, which exists exactly when
        // something is hidden.
        let mut out = vec![truncate_owned(head, w)];
        // The reason, on its own wrapping line, in the outcome's own role and never truncated. A
        // reason that is a document — layer A names every construct it could not resolve, addressed to
        // the model — stands unfolded only as its first sentence, and the rest arrives with the fold.
        let why = self.outcome.reason().map(|r| clean(r).into_owned());
        let mut why_folded = false;
        if let Some(why) = &why {
            // The payload says it too, so this stays a gist: a refusal's payload is a complete
            // explanation, and printing the whole `why` above it puts the same paragraph twice in one
            // card. When the reason is not below, unfolding shows all of it.
            let first = why.lines().next().unwrap_or("").trim();
            let echoed = first.len() >= 40 && lines.iter().any(|l| l.contains(first));
            let shown: String = if self.fold.is_open() && !echoed {
                why.clone()
            } else {
                let gist = first_sentence(why);
                why_folded = gist.len() < why.len();
                gist.into_owned()
            };
            out.extend(
                wrap(&shown, w.saturating_sub(2))
                    .into_iter()
                    .map(|l| one(format!("  {l}"), outcome_role)),
            );
        }
        // The decision it was gated by, carried across with the card: without it the approval leaves
        // the screen the moment the result row takes the call over.
        if let Some(d) = &self.decision {
            out.extend(decision_lines(d, self.fold, w));
        }
        // A file edit draws its diff, not the tool's prose: the payload is addressed to the model,
        // and the operator's question about an edit is what changed. Folded keeps the first hunk's
        // rows so the change is on screen; a row whose host did not hold both sides keeps the prose.
        if let Some(d) = &self.diff
            && verb_kind.is_an_edit()
            && !bad
        {
            // The full target, not the shortened subject: `…/a.rs` names the file too.
            let rows = d.rows_under(&self.target);
            let keep = if self.fold.is_open() {
                rows.len()
            } else {
                8.min(rows.len())
            };
            let hidden = rows.len() - keep;
            out.extend(rows.into_iter().take(keep).map(|l| indent(l, 2)));
            if hidden > 0 {
                out.push(one(
                    format!("  … +{hidden} diff rows · /t unfolds it"),
                    Role::Faint,
                ));
            }
            return RowLayout {
                lines: step_in(out, ind),
                max_page: None,
            };
        }
        // The payload's own window is what makes the rest of it reachable: drawn from the head, a
        // 418 KB log's fold reported `+N lines` and the chord revealed nothing, because opening the
        // fold changed the budget and not the offset.
        let window = self.window.is_some();
        let total = lines.len();
        // The window is the row's own length, not the fold's, so one result can be read to its end
        // without unfolding every result — and it fits the screen, where a forty-row window's first
        // lines sat above the top of a 24-row terminal.
        let shown_rows = if window {
            self.body_lines.min(self.window_rows).max(4)
        } else if self.fold.is_open() || (bad && why.is_none()) {
            self.body_lines
        } else {
            2
        };
        // The last page is a full one: clamping to `total - 1` left the end of a long output one line
        // under a seam.
        let max_page = total.saturating_sub(shown_rows.saturating_sub(2).max(1));
        let page = match self.window {
            Some(p) => p.min(max_page),
            None => 0,
        };
        // One row is spent on the seam when there is more payload, on either side.
        let above = page > 0;
        let body = shown_rows
            .saturating_sub(1)
            .saturating_sub(usize::from(above))
            .max(1);
        let end = (page + body).min(total);
        let below = end < total;
        if above {
            out.push(one(
                format!("  ↑ {page} more lines above · ↑ scrolls up"),
                Role::Faint,
            ));
        }
        // The payload's block is faint, and a coloured run inside it stacks its own role over that
        // and closes back to faint.
        let faint = Style::of(Role::Faint);
        out.extend(kept[page..end].iter().map(|(l, _)| {
            let mut row = indent((*l).clone(), 2);
            row.style = faint.patch(&row.style);
            row
        }));
        if below {
            let hidden = total - end;
            // The seam where content was taken out, not a sentence, and it names which key does what
            // now: the chord opens the view and the arrows move inside it.
            out.push(one(
                if window {
                    format!("  … +{hidden} lines · ↓ pages down · esc closes")
                } else if self.newest {
                    // The chord, on the row it acts on.
                    format!("  … +{hidden} lines · ctrl-v opens it")
                } else {
                    // Not this chord: it acts on the newest long result and there is no cursor to
                    // point at an older one, so this row names the verb that does reach it.
                    format!("  … +{hidden} lines · /t unfolds it")
                },
                Role::Faint,
            ));
        } else if window {
            // The end of the payload, said so "no more" is not confused with "the arrow stopped
            // working".
            out.push(one("  … end of output · esc closes", Role::Faint));
        } else if why_folded {
            // The payload was short enough to show whole but the reason was cut, so the affordance
            // has to be here or the rest is hidden behind a chord nothing on the row mentions.
            out.push(one(
                "  … the rest of the reason · /t unfolds it",
                Role::Faint,
            ));
        }
        out.extend(picture());
        RowLayout {
            lines: step_in(out.into_iter().map(|l| truncate_owned(l, w)).collect(), ind),
            max_page: window.then_some(max_page),
        }
    }
}

lines_widget!(ToolRow);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{drawn, plain, role_of};
    use crate::render::TestBuffer;
    use crate::style::Palette;

    fn numbered(n: usize, word: &str) -> Vec<Line> {
        payload_lines(&(0..n).map(|i| format!("{word} {i}\n")).collect::<String>())
    }

    fn bang(operator: bool) -> ToolRow {
        let mut r = ToolRow::new("bash", "bang-1", Outcome::Ok, numbered(60, "line"));
        r.operator = operator;
        r.indent = 2;
        r.fold = Fold::Open;
        r
    }

    #[test]
    fn a_models_tool_row_is_unmarked_and_its_paint_has_not_moved() {
        let theirs = bang(false).lines(120);
        assert_eq!(theirs[0].plain(), "  ▾ Ran (bang-1) · ok · 60 lines");
        let h = &theirs[0];
        assert_eq!(role_of(h, "▾"), Some(Role::Faint));
        assert_eq!(role_of(h, " Ran "), Some(Role::Faint));
        assert_eq!(role_of(h, "(bang-1)"), Some(Role::Plain));
        assert_eq!(role_of(h, " · ok"), Some(Role::Faint));
        assert_eq!(role_of(h, " · 60 lines"), Some(Role::Strong));
        assert!(!h.plain().contains('▌'));
    }

    /// A row with no duration still writes the faint run where the duration would be.
    #[test]
    fn a_models_tool_row_header_is_the_bytes_it_has_always_been() {
        assert_eq!(
            bang(false).lines(120)[0].to_ansi(Palette::Colour),
            "  \u{1b}[2m▾\u{1b}[0m\u{1b}[2m Ran \u{1b}[0m(bang-1)\u{1b}[2m · ok\u{1b}[0m\
             \u{1b}[2m\u{1b}[0m\u{1b}[1m · 60 lines\u{1b}[0m"
        );
    }

    #[test]
    fn an_operators_tool_row_is_the_models_row_with_the_persons_own_mark_on_it() {
        let mine = bang(true).lines(120);
        let theirs = bang(false).lines(120);
        assert_eq!(
            mine[0].plain(),
            format!("  ▌ {}", &theirs[0].plain()[2..]),
            "the operator's header is not the model's with the mark in front of it"
        );
        assert_eq!(role_of(&mine[0], "▌"), Some(Role::UserAccent));
        // The registers are the model's, span for span, after the mark.
        let after: Vec<_> = mine[0].spans.iter().skip(3).cloned().collect();
        assert_eq!(after, theirs[0].spans[1..].to_vec());
        assert_eq!(
            mine[1..],
            theirs[1..],
            "the count, the fold and the body must be the model's"
        );
        // And with no palette at all: the provenance is a glyph, so it survives.
        let rows = TestBuffer::new(120, 3).draw(&bang(true)).plain();
        assert!(rows[0].contains('▌'), "{rows:?}");
        assert!(!rows.iter().any(|l| l.contains('\x1b')));
    }

    /// A settled call is one row, and the row is the one with the result on it.
    #[test]
    fn a_settled_call_is_one_row_and_the_row_is_the_one_with_the_result_on_it() {
        let mut r = ToolRow::new(
            "read",
            "call_0",
            Outcome::Ok,
            payload_lines("# rano TODO\n"),
        );
        r.target = "TODO.md".into();
        let out = plain(&r.lines(120));
        assert_eq!(out, vec!["▸ Read TODO.md · ok · # rano TODO"]);
    }

    /// The one-line form drops the gutter a `read` puts before the only line there is.
    #[test]
    fn a_one_line_result_goes_on_the_header_without_its_gutter() {
        let mut r = ToolRow::new("read", "c", Outcome::Ok, payload_lines("     1| /target\n"));
        r.target = ".gitignore".into();
        r.elapsed_ms = Some(1_100);
        let out = r.lines(120);
        assert_eq!(plain(&out), vec!["▸ Read .gitignore · ok · 1.1s · /target"]);
        assert_eq!(role_of(&out[0], "/target"), Some(Role::Plain));
    }

    #[test]
    fn a_subject_too_long_for_the_row_never_pushes_the_outcome_off_it() {
        let r = ToolRow::new(
            "ask_code",
            "call_0",
            Outcome::NotRun("no retrieval backend is attached to this session".into()),
            payload_lines("a\nb\nc\n"),
        );
        let screen = plain(&r.lines(60)).join("\n");
        assert!(screen.contains("not run"), "{screen}");
        assert!(
            screen.contains("no retrieval backend"),
            "the reason wraps in the body rather than being cut off a header:\n{screen}"
        );
        // And a long target gives way before the outcome does.
        let mut r = r.clone();
        r.target = format!(
            "\"{}\"",
            "Give an overview of the crate architecture ".repeat(3)
        );
        let head = r.lines(60)[0].plain();
        assert!(head.contains("· not run"), "{head}");
        assert!(r.lines(60)[0].width() <= 60);
    }

    #[test]
    fn a_result_whose_round_the_head_cannot_see_says_so_rather_than_borrowing() {
        let r = ToolRow::new("read", "call_0", Outcome::Ok, payload_lines("hello\n"));
        assert!(plain(&r.lines(120))[0].contains("(call_0)"));
    }

    #[test]
    fn a_big_result_weighs_more_than_a_small_one_and_costs_no_cube_colour() {
        let small = ToolRow::new("grep", "call_0", Outcome::Ok, numbered(3, "x"));
        let big = ToolRow::new("grep", "call_0", Outcome::Ok, numbered(236, "x"));
        assert_eq!(
            role_of(&big.lines(120)[0], "236 lines"),
            Some(Role::Strong),
            "a big result is strong"
        );
        assert_eq!(
            role_of(&small.lines(120)[0], "3 lines"),
            Some(Role::Faint),
            "a small one is not"
        );
        let rows = TestBuffer::new(120, 4).draw(&big).rows(Palette::Colour);
        for cube in ["38;5;", "48;5;", "38;2;"] {
            assert!(!rows.join("").contains(cube), "{cube}: {rows:?}");
        }
    }

    #[test]
    fn a_backgrounded_call_is_not_drawn_as_a_failure() {
        let r = ToolRow::new(
            "bash",
            "c",
            Outcome::Backgrounded("as `j4` after 0.4s — `/job j4 out` to read it".into()),
            payload_lines("started\nmore\n"),
        );
        let out = r.lines(120);
        assert_eq!(role_of(&out[0], " · backgrounded"), Some(Role::Faint));
        assert_eq!(role_of(&out[1], "as `j4`"), Some(Role::Faint));
        let f = ToolRow::new("bash", "c", Outcome::Failed("exit 1".into()), vec![]);
        assert_eq!(role_of(&f.lines(120)[0], " · failed"), Some(Role::Failure));
    }

    /// The seam names the key that reaches the rest on this row.
    #[test]
    fn the_seam_names_the_key_that_reaches_the_rest_on_this_row() {
        let mut r = ToolRow::new("bash", "c1", Outcome::Ok, numbered(40, "line"));
        r.target = "cargo test".into();
        r.newest = true;
        let folded = plain(&r.lines(100)).join("\n");
        assert!(folded.contains("… +39 lines · ctrl-v opens it"), "{folded}");
        r.newest = false;
        let older = plain(&r.lines(100)).join("\n");
        assert!(older.contains("… +39 lines · /t unfolds it"), "{older}");
        r.window = Some(0);
        let open = plain(&r.lines(100)).join("\n");
        assert!(
            open.contains("pages down"),
            "the window did not open: {open}"
        );
    }

    #[test]
    fn a_long_payload_can_be_paged_to_its_end() {
        let mut r = ToolRow::new("bash", "c1", Outcome::Ok, numbered(200, "line"));
        r.newest = true;
        let folded = plain(&r.lines(100)).join("\n");
        assert!(folded.contains("line 0"), "{folded}");
        assert!(folded.contains("ctrl-v opens it"), "{folded}");
        r.window = Some(0);
        let head = plain(&r.lines(100)).join("\n");
        assert!(head.contains("line 0"), "{head}");
        assert!(head.contains("pages down"), "{head}");
        r.window = Some(1);
        let paged = plain(&r.lines(100));
        assert!(
            !paged.iter().any(|l| l.trim() == "line 0"),
            "the window did not move: {paged:?}"
        );
        assert!(paged.join("\n").contains("more lines above"), "{paged:?}");
        // Paging past the end lands on the end, and says so.
        r.window = Some(10_000);
        let end = r.layout(100);
        assert!(
            plain(&end.lines).join("\n").contains("end of output"),
            "{:?}",
            plain(&end.lines)
        );
        assert_eq!(end.max_page, Some(200 - 38));
    }

    /// A window reaches its end, comes back at once, and fits the screen.
    #[test]
    fn an_open_result_window_reaches_its_end_comes_back_at_once_and_fits() {
        let mut r = ToolRow::new("bash", "c0", Outcome::Ok, numbered(300, "output line"));
        r.window_rows = 18;
        r.window = Some(0);
        let shown = |r: &ToolRow| {
            plain(&r.lines(80))
                .into_iter()
                .filter(|l| l.contains("output line"))
                .collect::<Vec<_>>()
        };
        let open = shown(&r);
        assert!(open.first().is_some_and(|l| l.ends_with("output line 0")));
        assert!(r.lines(80).len() <= 18 + 1, "the window fits its rows");
        let max = r.layout(80).max_page.unwrap();
        r.window = Some(max);
        let end = shown(&r);
        assert!(
            end.last().is_some_and(|l| l.ends_with("output line 299")),
            "{end:?}"
        );
        assert!(end.len() > 5, "the last page is one line: {end:?}");
        r.window = Some(max - 1);
        assert_ne!(shown(&r).last(), end.last(), "Up did not move");
    }

    /// The envelope's markers never show, and a document of a reason folds to its gist with the verb
    /// that has the rest.
    #[test]
    fn a_reason_that_is_a_document_folds_and_the_envelope_never_shows() {
        let reason = "this command's meaning does not exist yet, so nothing can decide about \
                      it. The grammar read 315 bytes and could not resolve:\n  \
                      parameter_expansion at 1:2 decides the assignment";
        let r = ToolRow::new(
            "bash",
            "c",
            Outcome::Denied(reason.into()),
            payload_lines("<<<TOOL_ERROR 5ebfdef6>>>\ndenied\n<<<END_TOOL_ERROR 5ebfdef6>>>\n"),
        );
        let out = plain(&r.lines(120));
        let joined = out.join("\n");
        assert!(!joined.contains("<<<"), "{joined}");
        assert!(
            joined.contains("· 1 line"),
            "the count is of readable lines: {joined}"
        );
        assert!(!joined.contains("parameter_expansion"), "{joined}");
        assert!(
            joined.contains("… the rest of the reason · /t unfolds it"),
            "{joined}"
        );
        let mut open = r.clone();
        open.fold = Fold::Open;
        let joined = plain(&open.lines(120)).join("\n");
        assert!(joined.contains("parameter_expansion"), "{joined}");
        assert_eq!(
            role_of(&r.lines(120)[1], "this command"),
            Some(Role::Attention)
        );
    }

    #[test]
    fn a_landed_edit_draws_its_diff_folded_to_its_first_rows() {
        let mut r = ToolRow::new(
            "edit",
            "c1",
            Outcome::Ok,
            payload_lines("a.rs: 1 replacement(s)\n"),
        );
        r.target = "a.rs".into();
        let mut rows = vec![Line::raw("a.rs")];
        rows.extend((0..12).map(|i| Line::raw(format!("{i} + x"))));
        r.diff = Some(EditDiff {
            path: "/w/a.rs".into(),
            rows,
            capped_at: None,
        });
        let out = plain(&r.lines(120));
        assert_eq!(out[0], "▸ Edited a.rs · ok · 1 line");
        assert_eq!(
            out[1], "  0 + x",
            "the name line went: the header names the file"
        );
        assert_eq!(out.len(), 1 + 8 + 1);
        assert_eq!(out[9], "  … +4 diff rows · /t unfolds it");
        assert!(!out.join("\n").contains("replacement"), "{out:?}");
        r.fold = Fold::Open;
        assert_eq!(r.lines(120).len(), 1 + 12);
    }

    #[test]
    fn a_read_names_its_file_as_a_link_and_a_command_does_not() {
        let mut r = ToolRow::new("read", "c", Outcome::Ok, numbered(3, "x"));
        r.target = "src/app.rs".into();
        r.link_root = Some("/w".into());
        let l = &r.lines(120)[0];
        let sp = l.spans.iter().find(|s| s.content == "src/app.rs").unwrap();
        let url = sp.style.link.as_deref().unwrap();
        assert!(
            url.starts_with("file://") && url.ends_with("/w/src/app.rs"),
            "{url}"
        );
        r.name = "bash".into();
        assert!(r.lines(120)[0].spans.iter().all(|s| s.style.link.is_none()));
    }

    #[test]
    fn the_gate_and_the_picture_ride_the_row() {
        let mut r = ToolRow::new("read", "c", Outcome::Ok, payload_lines("ok\n"));
        r.target = "shot.png".into();
        r.decision = Some(SettledDecision {
            verdict: super::super::decision::Verdict::Refused,
            by_kind: "policy".into(),
            by_identity: String::new(),
            summary: String::new(),
            basis: String::new(),
            advice: None,
        });
        r.picture = crate::term::graphics::image_lines(7, 4, 2);
        let out = r.lines(120);
        assert_eq!(out.len(), 1 + 1 + 2);
        assert_eq!(out[1].plain(), "  · refused, by policy");
        assert!(out[2].plain().starts_with("  \u{10EEEE}"));
    }

    #[test]
    fn nothing_on_the_row_is_wider_than_the_row() {
        let mut r = ToolRow::new(
            "bash",
            "c",
            Outcome::Failed("x ".repeat(200)),
            payload_lines(&"y".repeat(500)),
        );
        r.target = "z".repeat(300);
        r.indent = 2;
        for w in [20usize, 40, 80, 200] {
            for l in r.lines(w) {
                assert!(l.width() <= w.max(22), "{w}: {}", l.plain());
            }
        }
        // And drawn into a buffer it is the same rows.
        let rows = drawn(&r, 80, 6);
        assert!(rows[0].contains("· failed"), "{rows:?}");
    }

    #[test]
    fn a_payloads_escapes_are_never_its_text() {
        let r = ToolRow::new(
            "bash",
            "c",
            Outcome::Ok,
            payload_lines("\x1b[?1000l\x1b[31mred\x1b[0m\nplain\nthird\n"),
        );
        let rows = TestBuffer::new(80, 4).draw(&r).rows(Palette::Colour);
        assert!(!rows.join("").contains("?1000"), "{rows:?}");
        assert!(plain(&r.lines(80))[1].ends_with("red"));
        assert_eq!(role_of(&r.lines(80)[1], "red"), Some(Role::Faint));
    }
}
