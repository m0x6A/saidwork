//! Rendering of the [`Model`].

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap};

use crate::model::{Model, Phase};

/// Draw the whole screen.
pub fn render(model: &Model, frame: &mut Frame) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(frame.area());
    render_header(model, frame, rows[0]);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(rows[1]);
    render_formats(model, frame, cols[0]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(3)])
        .split(cols[1]);
    render_session(model, frame, right[0]);
    render_log(model, frame, right[1]);
    render_help(model, frame, rows[2]);
}

fn render_header(model: &Model, frame: &mut Frame, area: Rect) {
    let mut spans = vec![Span::styled(
        " saidwork ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    for p in &model.providers {
        let color = if p.status == "up" {
            Color::Green
        } else {
            Color::Yellow
        };
        spans.push(Span::raw(format!("  {} ", p.role)));
        spans.push(Span::styled(
            format!("{} ({})", p.name, p.status),
            Style::default().fg(color),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_formats(model: &Model, frame: &mut Frame, area: Rect) {
    let items: Vec<ListItem> = model
        .formats
        .iter()
        .map(|f| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    f.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" — {}", f.description)),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(if model.formats.is_empty() {
        None
    } else {
        Some(model.selected_format)
    });
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Format (↑/↓) "),
        )
        .highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_session(model: &Model, frame: &mut Frame, area: Rect) {
    let (status, color) = match &model.phase {
        Phase::Idle => ("idle — press r to record".to_owned(), Color::Gray),
        Phase::Preparing => ("preparing providers…".to_owned(), Color::Yellow),
        Phase::Recording => (
            format!(
                "● recording  {:>5.1}s — press r to stop",
                model.elapsed.as_secs_f64()
            ),
            Color::Red,
        ),
        Phase::Processing => ("processing…".to_owned(), Color::Yellow),
        Phase::Done { note_path } => (format!("saved {}", note_path.display()), Color::Green),
        Phase::Failed { message } => (format!("failed: {message}"), Color::Red),
    };
    let device = |label: &str, d: &Option<saidwork_app::AudioDevice>| {
        Line::from(vec![
            Span::raw(format!("{label:<13}")),
            Span::raw(
                d.as_ref()
                    .map_or_else(|| "none".to_owned(), |d| d.label.clone()),
            ),
        ])
    };
    let block = Block::default().borders(Borders::ALL).title(" Session ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(status, Style::default().fg(color)))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(device("mic (m):", &model.selection.microphone)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(device("system (s):", &model.selection.system_audio)),
        rows[2],
    );
    let gauge = Gauge::default()
        .ratio(f64::from(model.level.clamp(0.0, 1.0)))
        .label(if model.phase == Phase::Recording {
            "level"
        } else {
            ""
        })
        .gauge_style(Style::default().fg(Color::Green).bg(Color::DarkGray));
    frame.render_widget(gauge, rows[4]);
}

fn render_log(model: &Model, frame: &mut Frame, area: Rect) {
    let visible = usize::from(area.height.saturating_sub(2));
    let start = model.log.len().saturating_sub(visible);
    let text: Vec<Line> = model.log[start..]
        .iter()
        .map(|l| Line::from(l.as_str()))
        .collect();
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(" Log ")),
        area,
    );
}

fn render_help(_model: &Model, frame: &mut Frame, area: Rect) {
    let help = " r record/stop   ↑/↓ format   m mic   s system audio   p probe providers   q quit";
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use saidwork_app::{AudioDevice, DeviceSelection, Format, FormatId, ProviderName, SourceKind};

    use super::*;
    use crate::model::ProviderView;

    #[test]
    fn renders_recording_screen() {
        let formats = vec![
            Format {
                id: FormatId::new("meeting-notes").unwrap(),
                name: "Meeting notes".into(),
                description: "Minutes".into(),
                when_to_use: None,
                template: "t".into(),
            },
            Format {
                id: FormatId::new("task-list").unwrap(),
                name: "Task list".into(),
                description: "Todos".into(),
                when_to_use: None,
                template: "t".into(),
            },
        ];
        let mut model = Model::new(formats, Some("task-list"));
        model.selection = DeviceSelection {
            microphone: Some(AudioDevice {
                id: "m".into(),
                label: "USB Mic".into(),
                kind: SourceKind::Microphone,
                is_default: true,
            }),
            system_audio: None,
        };
        model.providers = vec![
            ProviderView {
                name: ProviderName::new("whisper").unwrap(),
                role: "stt",
                status: "up".into(),
            },
            ProviderView {
                name: ProviderName::new("ollama").unwrap(),
                role: "llm",
                status: "down".into(),
            },
        ];
        model.phase = Phase::Recording;
        model.level = 0.5;
        model.elapsed = std::time::Duration::from_millis(12_300);
        model.push_log("capturing microphone: USB Mic");

        let mut terminal = Terminal::new(TestBackend::new(90, 18)).unwrap();
        terminal.draw(|f| render(&model, f)).unwrap();
        insta::assert_snapshot!(terminal.backend().to_string());
    }

    #[test]
    fn renders_without_formats_or_devices() {
        let mut model = Model::new(vec![], None);
        model.devices_loaded = true;
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| render(&model, f)).unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("idle"));
        assert!(text.contains("none"));
    }
}
