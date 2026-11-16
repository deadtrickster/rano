//! The editor with the `agent` feature OFF: the panel's commands answer by name, nothing
//! connects, and the C-] cross still trades the text and the terminal. This file is empty in a
//! default build; the proof runs as `cargo test --no-default-features --test agentless`, where
//! the library's own cfg(test) modules are not compiled and only the real public API is used.
#![cfg(not(feature = "agent"))]

use rano::buffer::Buffer;
use rano::config;
use rano::editor::{Editor, Focus};
fn editor() -> Editor {
    Editor::new(Buffer::new(), config::Config::default())
}
// The commands are called directly: M-i and C-] are the binary's host map, not the library's,
// and what is proven here is what those commands DO without the feature.

fn flashed(ed: &Editor) -> String {
    ed.status
        .as_ref()
        .map(|f| f.text.clone())
        .unwrap_or_default()
}

#[test]
fn the_panel_commands_refuse_by_name_and_nothing_connects() {
    let mut ed = editor();
    ed.panel_toggle();
    assert!(!ed.panel.open, "no pane opens without the feature");
    assert!(
        ed.panel.take_acts().is_empty(),
        "and no daemon is ever contacted"
    );
    assert!(
        flashed(&ed).contains("no letibot panel"),
        "M-i says why: {:?}",
        flashed(&ed)
    );
    ed.panel_send();
    assert!(
        flashed(&ed).contains("no letibot panel"),
        "the panel's own commands answer the same way: {:?}",
        flashed(&ed)
    );
    assert!(ed.panel.turn_line(60).is_none(), "no turn is claimed");
    assert!(!ed.panel.tick_turn(), "and no clock ticks");
    assert!(ed.panel.lines().is_empty(), "and no rows are drawn");
}

#[test]
fn the_cross_still_trades_the_text_and_the_terminal() {
    let mut ed = editor();
    ed.bottom.open = true;
    ed.bottom.height = 12;
    ed.panel_cross();
    assert_eq!(ed.focus, Focus::Terminal, "the strip is still a surface");
    ed.panel_cross();
    assert_eq!(ed.focus, Focus::Text, "and back");
    // With no strip either, the key says why nothing moved instead of opening a panel.
    let mut ed = editor();
    ed.panel_cross();
    assert_eq!(ed.focus, Focus::Text);
    assert!(flashed(&ed).contains("no letibot panel"));
}

#[test]
fn the_conversation_vocabulary_is_not_the_pane() {
    // The folder logic and the session memory are the ask/embedding flows' too: real, not
    // stubbed, with the feature off.
    let dir = std::env::temp_dir().join(format!("rano_agentless_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    assert_eq!(rano::panel::folder_for(Some(&file), &dir), dir);
    rano::panel::remember_in(&dir.join("sessions"), &dir, "s-1").unwrap();
    assert_eq!(
        rano::panel::remembered_in(&dir.join("sessions"), &dir).as_deref(),
        Some("s-1"),
        "the session memory still works"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
