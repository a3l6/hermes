use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "hermes", about = "A CLI/TUI email client")]
pub struct Cli {
    /// Without a command, hermes opens the TUI.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Mirror one folder, or every folder, into the local database
    Sync { folder: Option<String> },

    /// List the server's folders
    Folders,

    /// List locally stored messages, newest first
    List {
        #[arg(short, long, default_value = "INBOX")]
        folder: String,

        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },

    /// Print one message
    Read {
        uid: u32,

        #[arg(short, long, default_value = "INBOX")]
        folder: String,
    },

    /// Full-text search on the server
    Search {
        query: String,

        #[arg(short, long, default_value = "INBOX")]
        folder: String,
    },

    /// Move a message to another folder
    Mv {
        uid: u32,
        dest: String,

        #[arg(short, long, default_value = "INBOX")]
        folder: String,
    },

    /// Delete a message (moves it to the trash folder when there is one)
    Rm {
        uid: u32,

        #[arg(short, long, default_value = "INBOX")]
        folder: String,
    },

    /// Send a message
    Send {
        #[arg(long, required = true)]
        to: Vec<String>,

        #[arg(long)]
        cc: Vec<String>,

        #[arg(long)]
        bcc: Vec<String>,

        #[arg(long)]
        subject: String,

        #[arg(long)]
        body: String,
    },
}
