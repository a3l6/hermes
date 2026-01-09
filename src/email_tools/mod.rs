use chrono::DateTime;
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use mail_builder::MessageBuilder;
use mail_parser::MessageParser;
use native_tls::TlsConnector;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;

pub mod cli;

pub enum EmailProvider {
    Google,
    Outlook,
    Custom(String),
}

pub struct UserCredentials {
    username: String,
    password: String,
}

impl UserCredentials {
    pub fn new(username: String, password: String) -> UserCredentials {
        return UserCredentials { username, password };
    }
}

// FUTURE::
//  let config = Config {
//  port: 3000,
//  ..Default::default()
//  };

/*#[derive(Debug)]
pub struct Email {
    pub id: u32,
    pub host_email: String,
    pub subject: String,
    pub name: String,
    pub mailbox: String,
    pub host: String,
    pub body: String,
}*/

#[derive(Debug, Clone)]
pub struct Email {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub date: String,
    pub message_id: String,
    pub other_headers: HashMap<String, String>,
    pub body: String,
}

impl Default for Email {
    fn default() -> Self {
        Email {
            from: "".to_string(),
            to: vec!["".to_string()],
            cc: vec!["".to_string()],
            bcc: vec!["".to_string()],
            subject: "".to_string(),
            date: "".to_string(),
            message_id: "0".to_string(),
            other_headers: HashMap::new(),
            body: "".to_string(),
        }
    }
}

#[derive(Debug)]
pub struct Inbox {
    inbox: Vec<Email>,
}

// Fixed get_inbox_one function
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
        .login(credentials.username.clone(), credentials.password)
        .map_err(|e| e.0)?;

    let fetch_range = id.to_string();

    imap_session.select("INBOX")?;

    let messages = imap_session.fetch(fetch_range, "(BODY[] ENVELOPE)")?;

    let mut ret: Option<Email> = None;

    for message in messages.iter() {
        let envelope = message
            .envelope()
            .expect("message did not have an envelope");

        let from = envelope
            .from
            .as_ref()
            .and_then(|addrs| addrs.first())
            .map(|addr| {
                build_email(
                    addr.mailbox
                        .as_ref()
                        .map(|m| String::from_utf8_lossy(m).to_string())
                        .unwrap_or_default(),
                    addr.host
                        .as_ref()
                        .map(|h| String::from_utf8_lossy(h).to_string())
                        .unwrap_or_default(),
                )
            })
            .unwrap_or_default();

        let to = envelope
            .to
            .as_ref()
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(|addr| {
                        let mailbox = addr.mailbox.as_ref()?;
                        let host = addr.host.as_ref()?;
                        Some(format!(
                            "{}@{}",
                            String::from_utf8_lossy(mailbox),
                            String::from_utf8_lossy(host)
                        ))
                    })
                    .collect()
            })
            .unwrap_or_else(|| vec![credentials.username.clone()]);

        let cc = envelope
            .cc
            .as_ref()
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(|addr| {
                        let mailbox = addr.mailbox.as_ref()?;
                        let host = addr.host.as_ref()?;
                        Some(format!(
                            "{}@{}",
                            String::from_utf8_lossy(mailbox),
                            String::from_utf8_lossy(host)
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let bcc = envelope
            .bcc
            .as_ref()
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(|addr| {
                        let mailbox = addr.mailbox.as_ref()?;
                        let host = addr.host.as_ref()?;
                        Some(format!(
                            "{}@{}",
                            String::from_utf8_lossy(mailbox),
                            String::from_utf8_lossy(host)
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        ret = Some(Email {
            from,
            to,
            cc,
            bcc,
            subject: envelope
                .subject
                .as_ref()
                .map(|s| String::from_utf8_lossy(s).to_string())
                .unwrap_or_else(|| "(no subject)".to_string()),
            date: envelope
                .date
                .as_ref()
                .map(|d| String::from_utf8_lossy(d).to_string())
                .unwrap_or_default(),
            message_id: envelope
                .message_id
                .as_ref()
                .map(|id| String::from_utf8_lossy(id).to_string())
                .unwrap_or_else(|| message.message.to_string()),
            other_headers: HashMap::new(),
            body: message
                .body()
                .map(|b| String::from_utf8_lossy(b).to_string())
                .unwrap_or_default(),
        });
    }

    imap_session.logout()?;

    if ret.is_none() {
        return Err("Could not find requested email".into());
    }

    println!("\nDisconnected successfully");
    Ok(ret.unwrap())
}

// Fixed get_inbox_all function
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
        .login(credentials.username.clone(), credentials.password)
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

        let from = envelope
            .from
            .as_ref()
            .and_then(|addrs| addrs.first())
            .map(|addr| {
                build_email(
                    addr.mailbox
                        .as_ref()
                        .map(|m| String::from_utf8_lossy(m).to_string())
                        .unwrap_or_default(),
                    addr.host
                        .as_ref()
                        .map(|h| String::from_utf8_lossy(h).to_string())
                        .unwrap_or_default(),
                )
            })
            .unwrap_or_default();

        let to = envelope
            .to
            .as_ref()
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(|addr| {
                        let mailbox = addr.mailbox.as_ref()?;
                        let host = addr.host.as_ref()?;
                        Some(format!(
                            "{}@{}",
                            String::from_utf8_lossy(mailbox),
                            String::from_utf8_lossy(host)
                        ))
                    })
                    .collect()
            })
            .unwrap_or_else(|| vec![credentials.username.clone()]);

        let cc = envelope
            .cc
            .as_ref()
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(|addr| {
                        let mailbox = addr.mailbox.as_ref()?;
                        let host = addr.host.as_ref()?;
                        Some(format!(
                            "{}@{}",
                            String::from_utf8_lossy(mailbox),
                            String::from_utf8_lossy(host)
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let bcc = envelope
            .bcc
            .as_ref()
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(|addr| {
                        let mailbox = addr.mailbox.as_ref()?;
                        let host = addr.host.as_ref()?;
                        Some(format!(
                            "{}@{}",
                            String::from_utf8_lossy(mailbox),
                            String::from_utf8_lossy(host)
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        inbox.inbox.push(Email {
            from,
            to,
            cc,
            bcc,
            subject: envelope
                .subject
                .as_ref()
                .map(|s| String::from_utf8_lossy(s).to_string())
                .unwrap_or_else(|| "(no subject)".to_string()),
            date: envelope
                .date
                .as_ref()
                .map(|d| String::from_utf8_lossy(d).to_string())
                .unwrap_or_default(),
            message_id: envelope
                .message_id
                .as_ref()
                .map(|id| String::from_utf8_lossy(id).to_string())
                .unwrap_or_else(|| message.message.to_string()),
            other_headers: HashMap::new(),
            body: String::new(), // ENVELOPE doesn't include body
        });
    }

    imap_session.logout()?;

    println!("\nDisconnected successfully");
    Ok(inbox)
}

// Helper function (assumed to exist in your code)
fn build_email(mailbox: String, host: String) -> String {
    if mailbox.is_empty() || host.is_empty() {
        return String::new();
    }
    format!("{}@{}", mailbox, host)
}

pub fn send_email(
    email: Email,
    credentials: UserCredentials,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = Message::builder().from(email.from.parse::<Mailbox>()?);

    // Add all recipients
    for to_addr in email.to {
        builder = builder.to(to_addr.parse::<Mailbox>()?);
    }

    // Add CC recipients
    for cc_addr in email.cc {
        builder = builder.cc(cc_addr.parse::<Mailbox>()?);
    }

    // Add BCC recipients
    for bcc_addr in email.bcc {
        builder = builder.bcc(bcc_addr.parse::<Mailbox>()?);
    }

    let email_msg = builder
        .subject(email.subject)
        .header(ContentType::TEXT_PLAIN)
        .body(email.body)?;

    let creds: Credentials = Credentials::new(
        credentials.username.to_owned(),
        credentials.password.to_owned(),
    );

    let mailer = SmtpTransport::relay("smtp.gmail.com")?
        .credentials(creds)
        .build();

    match mailer.send(&email_msg) {
        // Changed from &email to &email_msg
        Ok(_) => println!("Email sent successfully!"),
        Err(e) => eprintln!("Could not send email: {}", e),
    }

    Ok(())
}

/// Converts an Email struct to RFC 5322 format and writes it to a File
pub fn build_email_to_file(email: &Email, mut file: File) -> Result<(), String> {
    let timestamp = DateTime::parse_from_rfc3339(&email.date)
        .map(|dt| dt.timestamp())
        .unwrap_or_else(|_| chrono::Utc::now().timestamp());
    let mut builder = MessageBuilder::new()
        .from(email.from.as_str())
        .subject(&email.subject)
        .message_id(email.message_id.clone())
        .date(timestamp)
        .text_body(&email.body);

    for to_addr in &email.to {
        builder = builder.to(to_addr.as_str());
    }

    for cc_addr in &email.cc {
        builder = builder.cc(cc_addr.as_str());
    }

    for bcc_addr in &email.bcc {
        builder = builder.bcc(bcc_addr.as_str());
    }

    // Simply skip custom headers or use write_header if needed
    // The mail_builder crate is restrictive with custom headers
    // Most standard headers are already handled above

    let email_bytes = builder.write_to_vec().map_err(|e| e.to_string())?;
    file.write_all(&email_bytes).map_err(|e| e.to_string())?;

    Ok(())
}

pub fn parse_email_from_file(mut file: File) -> Result<Email, String> {
    let mut raw_email = Vec::new();
    file.read_to_end(&mut raw_email)
        .map_err(|e| e.to_string())?;
    let parser = MessageParser::default();
    let message = parser.parse(&raw_email).ok_or("Failed to parse email")?;
    let from = message
        .from()
        .and_then(|addrs| addrs.first())
        .and_then(|addr| addr.address())
        .unwrap_or("")
        .to_string();
    let to = message
        .to()
        .map(|addrs| {
            addrs
                .iter()
                .filter_map(|addr| addr.address())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let cc = message
        .cc()
        .map(|addrs| {
            addrs
                .iter()
                .filter_map(|addr| addr.address())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let bcc = message
        .bcc()
        .map(|addrs| {
            addrs
                .iter()
                .filter_map(|addr| addr.address())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let subject = message.subject().unwrap_or("").to_string();
    let date = message.date().map(|d| d.to_rfc3339()).unwrap_or_default();
    let message_id = message.message_id().unwrap_or("").to_string();
    let body = message
        .body_text(0)
        .map(|cow| cow.to_string()) // Convert Cow<str> to String
        .unwrap_or_default();

    Ok(Email {
        from,
        to,
        cc,
        bcc,
        subject,
        date,
        message_id,
        body,
        ..Default::default()
    })
}
