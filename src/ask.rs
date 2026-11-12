//! Quick asks on a selection: a preset is data — a key under `M-k`, a label, where
//! its answer goes and the prompt — so the same article-writing asks work from
//! standalone rano and from a host's pane.

use crate::keymap::Key;
use crate::send::SendEvent;
use crate::term::KeyCode;

/// Where a preset's answer lands: prose in the conversation, notes on the
/// selection, or a suggested replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Answer {
    #[default]
    Chat,
    Notes,
    Suggest,
}

impl Answer {
    /// `chat`, `notes` or `suggest`, as a config line writes it.
    pub fn parse(v: &str) -> Option<Answer> {
        match v.trim().to_ascii_lowercase().as_str() {
            "chat" => Some(Answer::Chat),
            "note" | "notes" => Some(Answer::Notes),
            "suggest" | "suggestion" => Some(Answer::Suggest),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Answer::Chat => "chat",
            Answer::Notes => "notes",
            Answer::Suggest => "suggest",
        }
    }
}

/// One ask: a key under `M-k`, a label for the key bar and the help page, where its
/// answer goes, and the prompt the host sends. [`AskPreset::id`] names it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskPreset {
    pub id: String,
    pub key: Key,
    pub label: String,
    pub answer: Answer,
    pub prompt: String,
}

/// The compiled-in presets, in the order the key bar shows them. A config line
/// `ask.<id>.<field>` changes one of these field by field, and adds one of its own.
pub fn default_presets() -> Vec<AskPreset> {
    let one = |id: &str, key: char, label: &str, answer: Answer, prompt: &str| AskPreset {
        id: id.to_string(),
        key: Key::plain(KeyCode::Char(key)),
        label: label.to_string(),
        answer,
        prompt: prompt.to_string(),
    };
    vec![
        one(
            "explain",
            'e',
            "explain",
            Answer::Chat,
            "Explain the selection: what it says, who it is for and why it is put that way.",
        ),
        one(
            "fact-check",
            'f',
            "fact check",
            Answer::Notes,
            "Check each factual claim in the selection. One note per claim: ok, warn or \
             wrong, and why in a sentence.",
        ),
        one(
            "fill-gap",
            'g',
            "fill the gap",
            Answer::Suggest,
            "Fill the gap the selection leaves: write the text that belongs there, in the \
             same voice and at the same length.",
        ),
        one(
            "rewrite",
            'r',
            "rewrite",
            Answer::Suggest,
            "Rewrite the selection: the same meaning in better prose. Answer with the \
             replacement only.",
        ),
    ]
}

/// What a host receives when a preset's key is pressed: the ask, and the place it is
/// about — the selection, or the paragraph or line the cursor is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskEvent {
    pub send: SendEvent,
    /// The preset's id; `ask` for the free-text key, which has no prompt of its own.
    pub preset: String,
    pub answer: Answer,
    /// What to send; empty for the free-text key, where the operator types instead.
    pub prompt: String,
}

/// What a host does with an [`AskEvent`]: `Ok` carries what to tell the reader.
pub type OnAsk = Box<dyn FnMut(&AskEvent) -> Result<String, String>>;

/// A preset's binding is `ask:<id>`: a preset is config, not a command in the table,
/// so the dispatcher resolves the name itself.
pub(crate) const BIND: &str = "ask:";

/// The preset id a keymap entry names; `None` for a command in the table.
pub(crate) fn preset_of(name: &str) -> Option<&str> {
    name.strip_prefix(BIND)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_distinct_and_ask_about_something() {
        let p = default_presets();
        assert_eq!(p.len(), 4);
        for (i, a) in p.iter().enumerate() {
            for b in &p[i + 1..] {
                assert_ne!(a.id, b.id, "two presets with one id");
                assert_ne!(a.key, b.key, "{} and {} share a key", a.id, b.id);
            }
            assert!(!a.prompt.is_empty(), "{} has nothing to ask", a.id);
            assert!(!a.label.is_empty(), "{} has no label for the bar", a.id);
        }
    }

    #[test]
    fn an_answer_mode_reads_its_config_spelling() {
        assert_eq!(Answer::parse("notes"), Some(Answer::Notes));
        assert_eq!(Answer::parse(" Suggest "), Some(Answer::Suggest));
        assert_eq!(Answer::parse("chat"), Some(Answer::Chat));
        assert_eq!(Answer::parse("poetry"), None);
        for a in [Answer::Chat, Answer::Notes, Answer::Suggest] {
            assert_eq!(Answer::parse(a.as_str()), Some(a));
        }
    }

    #[test]
    fn a_binding_names_its_preset() {
        assert_eq!(preset_of("ask:fact-check"), Some("fact-check"));
        assert_eq!(preset_of("send-position"), None);
    }
}
