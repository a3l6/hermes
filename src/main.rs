mod email_tools;
mod ui;

use clap::Parser;
use dotenv::dotenv;
use email_tools::cli::{Cli, Commands, InboxCommands};
use email_tools::{Email, EmailProvider, UserCredentials, get_inbox_all, get_inbox_one, send_email};
use std::env;

fn main() {
    // Load environment variables from .env
    dotenv().ok();

    let cli = Cli::parse();

    let username = env::var("EMAIL_USERNAME").unwrap_or_else(|_| {
        eprintln!("EMAIL_USERNAME not set. Add it to your .env file or export it.");
        std::process::exit(1);
    });

    let password = env::var("EMAIL_PASSWORD").unwrap_or_else(|_| {
        eprintln!("EMAIL_PASSWORD not set. Add it to your .env file or export it.");
        std::process::exit(1);
    });

    let credentials = UserCredentials::new(username, password);
    let provider = EmailProvider::Google;

    match cli.command {
        Commands::Inbox { command } => match command {
            InboxCommands::One { id } => match get_inbox_one(provider, credentials.clone(), id) {
                Ok(email) => println!("{:#?}", email),
                Err(e) => eprintln!("Could not retrieve message: {}", e),
            },
            InboxCommands::All => match get_inbox_all(provider, credentials.clone()) {
                Ok(inbox) => println!("{:#?}", inbox),
                Err(e) => eprintln!("Could not retrieve inbox: {}", e),
            },
        },
        Commands::Send { from, to, subject, body } => {
            let mailbox = to.into_iter().next().expect("No recipient provided");
            let email = Email {
                from: from.clone(),
                to: vec![mailbox.clone()],
                subject,
                body,
                ..Default::default()
            };

            if let Err(e) = send_email(email, credentials.clone()) {
                eprintln!("Failed to send email: {}", e);
            }
        },
        Commands::Ui => {
            if let Err(e) = ui::run_tui(provider, credentials) {
                eprintln!("Error running UI: {}", e);
            }
        }
    }
}
