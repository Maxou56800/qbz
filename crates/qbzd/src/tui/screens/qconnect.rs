// crates/qbzd/src/tui/screens/qconnect.rs — the Qobuz Connect screen (03 §3.4).
//
// Writes the daemon-root qconnect_settings.db KV via the App's write_one path
// (qconnect.startup_mode / device_name / volume_mode / block_lan). `device_uuid`
// is never displayed. `remember_last` is not offered (desktop-ism). The
// device-name preview resolves through the SAME `resolve_qconnect_friendly_name`
// the connect path uses, so the phone-facing name shown here is exactly what
// will appear. `block_lan` is the daemon's OWN LAN safety switch — a separate
// flag from the desktop's Settings -> Network kill switch, since qbzd never
// reads the desktop's profile (see qconnect/transport.rs's doc comment).

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;
use serde_json::Value;

use crate::qconnect::transport as qconnect_kv;
use crate::tui::app::{DrawCtx, ScreenAction};
use crate::tui::strings as s;
use crate::tui::theme;
use crate::tui::widgets::{self, InputOutcome, SelectOutcome, SelectPopup, TextInput};

#[derive(Debug, Clone, PartialEq)]
struct Staged {
    enable: bool,
    /// Empty = clear the override (desktop write semantics, §3.4).
    device_name: String,
    /// "software" (OD4 default) | "locked".
    volume_mode: String,
    /// Daemon's own LAN safety switch — see qconnect/mod.rs::start_lan and
    /// qconnect/transport.rs::load_block_lan_at. NOT the desktop's
    /// block_qconnect_lan (a separate flag in a separate profile).
    block_lan: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QField {
    Enable,
    DeviceName,
    VolumeMode,
    BlockLan,
}

enum Editor {
    Name(TextInput),
    Volume(SelectPopup),
}

pub struct QConnectState {
    baseline: Staged,
    staged: Staged,
    focus: usize,
    editor: Option<Editor>,
}

const FIELDS: [QField; 4] = [QField::Enable, QField::DeviceName, QField::VolumeMode, QField::BlockLan];

impl QConnectState {
    pub fn new(
        startup_on: bool,
        device_name: Option<String>,
        volume_mode: Option<String>,
        block_lan: bool,
    ) -> Self {
        let staged = Staged {
            enable: startup_on,
            device_name: device_name.unwrap_or_default(),
            volume_mode: volume_mode.unwrap_or_else(|| s::VOL_SOFTWARE.to_string()),
            block_lan,
        };
        Self {
            baseline: staged.clone(),
            staged,
            focus: 0,
            editor: None,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.staged != self.baseline
    }
    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }
    pub fn mark_saved(&mut self) {
        self.baseline = self.staged.clone();
    }

    /// The breadcrumb's level-2 node when a field editor/picker is active.
    pub fn editing_label(&self) -> Option<&'static str> {
        match &self.editor {
            Some(Editor::Name(_)) => Some(s::QC_DEVICE_NAME),
            Some(Editor::Volume(_)) => Some(s::QC_VOLUME_MODE),
            None => None,
        }
    }

    pub fn save_keys(&self) -> Vec<(String, String)> {
        let b = &self.baseline;
        let a = &self.staged;
        let mut out = Vec::new();
        if a.enable != b.enable {
            out.push((
                "qconnect.startup_mode".to_string(),
                if a.enable { "on" } else { "off" }.to_string(),
            ));
        }
        if a.device_name != b.device_name {
            // Empty clears the override; write_one's parse treats ""/system as clear.
            out.push(("qconnect.device_name".to_string(), a.device_name.clone()));
        }
        if a.volume_mode != b.volume_mode {
            out.push(("qconnect.volume_mode".to_string(), a.volume_mode.clone()));
        }
        if a.block_lan != b.block_lan {
            out.push(("qconnect.block_lan".to_string(), a.block_lan.to_string()));
        }
        out
    }

    fn effective_name(&self) -> String {
        let custom = if self.staged.device_name.trim().is_empty() {
            None
        } else {
            Some(self.staged.device_name.as_str())
        };
        qconnect_kv::resolve_qconnect_friendly_name(custom)
    }

    // -------------------------- input --------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> ScreenAction {
        if self.editor.is_some() {
            return self.handle_editor_key(key);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                self.focus = if self.focus == 0 { FIELDS.len() - 1 } else { self.focus - 1 };
                ScreenAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.focus = (self.focus + 1) % FIELDS.len();
                ScreenAction::Consumed
            }
            KeyCode::Char('s') => ScreenAction::Save,
            KeyCode::Enter | KeyCode::Char(' ') => {
                match FIELDS[self.focus] {
                    QField::Enable => self.staged.enable ^= true,
                    QField::BlockLan => self.staged.block_lan ^= true,
                    QField::DeviceName => {
                        self.editor = Some(Editor::Name(TextInput::new(&self.staged.device_name, false)));
                    }
                    QField::VolumeMode => {
                        let opts = vec![s::VOL_SOFTWARE.to_string(), s::VOL_LOCKED.to_string()];
                        let sel = if self.staged.volume_mode == "locked" { 1 } else { 0 };
                        self.editor = Some(Editor::Volume(SelectPopup::new(s::QC_VOLUME_MODE, opts, sel, false)));
                    }
                }
                ScreenAction::Consumed
            }
            KeyCode::Esc => ScreenAction::Back,
            _ => ScreenAction::Consumed,
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent) -> ScreenAction {
        let editor = self.editor.take().unwrap();
        match editor {
            Editor::Name(mut input) => match input.handle_key(key) {
                InputOutcome::Accepted => {
                    self.staged.device_name = input.buf.trim().to_string();
                    ScreenAction::Consumed
                }
                InputOutcome::Cancelled => ScreenAction::Consumed,
                InputOutcome::Pending => {
                    self.editor = Some(Editor::Name(input));
                    ScreenAction::Consumed
                }
            },
            Editor::Volume(mut p) => match p.handle_key(key) {
                SelectOutcome::Chosen(i) => {
                    self.staged.volume_mode = if i == 1 { "locked" } else { "software" }.to_string();
                    ScreenAction::Consumed
                }
                SelectOutcome::Cancelled => ScreenAction::Consumed,
                SelectOutcome::Pending => {
                    self.editor = Some(Editor::Volume(p));
                    ScreenAction::Consumed
                }
            },
        }
    }

    // -------------------------- render --------------------------

    pub fn draw(&self, f: &mut Frame, area: Rect, ctx: &DrawCtx) {
        let width = area.width.saturating_sub(2); // section inner width
        let ctrl_col = widgets::control_column(
            &[s::QC_ENABLE, s::QC_DEVICE_NAME, s::QC_VOLUME_MODE, s::QC_BLOCK_LAN],
            width,
        );
        let mut lines: Vec<Line> = Vec::new();
        let mut within: Option<(u16, u16)> = None;
        for (i, field) in FIELDS.iter().enumerate() {
            let focused = i == self.focus && self.editor.is_none();
            let (label, value, widget) = match field {
                QField::Enable => (
                    s::QC_ENABLE,
                    if self.staged.enable { "on" } else { "off" }.to_string(),
                    "[toggle]",
                ),
                QField::DeviceName => {
                    let shown = if let Some(Editor::Name(input)) = &self.editor {
                        input.display()
                    } else if self.staged.device_name.is_empty() {
                        self.effective_name()
                    } else {
                        self.staged.device_name.clone()
                    };
                    (s::QC_DEVICE_NAME, shown, "[input]")
                }
                QField::VolumeMode => (s::QC_VOLUME_MODE, self.staged.volume_mode.clone(), "[select]"),
                QField::BlockLan => (
                    s::QC_BLOCK_LAN,
                    if self.staged.block_lan { "on" } else { "off" }.to_string(),
                    "[toggle]",
                ),
            };
            let start = lines.len() as u16;
            let mut block = widgets::field_block(
                &widgets::Field { label, value, widget, focused, enabled: true, reason: None, description: None },
                ctrl_col,
                width,
            );
            // The device-name field carries a live preview + applies-next note.
            if *field == QField::DeviceName {
                block.extend(widgets::wrapped_note(&s::qc_preview(&self.effective_name()), width, theme::dim()));
                block.extend(widgets::wrapped_note(s::QC_APPLIES_NEXT, width, theme::dim()));
            }
            if *field == QField::BlockLan {
                block.extend(widgets::wrapped_note(s::QC_BLOCK_LAN_NOTE, width, theme::dim()));
                block.extend(widgets::wrapped_note(s::QC_APPLIES_NEXT, width, theme::dim()));
            }
            if focused {
                within = Some((start, block.len() as u16));
            }
            lines.extend(block);
        }

        // Live status line (§3.4) — from GET /api/status when the daemon runs.
        if let Some(live) = ctx.status.and_then(qconnect_live_line) {
            lines.push(widgets::blank());
            lines.extend(widgets::wrapped_note(&live, width, theme::dim()));
        }

        let mut secs: Vec<widgets::Section> = Vec::new();
        let mut anchor: Option<widgets::FocusAnchor> = None;
        widgets::push_section(&mut secs, &mut anchor, s::QCONNECT_SECTION, true, lines, within);
        widgets::sections_scroll(f, area, &secs, anchor);

        match &self.editor {
            Some(Editor::Volume(p)) => p.draw(f, area),
            Some(Editor::Name(input)) => {
                widgets::modal(f, area, s::QC_DEVICE_NAME, &input.display(), s::HELP_INPUT)
            }
            None => {}
        }
    }
}

/// One-line live QConnect state from the status payload's qconnect block.
fn qconnect_live_line(p: &Value) -> Option<String> {
    let qc = p.get("qconnect")?;
    let enabled = qc.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    if !enabled {
        return None;
    }
    let state = qc.get("state").and_then(Value::as_str).unwrap_or("");
    let session = qc.get("session_active").and_then(Value::as_bool).unwrap_or(false);
    let mut parts = Vec::new();
    parts.push(format!("live: {}", if state.is_empty() { "enabled" } else { state }));
    if session {
        parts.push("session active".to_string());
    }
    Some(parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_keys_only_emits_changed_fields() {
        let mut st = QConnectState::new(false, None, None, false);
        assert!(st.save_keys().is_empty(), "clean screen writes nothing");
        st.staged.block_lan = true;
        assert_eq!(
            st.save_keys(),
            vec![("qconnect.block_lan".to_string(), "true".to_string())]
        );
    }

    #[test]
    fn enter_on_the_block_lan_field_toggles_it() {
        use ratatui::crossterm::event::KeyModifiers;

        let mut st = QConnectState::new(false, None, None, false);
        st.focus = FIELDS.iter().position(|f| *f == QField::BlockLan).unwrap();
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(st.staged.block_lan);
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!st.staged.block_lan);
    }
}
