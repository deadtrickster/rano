//! Global key dispatch: maps terminal key events to Editor actions
//! (nano's Ctrl/Meta/F-key tables).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::editor::Editor;

impl Editor {
    // ---------- key dispatch ----------

    pub fn handle_key(&mut self, key: KeyEvent) {
        // Keep the soft-wrap visual-row table fresh (M-\): a resize or the
        // M-\ toggle itself may have changed the wrap width.
        self.ensure_wrap_prefix();
        // Live completion popup: navigation/accept/cancel first; anything
        // else closes it and falls through to normal handling (typing and
        // backspace stay open — they re-request with the new prefix).
        if self
            .completion
            .as_ref()
            .is_some_and(|c| !c.items.is_empty())
        {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            let plain = !ctrl && !alt;
            match key.code {
                KeyCode::Up if plain => {
                    self.completion_up();
                    return;
                }
                KeyCode::Down if plain => {
                    self.completion_down();
                    return;
                }
                KeyCode::Char('p') if ctrl && !alt => {
                    self.completion_up();
                    return;
                }
                KeyCode::Char('n') if ctrl && !alt => {
                    self.completion_down();
                    return;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    self.completion_accept();
                    return;
                }
                KeyCode::Esc => {
                    self.completion_close();
                    return;
                }
                KeyCode::Backspace if plain => {}
                KeyCode::Char(_) if plain => {}
                _ => self.completion_close(),
            }
        }
        if self.help {
            self.help = false;
            return;
        }
        if let Some(p) = self.prompt.take() {
            self.handle_prompt_key(p, key);
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if alt {
            // nano 8 Meta bindings: M-A mark, M-U undo, M-E redo, M-6 copy,
            // M-B previous, M-F next, M-] to bracket.
            match key.code {
                KeyCode::Char('a') => self.toggle_mark(),
                KeyCode::Char('u') => self.undo(),
                KeyCode::Char('e') => self.redo(),
                KeyCode::Char('6') => self.copy(),
                KeyCode::Char('b') => self.prev_match(),
                KeyCode::Char('f') => self.next_match(),
                KeyCode::Char(']') => self.match_bracket(),
                // M-D jumps to the next diagnostic; word motions live on
                // Alt+Left/Right (Ctrl+Left/Right also work).
                KeyCode::Char('d') => self.jump_next_diag(),
                // M-. : jump to the definition under the cursor; M-, unwinds
                // (stacked).
                KeyCode::Char('.') => self.jump_definition(),
                KeyCode::Char(',') => self.jump_back(),
                // M-| : filter the marked rows through a shell command.
                KeyCode::Char('|') => self.start_filter(),
                // M-N: toggle the line-number gutter.
                KeyCode::Char('n') => self.show_line_numbers = !self.show_line_numbers,
                // M-\ : toggle soft line wrap (nano).
                KeyCode::Char('\\') => self.wrap = !self.wrap,
                // M-< / M-> : previous / next buffer (wraps).
                KeyCode::Char('<') => self.switch_buffer(-1),
                KeyCode::Char('>') => self.switch_buffer(1),
                KeyCode::Left => self.prev_word(),
                KeyCode::Right => self.next_word(),
                _ => {}
            }
            return;
        }
        if ctrl {
            match key.code {
                KeyCode::Char('g') => self.help = true,
                KeyCode::Char('o') => self.start_write(),
                KeyCode::Char('r') => self.start_read(),
                KeyCode::Char('k') => self.cut(),
                // Where Is (nano's ^F): forward search.
                KeyCode::Char('f') => self.start_search(),
                // Where Was (nano's ^B): backward search.
                KeyCode::Char('b') => self.start_search_backward(),
                KeyCode::Char('x') => self.try_quit(),
                KeyCode::Char('a') => self.toggle_mark(),
                KeyCode::Char('u') => self.paste(),
                // Ctrl+\ arrives as 0x1c, which crossterm reports as Char('4')+CONTROL
                KeyCode::Char('4') => self.start_replace(),
                KeyCode::Char('\\') => self.start_replace(),
                // Ctrl+_ and Ctrl+/ both arrive as 0x1f, which crossterm
                // reports as Char('7')+CONTROL. That byte is Go To Line
                // (nano's ^/); undo/redo live on M-U / M-E.
                KeyCode::Char('7') => self.start_goto(),
                KeyCode::Char('/') => self.start_goto(),
                KeyCode::Char('j') => self.justify(),
                KeyCode::Char('c') => {
                    self.loc_until = Some(Instant::now() + Duration::from_secs(2))
                }
                KeyCode::Char('d') => self.delete_char_cut(),
                KeyCode::Char('e') => self.move_end(),
                // Ctrl+T (0x14) is Execute, as in nano; redo lives on M-E.
                KeyCode::Char('t') => self.start_exec(),
                KeyCode::Char('h') => self.backspace(),
                KeyCode::Char('n') => self.next_line(),
                KeyCode::Char('p') => self.prev_line(),
                // Ctrl+Left / Ctrl+Right = Prev Word / Next Word (nano).
                KeyCode::Left => self.prev_word(),
                KeyCode::Right => self.next_word(),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_at(),
            KeyCode::Enter => self.newline(),
            KeyCode::Tab => self.indent_line(),
            KeyCode::Esc => {
                if self.bs().mark.is_some() {
                    self.bs_mut().mark = None;
                }
            }
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => self.move_up(),
            KeyCode::Down => self.move_down(),
            KeyCode::Home => self.move_home(),
            KeyCode::End => self.move_end(),
            KeyCode::PageUp => self.page_up(self.text_h),
            KeyCode::PageDown => self.page_down(self.text_h),
            KeyCode::F(1) => self.help = true,
            KeyCode::F(2) => self.start_write(),
            KeyCode::F(3) => self.start_search(),
            KeyCode::F(4) => self.start_replace(),
            KeyCode::F(5) => self.start_read(),
            KeyCode::F(6) => self.start_exec(),
            KeyCode::F(7) => self.start_backup(),
            KeyCode::F(8) => self.start_open(),
            KeyCode::F(9) => self.sort_lines(),
            KeyCode::F(10) => self.justify(),
            KeyCode::F(11) => self.start_goto(),
            KeyCode::Char(c) if !c.is_control() => self.insert_char(c),
            _ => {}
        }
    }
}
