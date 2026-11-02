//! A cell's style: the roles it means, attributes on top, a hyperlink, and the two raw
//! colours the kitty graphics placeholders need.

use std::sync::Arc;

use crate::style::{Attrs, Look, Palette, Role};

/// Roles a style can stack (a keyword, in an added line, inside a block); a
/// fixed array keeps the style `Clone`-cheap and allocation-free.
const DEPTH: usize = 3;

/// What a cell looks like, said as meaning. The roles are resolved by a [`Palette`]
/// only when the buffer is emitted, so nothing that builds a frame names a colour.
/// [`Style::patch`] stacks roles rather than replacing them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Style {
    /// Bottom to top; `None` past the last.
    roles: [Option<Role>; DEPTH],
    /// Attributes asked for directly, on top of whatever the roles give.
    pub attrs: Attrs,
    /// A hyperlink (OSC 8), as an attribute of the cells it covers: the emitter
    /// opens and closes it around the linked run, so clipping or truncation
    /// can never leave it open.
    pub link: Option<Arc<str>>,
    /// Colours no role can name. See [`Raw`].
    pub raw: Raw,
}

/// The escape hatch: the kitty placeholders carry the image id in a 24-bit
/// foreground and the placement id in the underline colour. Nothing else
/// should use this.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Raw {
    /// A 24-bit foreground, `38;2;r;g;b`.
    pub fg_rgb: Option<[u8; 3]>,
    /// An underline colour index, `58;5;n`.
    pub underline: Option<u8>,
}

impl Style {
    pub fn new() -> Style {
        Style::default()
    }

    /// A style that means `r`.
    pub fn of(r: Role) -> Style {
        Style::new().role(r)
    }

    /// `r` stacked on top of what this style already means.
    pub fn role(mut self, r: Role) -> Style {
        self.push(r);
        self
    }

    /// The role on top, or [`Role::Plain`].
    pub fn top(&self) -> Role {
        self.roles
            .iter()
            .rev()
            .flatten()
            .next()
            .copied()
            .unwrap_or(Role::Plain)
    }

    /// The roles, bottom to top.
    pub fn roles(&self) -> impl Iterator<Item = Role> + '_ {
        self.roles.iter().flatten().copied()
    }

    pub fn bold(mut self) -> Style {
        self.attrs = self.attrs | Attrs::BOLD;
        self
    }
    pub fn dim(mut self) -> Style {
        self.attrs = self.attrs | Attrs::DIM;
        self
    }
    pub fn italic(mut self) -> Style {
        self.attrs = self.attrs | Attrs::ITALIC;
        self
    }
    pub fn underline(mut self) -> Style {
        self.attrs = self.attrs | Attrs::UNDERLINE;
        self
    }
    pub fn reverse(mut self) -> Style {
        self.attrs = self.attrs | Attrs::REVERSE;
        self
    }

    /// Link the cells this style covers to `url`.
    pub fn link(mut self, url: impl Into<Arc<str>>) -> Style {
        self.link = Some(url.into());
        self
    }

    pub fn raw(mut self, raw: Raw) -> Style {
        self.raw = raw;
        self
    }

    fn push(&mut self, r: Role) {
        // Plain means nothing, and stacking it would only push a real role off the bottom.
        if r == Role::Plain {
            return;
        }
        match self.roles.iter().position(Option::is_none) {
            Some(i) => self.roles[i] = Some(r),
            None => {
                // Full: the bottom goes — the top is what the caller wants now.
                self.roles.rotate_left(1);
                self.roles[DEPTH - 1] = Some(r);
            }
        }
    }

    /// `over` laid on top of this: its roles stacked above these, attributes
    /// added, its link and raw colours where it has them.
    pub fn patch(&self, over: &Style) -> Style {
        let mut s = self.clone();
        for r in over.roles() {
            s.push(r);
        }
        s.attrs = s.attrs | over.attrs;
        if over.link.is_some() {
            s.link = over.link.clone();
        }
        if over.raw.fg_rgb.is_some() {
            s.raw.fg_rgb = over.raw.fg_rgb;
        }
        if over.raw.underline.is_some() {
            s.raw.underline = over.raw.underline;
        }
        s
    }

    /// What the roles and attributes look like under `p`.
    pub fn look(&self, p: Palette) -> Look {
        if p == Palette::None {
            return Look::PLAIN;
        }
        let mut l = Look::PLAIN;
        for r in self.roles() {
            l = l.patch(p.look(r));
        }
        l.attrs = l.attrs | self.attrs;
        l
    }
}

impl From<Role> for Style {
    fn from(r: Role) -> Style {
        Style::of(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_patched_over_a_row_keeps_the_rows_tint() {
        let row = Style::of(Role::Added);
        let kw = Style::of(Role::Keyword);
        let both = row.patch(&kw);
        assert_eq!(both.top(), Role::Keyword);
        let l = both.look(Palette::Colour);
        assert_eq!(l.bg, Palette::Colour.look(Role::Added).bg);
        assert_eq!(l.fg, Palette::Colour.look(Role::Keyword).fg);
        assert_eq!(row.patch(&Style::new()), row);
    }

    #[test]
    fn the_stack_drops_its_bottom_when_full() {
        let s = Style::of(Role::Reasoning)
            .role(Role::Added)
            .role(Role::Keyword)
            .role(Role::Emphasis);
        assert_eq!(
            s.roles().collect::<Vec<_>>(),
            vec![Role::Added, Role::Keyword, Role::Emphasis]
        );
    }

    #[test]
    fn the_none_palette_resolves_everything_to_nothing() {
        let s = Style::of(Role::Heading)
            .bold()
            .link("https://x.org")
            .raw(Raw {
                fg_rgb: Some([1, 2, 3]),
                underline: Some(1),
            });
        assert_eq!(s.look(Palette::None), Look::PLAIN);
    }
}
