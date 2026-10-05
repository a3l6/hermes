mod config;
mod email_tools;
mod login;
mod store;
mod tui;

use clap::Parser;
use config::Config;
use email_tools::cli::{Cli, Commands};
use email_tools::remote::Remote;
use email_tools::{Email, body_and_attachments, format_date, send_email};
use store::{Message, Store};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn main() {
    if let Err(e) = run() {
        eprintln!("hermes: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        let mut terminal = ratatui::init();
        let result = login::run(&mut terminal).and_then(|unlocked| match unlocked {
            Some((config, key, store)) => tui::run(&mut terminal, config, key, store),
            None => Ok(()),
        });
        ratatui::restore();
        return result;
    };
    let (config, store) = unlock()?;
    execute(command, &config, &store)
}

/// Unlocks the account for a command-line subcommand: asks for the hermes
/// password on the terminal, checks it, and opens the encrypted database.
fn unlock() -> Result<(Config, Store)> {
    let setup = "no account configured yet; run `hermes` to set one up";
    let mut config = Config::load()?.ok_or(setup)?;
    // HERMES_PASSWORD lets scripts and cron jobs skip the prompt.
    let hermes_password = match std::env::var("HERMES_PASSWORD") {
        Ok(password) => password,
        Err(_) => rpassword::prompt_password("Hermes password: ")?,
    };
    let key = config.unlock(&hermes_password)?;
    let store = Store::open(&config.db_path()?, &key)?;
    config.password = store.setting("imap_password")?.ok_or(setup)?;
    Ok((config, store))
}

fn execute(command: Commands, config: &Config, store: &Store) -> Result<()> {
    match command {
        Commands::Sync { folder } => {
            let mut remote = Remote::connect(config)?;
            remote.list_folders(store)?;
            let folders = match folder {
                Some(folder) => vec![folder],
                None => store.folders()?.into_iter().map(|f| f.name).collect(),
            };
            for folder in folders {
                let missing = remote.sync(store, &folder)?;
                for chunk in missing.chunks(200) {
                    remote.fetch_headers(store, &folder, chunk)?;
                }
                let mut bodies = 0;
                loop {
                    let mut uids = store.missing_bodies(&folder, 25)?;
                    if uids.is_empty() {
                        break;
                    }
                    uids.sort_unstable();
                    remote.fetch_bodies(store, &folder, &uids)?;
                    bodies += uids.len();
                }
                println!(
                    "{}: {} new messages, {} bodies downloaded",
                    folder,
                    missing.len(),
                    bodies
                );
            }
        }

        Commands::Folders => {
            Remote::connect(config)?.list_folders(store)?;
            for folder in store.folders()? {
                println!(
                    "{:<32} {:>5} unread {:>6} stored  {}",
                    folder.name, folder.unread, folder.total, folder.role
                );
            }
        }

        Commands::List { folder, limit } => {
            for message in store.messages(&folder)?.iter().take(limit) {
                print_row(message);
            }
        }

        Commands::Read { uid, folder } => {
            let message = store
                .messages(&folder)?
                .into_iter()
                .find(|m| m.uid == uid)
                .ok_or("no such message stored locally; run `hermes sync`")?;
            if store.raw(&folder, uid)?.is_none() {
                Remote::connect(config)?.fetch_bodies(store, &folder, &[uid])?;
            }
            let raw = store.raw(&folder, uid)?.ok_or("message is gone")?;
            let (body, attachments) = body_and_attachments(&raw);
            println!("From:    {}", message.from);
            println!("To:      {}", message.to);
            if !message.cc.is_empty() {
                println!("Cc:      {}", message.cc);
            }
            println!("Date:    {}", format_date(message.date));
            println!("Subject: {}", message.subject);
            if !attachments.is_empty() {
                println!("Attachments: {}", attachments.join(", "));
            }
            println!("\n{}", body);
        }

        Commands::Search { query, folder } => {
            let mut remote = Remote::connect(config)?;
            let found = remote.search(&folder, &query)?;
            let missing = remote.sync(store, &folder)?;
            let wanted: Vec<u32> = missing.into_iter().filter(|u| found.contains(u)).collect();
            remote.fetch_headers(store, &folder, &wanted)?;
            for message in store.messages(&folder)? {
                if found.contains(&message.uid) {
                    print_row(&message);
                }
            }
        }

        Commands::Mv { uid, dest, folder } => {
            Remote::connect(config)?.move_to(store, &folder, &[uid], &dest)?;
        }

        Commands::Rm { uid, folder } => {
            Remote::connect(config)?.delete(store, &folder, &[uid])?;
        }

        Commands::Send {
            to,
            cc,
            bcc,
            subject,
            body,
        } => {
            let email = Email {
                from: config.username.clone(),
                to,
                cc,
                bcc,
                subject,
                body,
                ..Default::default()
            };
            send_email(&email, config)?;
            println!("Email sent successfully!");
        }
    }
    Ok(())
}

fn print_row(message: &Message) {
    println!(
        "{:>7} {}{} {}  {:<28.28}  {}",
        message.uid,
        if message.seen { ' ' } else { 'N' },
        if message.flagged { '*' } else { ' ' },
        format_date(message.date),
        message.from,
        message.subject
    );
}
