use crate::email_tools::{Email, get_inbox_all, UserCredentials, EmailProvider, Inbox};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
    widgets::ListState,
};
use std::io;

pub fn run_tui(
    provider: EmailProvider,
    credentials: UserCredentials,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Load inbox
    let inbox: Inbox = get_inbox_all(provider, credentials.clone())
        .unwrap_or(Inbox { inbox: vec![] });

    let mut selected_index: usize = 0;
    let mut view_email: Option<Email> = None;
    let mut list_state = ListState::default();
    list_state.select(Some(selected_index));

    loop {
        terminal.draw(|f| {
            let size = f.size();

            // Layout: left panel = inbox, right panel = email view
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                .split(size);

            // Inbox panel
            let items: Vec<ListItem> = inbox
                .inbox
                .iter()
                .map(|e| ListItem::new(format!("{}: {}", e.from, e.subject)))
                .collect();

            let list = List::new(items)
                .block(Block::default().title("Inbox").borders(Borders::ALL))
                .highlight_style(Style::default().bg(Color::Blue));

            list_state.select(Some(selected_index));
            f.render_stateful_widget(list, chunks[0], &mut list_state);

            // Email view panel
            let paragraph = if let Some(email) = &view_email {
                Paragraph::new(format!(
                    "From: {}\nTo: {:?}\nCC: {:?}\nSubject: {}\nDate: {}\n\n{}",
                    email.from, email.to, email.cc, email.subject, email.date, email.body
                ))
                .block(Block::default().title("Email").borders(Borders::ALL))
            } else {
                Paragraph::new("Press Enter to view email")
                    .block(Block::default().title("Email").borders(Borders::ALL))
            };

            f.render_widget(paragraph, chunks[1]);
        })?;

        // Handle input
        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Down => {
                    if selected_index + 1 < inbox.inbox.len() {
                        selected_index += 1;
                        list_state.select(Some(selected_index));
                    }
                }
                KeyCode::Up => {
                    if selected_index > 0 {
                        selected_index -= 1;
                        list_state.select(Some(selected_index));
                    }
                }
                KeyCode::Enter => {
                    view_email = inbox.inbox.get(selected_index).cloned();
                }
                KeyCode::Esc => {
                    view_email = None;
                }
                _ => {}
            }
        }
    }

    // Cleanup
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}
