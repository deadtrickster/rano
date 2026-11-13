//! The keys the two programs share: one table, so neither can promise a key the other does not
//! bind.
//!
//! letibot draws its hint row and its help pages from this table and sets `key_notation = words` on
//! the embedded editor, so the spelling in the head is the spelling here. A key in this table is a
//! **tool** key (design `docs/rano-integration.md` §1.2): the same chord means the same thing in
//! every focus and in both programs, and it is a rano command bound in rano's global map so
//! standalone rano and the embedded pane get it from the same `Editor`.
//!
//! Text keys are not here: nano in the editor and readline in a composer keep their own
//! conventions, and that is what protects the operator's hands in both places.

/// Which program binds a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// rano binds it in the global map.
    Editor,
    /// Reserved for the host: the head binds it to its own action, so rano must bind it nowhere.
    /// A shadow here would silently take the head's key in the embedded pane.
    Host,
}

/// One shared key.
#[derive(Debug, Clone, Copy)]
pub struct Shared {
    /// The command it runs in rano's global map.
    pub action: &'static str,
    /// The chord, as `Keymap::bind` spells it.
    pub key: &'static str,
    pub scope: Scope,
    /// One line for a hint row or a help page: what the key does, in the operator's words.
    pub hint: &'static str,
    /// Other commands a mode map may bind the same key to — the same action on a surface of its
    /// own, since Esc closes a palette exactly as it clears a mark. A binding to anything else is
    /// a shadow, and [`tests`] fails on it.
    pub also: &'static [&'static str],
}

/// Esc's other homes: every mode that is a level to back out of.
const BACK_OUT: &[&str] = &[
    "diff-close",
    "close-buffer",
    "list-close",
    "page-close",
    "palette-cancel",
    "panel-leave",
];

/// Every key both programs promise.
pub const SHARED: &[Shared] = &[
    Shared {
        action: "deactivate-mark",
        key: "ESC",
        scope: Scope::Editor,
        hint: "back out one level; at the bottom, leave",
        also: BACK_OUT,
    },
    Shared {
        action: "help",
        key: "<f1>",
        scope: Scope::Editor,
        hint: "help",
        also: &["help"],
    },
    Shared {
        action: "send-position",
        key: "M-s",
        scope: Scope::Editor,
        hint: "attach the place or selection to the next prompt",
        also: &[],
    },
    Shared {
        action: "next-diagnostic",
        key: "M-d",
        scope: Scope::Editor,
        hint: "next mark or diagnostic",
        also: &[],
    },
    // The three aliases: the same actions both widgets already have, under one key each.
    Shared {
        action: "undo",
        key: "C-z",
        scope: Scope::Editor,
        hint: "undo",
        also: &[],
    },
    Shared {
        action: "redo",
        key: "M-z",
        scope: Scope::Editor,
        hint: "redo",
        also: &[],
    },
    Shared {
        action: "paste",
        key: "C-y",
        scope: Scope::Editor,
        hint: "paste what was cut",
        also: &[],
    },
    // Reserved: the head binds these to its own actions over a rano editor.
    Shared {
        action: "host-focus",
        key: "C-]",
        scope: Scope::Host,
        hint: "cross focus, opening the other surface",
        also: &[],
    },
    Shared {
        action: "host-side",
        key: "M-i",
        scope: Scope::Host,
        hint: "show or hide the side surface",
        also: &[],
    },
    Shared {
        action: "host-zoom",
        key: "M-m",
        scope: Scope::Host,
        hint: "zoom the focused surface to full width",
        also: &[],
    },
    // rano owns the ask prefix: the menu is its key bar, and the host hears the pick through
    // `on_ask` - so standalone rano asks the same way the embedded pane does.
    Shared {
        action: "ask",
        key: "M-k",
        scope: Scope::Editor,
        hint: "quick ask on the selection",
        also: &[],
    },
    Shared {
        action: "host-interrupt",
        key: "ESC ESC",
        scope: Scope::Host,
        hint: "interrupt the turn",
        also: &[],
    },
];

/// The rows rano binds itself.
pub fn editor_rows() -> impl Iterator<Item = &'static Shared> {
    SHARED.iter().filter(|r| r.scope == Scope::Editor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Keymaps, command};
    use crate::keymap::{Key, Lookup, lookup, parse_seq};

    fn maps() -> Vec<(&'static str, Keymaps)> {
        // One standard set is every map this program ships; the name is only for the message.
        vec![("standard", Keymaps::standard())]
    }

    /// Walking every map is what makes a shadow fail here rather than in the embedded pane.
    fn each_map(k: &Keymaps, mut f: impl FnMut(&str, &crate::keymap::Keymap)) {
        for m in [
            &k.global,
            &k.diff,
            &k.save,
            &k.patch,
            &k.markdown,
            &k.picture,
            &k.view,
            &k.conflict,
            &k.review,
            &k.dir,
            &k.list,
            &k.buffers,
            &k.page,
            &k.palette,
            &k.panel,
        ] {
            f(&m.name, m);
        }
    }

    /// Every shared key resolves to its action in the global map, and no map binds it to anything
    /// else — the test the design asks each repo to keep.
    #[test]
    fn every_shared_key_resolves_to_its_action_and_no_map_shadows_it() {
        let k = Keymaps::standard();
        for row in SHARED {
            let Some(seq) = parse_seq(row.key) else {
                panic!("{} is not a key sequence", row.key);
            };
            fn at<'a>(m: &'a crate::keymap::Keymap, seq: &[Key]) -> Lookup<'a> {
                lookup(&[m], seq)
            }
            match row.scope {
                Scope::Editor => {
                    match at(&k.global, &seq) {
                        Lookup::Command(name) => assert_eq!(
                            name, row.action,
                            "{} is bound to {name}, not {}",
                            row.key, row.action
                        ),
                        Lookup::Prefix(m) => assert_eq!(
                            m.name, row.action,
                            "{} opens the {} map, not {}",
                            row.key, m.name, row.action
                        ),
                        Lookup::Unbound => panic!("{} is unbound in the global map", row.key),
                    }
                    assert!(
                        command(row.action).is_some() || row.action == "help",
                        "{} names a command that does not exist: {}",
                        row.key,
                        row.action
                    );
                }
                Scope::Host => {
                    // Reserved means reserved: a row the head binds must be free here, or the
                    // embedded pane would quietly take the head's key.
                    each_map(&k, |name, m| {
                        assert_eq!(
                            at(m, &seq),
                            Lookup::Unbound,
                            "{} is bound in the {name} map, and it belongs to the host",
                            row.key
                        );
                    });
                }
            }
            // Whatever a mode does bind the key to, it must be the same action on its own surface.
            for (name, maps) in maps() {
                each_map(&maps, |map_name, m| {
                    if let Lookup::Command(c) = at(m, &seq)
                        && c != row.action
                    {
                        assert!(
                            row.also.contains(&c),
                            "the {map_name} map binds {} to {c} ({name}), which shadows {}",
                            row.key,
                            row.action
                        );
                    }
                });
            }
        }
    }
}
