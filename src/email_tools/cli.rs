
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "hermes")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    Inbox {
        #[command(subcommand)]
        command: InboxCommands,
    },

    Send {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: Vec<String>,
        #[arg(long)]
        subject: String,
        #[arg(long)]
        body: String,
    },

    /// Launch the TUI (Neomutt-style interface)
    Ui,
}

#[derive(Subcommand, Debug)]
pub enum InboxCommands {
    One { id: u32 },
    All,
}

