use crate::Result;
use crate::config::{Config, Key};
use crate::store::Store;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Flex, Layout, Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};

struct Field {
    label: &'static str,
    value: String,
    secret: bool,
}

impl Field {
    fn new(label: &'static str, value: &str, secret: bool) -> Field {
        Field {
            label,
            value: value.to_string(),
            secret,
        }
    }
}

/// Unlocks the account, running first-time setup if there is none yet.
/// Returns `None` if the user backed out.
pub fn run(terminal: &mut DefaultTerminal) -> Result<Option<(Config, Key, Store)>> {
    let Some(mut config) = Config::load()? else {
        return setup(terminal);
    };

    let mut error = String::new();
    let key = loop {
        let mut fields = [Field::new("Hermes password", "", true)];
        if !form(terminal, &config.username, &mut fields, &error)? {
            return Ok(None);
        }
        notice(terminal, "Unlocking...")?;
        match config.unlock(&fields[0].value) {
            Ok(key) => break key,
            Err(e) => error = e.to_string(),
        }
    };
    let store = Store::open(&config.db_path()?, &key)?;

    config.password = match store.setting("imap_password")? {
        Some(password) => password,
        // The database was deleted since setup.
        None => {
            let mut fields = [Field::new("Mail password (app password)", "", true)];
            if !form(terminal, &config.username, &mut fields, "")? {
                return Ok(None);
            }
            store.set_setting("imap_password", &fields[0].value)?;
            fields[0].value.clone()
        }
    };
    Ok(Some((config, key, store)))
}

fn setup(terminal: &mut DefaultTerminal) -> Result<Option<(Config, Key, Store)>> {
    let mut fields = [
        Field::new("Email address", "", false),
        Field::new("IMAP host", "imap.gmail.com", false),
        Field::new("SMTP host", "smtp.gmail.com", false),
        Field::new("Mail password (app password)", "", true),
        Field::new("Hermes password (asked on every start)", "", true),
        Field::new("Repeat hermes password", "", true),
    ];
    let mut error = String::new();
    loop {
        if !form(terminal, "Set up hermes", &mut fields, &error)? {
            return Ok(None);
        }
        if fields[..5].iter().any(|f| f.value.trim().is_empty()) {
            error = "every field is required".to_string();
        } else if fields[4].value != fields[5].value {
            error = "the hermes passwords do not match".to_string();
            // Emptying it puts the cursor back on it.
            fields[5].value.clear();
        } else {
            break;
        }
    }

    notice(terminal, "Creating the encrypted mailbox...")?;
    let [email, imap, smtp, mail_password, password, _] = fields;
    let mut config = Config::new(
        email.value.trim().to_string(),
        imap.value.trim().to_string(),
        smtp.value.trim().to_string(),
        &password.value,
    )?;
    let key = config.unlock(&password.value)?;
    let store = Store::open(&config.db_path()?, &key)?;
    store.set_setting("imap_password", &mail_password.value)?;
    config.save()?;
    config.password = mail_password.value;
    Ok(Some((config, key, store)))
}

/// A centered box of the given size.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn notice(terminal: &mut DefaultTerminal, text: &str) -> Result<()> {
    terminal.draw(|frame| {
        let area = centered(frame.area(), 60, 3);
        frame.render_widget(Clear, frame.area());
        frame.render_widget(Line::from(text).centered(), area.inner(Margin::new(0, 1)));
    })?;
    Ok(())
}

/// Shows `fields` as a form and edits them in place. Returns false if the
/// user backed out with Esc or Ctrl-C.
fn form(
    terminal: &mut DefaultTerminal,
    title: &str,
    fields: &mut [Field],
    error: &str,
) -> Result<bool> {
    let mut focus = fields
        .iter()
        .position(|f| f.value.is_empty())
        .unwrap_or_default();
    loop {
        terminal.draw(|frame| {
            let area = centered(frame.area(), 64, fields.len() as u16 * 3 + 4);
            frame.render_widget(Clear, frame.area());
            frame.render_widget(Block::bordered().title(format!(" {title} ")), area);

            let mut heights = vec![Constraint::Length(3); fields.len()];
            heights.extend([Constraint::Length(1); 2]);
            let rows = Layout::vertical(heights).split(area.inner(Margin::new(2, 1)));
            for (i, field) in fields.iter().enumerate() {
                let length = field.value.chars().count();
                let shown = if field.secret {
                    "*".repeat(length)
                } else {
                    field.value.clone()
                };
                let color = if i == focus {
                    Color::Yellow
                } else {
                    Color::DarkGray
                };
                let block = Block::bordered()
                    .title(field.label)
                    .border_style(Style::new().fg(color));
                frame.render_widget(Paragraph::new(shown).block(block), rows[i]);
                if i == focus {
                    let x = (rows[i].x + 1 + length as u16).min(rows[i].right().saturating_sub(2));
                    frame.set_cursor_position((x, rows[i].y + 1));
                }
            }
            frame.render_widget(
                Line::styled(error, Style::new().fg(Color::Red)),
                rows[fields.len()],
            );
            frame.render_widget(
                Line::styled(
                    "Tab next field   Ctrl-U clear   Enter confirm   Esc quit",
                    Style::new().fg(Color::DarkGray),
                ),
                rows[fields.len() + 1],
            );
        })?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let last = fields.len() - 1;
        match key.code {
            KeyCode::Esc => return Ok(false),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(false);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                fields[focus].value.clear();
            }
            KeyCode::Enter if focus == last => return Ok(true),
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => focus = (focus + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => focus = focus.saturating_sub(1),
            KeyCode::Backspace => {
                fields[focus].value.pop();
            }
            KeyCode::Char(c) => fields[focus].value.push(c),
            _ => {}
        }
    }
}
