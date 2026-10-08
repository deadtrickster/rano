//! Finding a command: `M-x` (the palette), the help pages, `describe-key`, and
//! the cards a prefix key shows.
//!
//! Everything here reads the command table and the keymaps
//! (`commands.rs`); none of it lists a key of its own.
//!
//! - **`M-x`** runs any command by name. Type words in any order — each must
//!   match the title, the name or the doc — and the best matches come first,
//!   recently run ones ahead; every row shows the command's key, so the palette
//!   teaches the keys it saves you from needing.
//! - **Help pages** (`C-g ?`, `C-g b`): every key in effect, as cards grouped
//!   by what they do.
//! - **`describe-key`** (`C-g k`): press a key, read what it runs.

use crate::keymap::{Key, Keymap, seq_emacs, seq_nano, where_is};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::commands::{Group, command, commands};
use crate::editor::Editor;

/// How keys are written to the reader: emacs's `C-x M-p`, or nano's `^X M-P`.
pub fn notation(nano: bool, seq: &[Key]) -> String {
    if nano { seq_nano(seq) } else { seq_emacs(seq) }
}

// ---------- M-x ----------

pub struct Palette {
    pub query: String,
    /// Matching command names, best first.
    pub items: Vec<&'static str>,
    pub sel: usize,
}

/// How well `terms` match a command, or `None` when one of them does not: a
/// title starting with the term counts most, then a word of the title or name
/// starting with it, then containing it anywhere, and the doc least.
fn score(name: &str, title: &str, doc: &str, terms: &[String]) -> Option<u32> {
    let title = title.to_lowercase();
    let doc = doc.to_lowercase();
    let mut s = 0;
    for t in terms {
        s += if title.starts_with(t.as_str()) {
            30
        } else if title.split(' ').any(|w| w.starts_with(t.as_str()))
            || name.split('-').any(|w| w.starts_with(t.as_str()))
        {
            20
        } else if title.contains(t.as_str()) || name.contains(t.as_str()) {
            10
        } else if doc.contains(t.as_str()) {
            1
        } else {
            return None;
        };
    }
    Some(s)
}

impl Editor {
    pub(crate) fn open_palette(&mut self) {
        let mut p = Palette {
            query: String::new(),
            items: Vec::new(),
            sel: 0,
        };
        self.palette_filter(&mut p);
        self.palette = Some(p);
    }

    fn palette_filter(&self, p: &mut Palette) {
        let terms: Vec<String> = p
            .query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let recent = |n: &str| {
            self.command_history
                .iter()
                .position(|h| *h == n)
                .unwrap_or(usize::MAX)
        };
        let mut hits: Vec<(u32, usize, &'static str, &'static str)> = commands()
            .iter()
            .filter(|c| c.group.global())
            .filter_map(|c| {
                score(c.name, c.title, c.doc, &terms).map(|s| (s, recent(c.name), c.title, c.name))
            })
            .collect();
        // Best match, then most recently run, then by title.
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)));
        p.items = hits.into_iter().map(|h| h.3).collect();
        p.sel = 0;
    }

    pub(crate) fn palette_type(&mut self, c: char) {
        if let Some(mut p) = self.palette.take() {
            p.query.push(c);
            self.palette_filter(&mut p);
            self.palette = Some(p);
        }
    }

    pub(crate) fn palette_backspace(&mut self) {
        if let Some(mut p) = self.palette.take() {
            p.query.pop();
            self.palette_filter(&mut p);
            self.palette = Some(p);
        }
    }

    pub(crate) fn palette_move(&mut self, d: isize) {
        if let Some(p) = self.palette.as_mut() {
            let last = p.items.len().saturating_sub(1) as isize;
            p.sel = (p.sel as isize + d).clamp(0, last.max(0)) as usize;
        }
    }

    pub(crate) fn palette_run(&mut self) {
        let Some(p) = self.palette.take() else {
            return;
        };
        match p.items.get(p.sel) {
            Some(name) => self.run_command(name),
            None => self.flash(&format!("No command matches {:?}", p.query)),
        }
    }

    /// The keys that reach `name` in the global layer, written for the reader;
    /// empty when none do (it is `M-x` only).
    pub(crate) fn keys_for(&self, name: &str) -> String {
        where_is(&[&self.keymaps.global], name)
            .iter()
            .map(|s| notation(self.config.nano_keys, s))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// ---------- help pages ----------

/// What a help page shows; re-rendered when the width changes.
pub enum PageKind {
    /// How rano works, then every key.
    Overview,
    /// Every key in effect.
    Bindings,
    /// Fixed lines (a key's description).
    Text(Vec<Line<'static>>),
}

pub struct InfoView {
    pub title: String,
    pub kind: PageKind,
    pub lines: Vec<Line<'static>>,
    pub top: usize,
    width: usize,
}

impl Editor {
    fn open_page(&mut self, title: &str, kind: PageKind) {
        self.completion_close();
        let mut v = InfoView {
            title: title.to_string(),
            kind,
            lines: Vec::new(),
            top: 0,
            width: 0,
        };
        self.render_page(&mut v, self.text_w);
        self.info = Some(v);
    }

    fn render_page(&self, v: &mut InfoView, width: usize) {
        v.lines = match &v.kind {
            PageKind::Overview => {
                let mut l = overview_intro(self);
                l.extend(self.binding_cards(width));
                l
            }
            PageKind::Bindings => self.binding_cards(width),
            PageKind::Text(t) => t.clone(),
        };
        v.width = width;
    }

    /// Re-render a help page after a resize. Returns whether it changed.
    pub(crate) fn refresh_info_view(&mut self) -> bool {
        let w = self.text_w;
        let Some(mut v) = self.info.take() else {
            return false;
        };
        let changed = v.width != w;
        if changed {
            self.render_page(&mut v, w);
        }
        self.info = Some(v);
        changed
    }

    pub(crate) fn open_help_overview(&mut self) {
        self.open_page("Help", PageKind::Overview);
    }

    pub(crate) fn open_describe_bindings(&mut self) {
        self.open_page("All keys", PageKind::Bindings);
    }

    pub(crate) fn start_describe_key(&mut self) {
        self.pending.describe = true;
        self.flash("Describe key: press a key");
    }

    /// What `seq` does, on a page of its own.
    pub(crate) fn show_description(&mut self, seq: &[Key], name: Option<&str>) {
        let keys = notation(self.config.nano_keys, seq);
        let strong = Style::new().add_modifier(Modifier::BOLD);
        let faint = Style::new().add_modifier(Modifier::DIM);
        let mut lines = vec![Line::default()];
        match name.and_then(command) {
            Some(c) => {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {keys}"), strong.fg(Color::Cyan)),
                    Span::raw(" runs "),
                    Span::styled(c.title.to_string(), strong),
                    Span::styled(format!("  ({})", c.name), faint),
                ]));
                lines.push(Line::default());
                lines.push(Line::from(format!("  {}", c.doc)));
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    format!("  {}", c.group.title()),
                    faint,
                )));
                let others = self.keys_for(c.name);
                if c.group.global() {
                    let also = if others.is_empty() {
                        format!("  M-x {} runs it by name.", c.name)
                    } else {
                        format!("  Keys: {others}.  M-x {} runs it by name.", c.name)
                    };
                    lines.push(Line::from(Span::styled(also, faint)));
                }
            }
            None => lines.push(Line::from(vec![
                Span::styled(format!("  {keys}"), strong.fg(Color::Cyan)),
                Span::raw(" is not bound to anything here."),
            ])),
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  q or Esc closes this page",
            faint,
        )));
        self.open_page("Describe key", PageKind::Text(lines));
    }

    pub(crate) fn info_scroll(&mut self, d: isize) {
        let page = crate::diffview::body_rows(self.text_h);
        if let Some(v) = self.info.as_mut() {
            let last = v.lines.len().saturating_sub(page) as isize;
            v.top = (v.top as isize).saturating_add(d).clamp(0, last.max(0)) as usize;
        }
    }

    pub(crate) fn info_page(&mut self, d: isize) {
        let page = crate::diffview::body_rows(self.text_h) as isize;
        self.info_scroll(d * page);
    }

    /// Every key in effect as cards: the global commands by group, then each
    /// mode's own keys.
    fn binding_cards(&self, width: usize) -> Vec<Line<'static>> {
        let nano = self.config.nano_keys;
        let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for g in [
            Group::Help,
            Group::File,
            Group::Edit,
            Group::Mark,
            Group::Search,
            Group::Move,
            Group::Code,
            Group::Buffers,
            Group::Todo,
            Group::View,
            Group::Host,
        ] {
            let entries: Vec<(String, String)> = commands()
                .iter()
                .filter(|c| c.group == g)
                .map(|c| {
                    let k = self.keys_for(c.name);
                    let k = if k.is_empty() {
                        format!("M-x {}", c.name)
                    } else {
                        k
                    };
                    (k, c.title.to_string())
                })
                .collect();
            sections.push((g.title().to_string(), entries));
        }
        let k = &self.keymaps;
        for (title, maps) in [
            ("Diff view", vec![&k.diff, &k.save]),
            ("Patch and conflict views", vec![&k.patch, &k.conflict]),
            ("Lists", vec![&k.list, &k.buffers]),
            ("M-x", vec![&k.palette]),
            ("Help pages", vec![&k.page]),
        ] {
            sections.push((title.to_string(), mode_entries(&maps, nano)));
        }
        cards(&sections, width)
    }
}

/// A mode's keys, every key per command joined.
fn mode_entries(maps: &[&Keymap], nano: bool) -> Vec<(String, String)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for m in maps {
        for (seq, name) in m.flatten() {
            let Some(c) = command(&name) else { continue };
            let k = notation(nano, &seq);
            match out.iter_mut().find(|(t, _)| t == c.title) {
                Some((_, ks)) => ks.push(k),
                None => out.push((c.title.to_string(), vec![k])),
            }
        }
    }
    out.into_iter().map(|(t, ks)| (ks.join(" "), t)).collect()
}

fn overview_intro(ed: &Editor) -> Vec<Line<'static>> {
    let strong = Style::new().add_modifier(Modifier::BOLD);
    let k = |name: &str| ed.keys_for(name);
    let mut l = vec![
        Line::default(),
        Line::from(Span::styled(
            "  rano: a nano-style editor with emacs-style keymaps",
            strong,
        )),
        Line::default(),
    ];
    for s in [
        format!("  {}  run any command by name: type words, Enter runs it.", k("execute-extended-command")),
        format!("  {}  what does a key do?   {}  every key, as below.", k("describe-key"), k("describe-bindings")),
        "  A prefix key (the help key, M-t for todo lists) shows what can follow it after a moment.".into(),
        "  A view (lists, diffs, these pages) has its own keys, shown in the bar while it is open.".into(),
        "  Type to insert; the mark (set-mark) starts a region that cut and copy act on.".into(),
    ] {
        l.push(Line::from(s));
    }
    l.push(Line::default());
    l
}

/// Sections of `(key, title)` entries as cards: a bold heading per section,
/// then its entries in as many columns as fit, keys coloured.
pub fn cards(sections: &[(String, Vec<(String, String)>)], width: usize) -> Vec<Line<'static>> {
    let key_style = Style::new().fg(Color::Cyan);
    let mut out = Vec::new();
    for (title, entries) in sections {
        if entries.is_empty() {
            continue;
        }
        out.push(Line::from(Span::styled(
            format!("  {title}"),
            Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )));
        out.extend(card_rows(entries, width, key_style));
        out.push(Line::default());
    }
    out
}

/// Entries laid out in columns of equal width, row by row.
pub fn card_rows(
    entries: &[(String, String)],
    width: usize,
    key_style: Style,
) -> Vec<Line<'static>> {
    let kw = entries
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
        .min(24);
    let tw = entries
        .iter()
        .map(|(_, t)| t.chars().count())
        .max()
        .unwrap_or(0);
    let cell = (kw + 2 + tw + 3).min(width.saturating_sub(2)).max(8);
    let cols = (width.saturating_sub(2) / cell).max(1);
    entries
        .chunks(cols)
        .map(|row| {
            let mut spans = vec![Span::raw("  ")];
            for (k, t) in row {
                let k: String = k.chars().take(kw.max(1)).collect();
                let pad = kw.saturating_sub(k.chars().count());
                spans.push(Span::styled(k, key_style));
                let t_room = cell.saturating_sub(kw + 2);
                let t: String = t.chars().take(t_room).collect();
                let tpad = t_room.saturating_sub(t.chars().count());
                spans.push(Span::raw(format!(
                    "{}  {t}{}",
                    " ".repeat(pad),
                    " ".repeat(tpad)
                )));
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_match_in_any_order_and_titles_win_over_docs() {
        let t = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        assert!(score("write-out", "Write Out", "Save the buffer.", &t("out wri")).is_some());
        assert!(score("write-out", "Write Out", "Save the buffer.", &t("zzz")).is_none());
        let title = score("write-out", "Write Out", "Save the buffer.", &t("write")).unwrap();
        let doc = score("write-out", "Write Out", "Save the buffer.", &t("buffer")).unwrap();
        assert!(title > doc);
    }

    #[test]
    fn cards_fit_the_width() {
        let entries: Vec<(String, String)> = (0..10)
            .map(|i| (format!("C-{i}"), format!("Command {i}")))
            .collect();
        for w in [30usize, 60, 120] {
            for l in card_rows(&entries, w, Style::new()) {
                let n: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
                assert!(n <= w, "{w}: {n}");
            }
        }
    }
}
