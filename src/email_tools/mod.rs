use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use native_tls::TlsConnector;
use std::net::TcpStream;

pub mod cli;

pub enum EmailProvider {
    Google,
    Outlook,
    Custom(String),
}

pub struct UserCredentials {
    pub username: String,
    pub password: String,
}

// FUTURE::
//  let config = Config {
//  port: 3000,
//  ..Default::default()
//  };

#[derive(Debug)]
pub struct Email {
    pub id: u32,
    pub host_email: String,
    pub subject: String,
    pub name: String,
    pub mailbox: String,
    pub host: String,
    pub body: String,
}

impl Default for Email {
    fn default() -> Self {
        Email {
            id: 0,
            host_email: "".to_string(),
            subject: "".to_string(),
            name: "".to_string(),
            mailbox: "".to_string(),
            host: "".to_string(),
            body: "".to_string(),
        }
    }
}

#[derive(Debug)]
pub struct Inbox {
    inbox: Vec<Email>,
}

// Returns a full IMAP_DATA with contents
pub fn get_inbox_one(
    provider: EmailProvider,
    credentials: UserCredentials,
    id: u32,
) -> Result<Email, Box<dyn std::error::Error>> {
    let domain = "imap.gmail.com";
    let tcp_stream = TcpStream::connect((domain, 993))?;

    let tls = TlsConnector::builder().build()?;
    let tls_stream = tls.connect(domain, tcp_stream)?;

    let client = imap::Client::new(tls_stream);

    let mut imap_session = client
        .login(credentials.username, credentials.password)
        .map_err(|e| e.0)?;

    let fetch_range = id.to_string();

    imap_session.select("INBOX")?;

    let messages = imap_session.fetch(fetch_range, "(BODY[] ENVELOPE)")?;

    let mut ret: Option<Email> = None; // No email yet

    for message in messages.iter() {
        let envelope = message
            .envelope()
            .expect("message did not have an envelope");

        if let Some(from) = envelope.from.as_ref() {
            for address in from {
                ret = Some(Email {
                    id: message.message,
                    subject: envelope
                        .subject
                        .as_ref()
                        .map(|s| String::from_utf8_lossy(s).to_string())
                        .unwrap_or_else(|| "(no subj:wect)".to_string()),
                    name: address
                        .name
                        .as_ref()
                        .map(|n| String::from_utf8_lossy(n).to_string())
                        .unwrap_or_default(),
                    mailbox: address
                        .mailbox
                        .as_ref()
                        .map(|m| String::from_utf8_lossy(m).to_string())
                        .unwrap_or_default(),
                    host: address
                        .host
                        .as_ref()
                        .map(|h| String::from_utf8_lossy(h).to_string())
                        .unwrap_or_default(),
                    body: message
                        .body()
                        .as_ref()
                        .map(|b| String::from_utf8_lossy(b).to_string())
                        .unwrap_or_default(),
                    ..Default::default()
                })
            }
        }
    }

    if ret.is_none() {
        return Err("Could not find requested email".into());
    }

    println!("\nDisconnected successfully");
    Ok(ret.unwrap_or_default())
}

pub fn get_inbox_all(
    provider: EmailProvider,
    credentials: UserCredentials,
) -> Result<Inbox, Box<dyn std::error::Error>> {
    let mut inbox = Inbox { inbox: Vec::new() };

    let domain = "imap.gmail.com";
    let tcp_stream = TcpStream::connect((domain, 993))?;

    let tls = TlsConnector::builder().build()?;
    let tls_stream = tls.connect(domain, tcp_stream)?;

    let client = imap::Client::new(tls_stream);

    let mut imap_session = client
        .login(credentials.username, credentials.password)
        .map_err(|e| e.0)?;

    let mailbox = imap_session.select("INBOX")?;

    let total_messages = mailbox.exists;
    println!("Total messages in inbox: {}", total_messages);

    let fetch_range = if total_messages > 0 {
        format!("1:{}", total_messages)
    } else {
        println!("No messages in inbox");
        return Ok(inbox);
    };

    let messages = imap_session.fetch(fetch_range, "ENVELOPE")?;

    println!("Fetched {} messages", messages.len());

    for message in messages.iter() {
        let envelope = message
            .envelope()
            .expect("message did not have an envelope");

        if let Some(from) = envelope.from.as_ref() {
            for address in from {
                inbox.inbox.push(Email {
                    id: message.message,
                    subject: envelope
                        .subject
                        .as_ref()
                        .map(|s| String::from_utf8_lossy(s).to_string())
                        .unwrap_or_else(|| "(no subj:wect)".to_string()),
                    name: address
                        .name
                        .as_ref()
                        .map(|n| String::from_utf8_lossy(n).to_string())
                        .unwrap_or_default(),
                    mailbox: address
                        .mailbox
                        .as_ref()
                        .map(|m| String::from_utf8_lossy(m).to_string())
                        .unwrap_or_default(),
                    host: address
                        .host
                        .as_ref()
                        .map(|h| String::from_utf8_lossy(h).to_string())
                        .unwrap_or_default(),
                    body: "".to_string(),
                    ..Default::default()
                })
            }
        }
    }

    imap_session.logout()?;

    println!("\nDisconnected successfully");
    Ok(inbox)
}

fn build_email(mailbox: String, host: String) -> String {
    return format!("{mailbox}@{host}");
}

pub fn send_email(
    email: Email,
    credentials: UserCredentials,
) -> Result<(), Box<dyn std::error::Error>> {
    let email = Message::builder()
        .from(email.host_email.parse()?)
        .to(build_email(email.mailbox, email.host).parse()?)
        .subject(email.subject)
        .header(ContentType::TEXT_PLAIN)
        .body(String::from(email.body))?;

    let creds = Credentials::new(
        credentials.username.to_owned(),
        credentials.password.to_owned(),
    );

    let mailer = SmtpTransport::relay("smtp.gmail.com")?
        .credentials(creds)
        .build();

    match mailer.send(&email) {
        Ok(_) => println!("Email sent successfully!"),
        Err(e) => eprintln!("Could not send email: {}", e),
    }

    Ok(())
}
