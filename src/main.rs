mod email_tools;

use clap::Parser;
use email_tools::cli::{Cli, Commands, InboxCommands};
use email_tools::{
    Email, EmailProvider, UserCredentials, get_inbox_all, get_inbox_one, send_email,
};

fn main() {
    let cli = Cli::parse();

    // TEMP: credentials (you should load these from env vars later)
    let credentials = UserCredentials::new(
        std::env::var("EMAIL_USERNAME").expect("EMAIL_USERNAME not set"), 
        std::env::var("EMAIL_PASSWORD").expect("EMAIL_PASSWORD not set"))

    let provider = EmailProvider::Google;

    match cli.command {
        Commands::Inbox { command } => match command {
            InboxCommands::One { id } => match get_inbox_one(provider, credentials, id) {
                Ok(email) => println!("{:#?}", email),
                Err(e) => {
                    eprintln!("Could not retrieve message");
                    eprintln!("{}", e);
                }
            },

            InboxCommands::All => match get_inbox_all(provider, credentials) {
                Ok(inbox) => println!("{:#?}", inbox),
                Err(e) => {
                    eprintln!("Could not retrieve inbox");
                    eprintln!("{}", e);
                }
            },
        },

        Commands::Send {
            from,
            to,
            subject,
            body,
            ..
        } => {
            let mailbox = to.into_iter().next().expect("No recipient provided");

            let email = Email {
                host_email: from,
                mailbox,
                subject,
                body,
                ..Default::default()
            };
            if let Err(e) = send_email(email, credentials) {
                eprintln!("Failed to send email");
                eprintln!("{}", e);
            }
        }
    }
}
