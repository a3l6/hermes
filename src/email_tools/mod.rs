use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use mailparse::{MailHeaderMap, parse_mail};
use native_tls::TlsConnector;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Write};
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
    pub from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: Option<String>,
    pub date: Option<String>,
    pub message_id: Option<String>,
    pub other_headers: HashMap<String, String>,
    pub body: EmailBody,
}


impl Default for Email {
    fn default() -> Self {
        Email {
            from: "".to_string(),
            to: "".to_string(),
            cc: "".to_string(),
            bcc: "".to_string(),
            subject: "".to_string(),
            date: "".to_string(),
            message_id: 0,
            other_headers: HashMap::new(),
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
    provider: EmailProvider, // not implemented yet
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
                    from: build_email(
                        address
                            .mailbox
                            .as_ref()
                            .map(|m| String::from_utf8_lossy(m).to_string())
                            .unwrap_or_default(),
                        address
                            .host
                            .as_ref()
                            .map(|h| String::from_utf8_lossy(h).to_string())
                            .unwrap_or_default(),
                    ),
                    to: credentials.username.clone(),
                    cc: unpack_cc(envelope.cc),
                    subject: envelope
                        .subject
                        .as_ref()
                        .map(|s| String::from_utf8_lossy(s).to_string())
                        .unwrap_or_else(|| "(no subj:wect)".to_string()),
                    date: envelope
                        .date
                        .as_ref()
                        .map(|d| String::from_utf8_lossy(d).to_string())
                        .unwrap_or_default(),
                    message_id: message.message,
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
                    from: build_email(
                        address
                            .mailbox
                            .as_ref()
                            .map(|m| String::from_utf8_lossy(m).to_string())
                            .unwrap_or_default(),
                        address
                            .host
                            .as_ref()
                            .map(|h| String::from_utf8_lossy(h).to_string())
                            .unwrap_or_default(),
                    ),
                    to: credentials.username.clone(),
                    cc: unpack_cc(envelope.cc),
                    subject: envelope
                        .subject
                        .as_ref()
                        .map(|s| String::from_utf8_lossy(s).to_string())
                        .unwrap_or_else(|| "(no subj:wect)".to_string()),
                    date: envelope
                        .date
                        .as_ref()
                        .map(|d| String::from_utf8_lossy(d).to_string())
                        .unwrap_or_default(),
                    message_id: message.message,
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


fn build_email_addr(addr: &imap_proto::types::Address) -> String {
    let mailbox = addr.mailbox.as_ref()
        .map(|m| String::from_utf8_lossy(m).to_string())
        .unwrap_or_default();

    let host = addr.host.as_ref()
        .map(|h| String::from_utf8_lossy(h).to_string())
        .unwrap_or_default();

    return format!("{}@{}", mailbox, host)
}


fn unpack_cc(header: Option<String>) -> Vec<String> {
    let mut cc: Vec<String> = Vec<String>::new();

    if let Some(addrs) = header.as_ref() {
        for addr in addrs {
            cc.push(build_email_addr(&addr));
        }
    }

    return cc
}

pub fn send_email(
    email: Email,
    credentials: UserCredentials,
) -> Result<(), Box<dyn std::error::Error>> {
    let email = Message::builder()
        .from(email.from?)
        .to(email.to.parse()?)
        .subject(email.subject)
        .header(ContentType::TEXT_PLAIN)
        .body(String::from(email.body))?;

    let creds: Credentials = Credentials::new(
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

pub struct EmailStorage {
    path: String,
}

impl EmailStorage {
    pub fn read_email(file: File) -> Result<Email, Box<dyn std::error::Error>> {
        let mut reader = BufReader::new(file);
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer)?;

        let parsed = parse_mail(&buffer)?;

        println!("Subject: {:?}", parsed.headers.get_first_value("Subject"));
        println!("Body: {}", parsed.get_body()?);

        Ok(())
    }
    fn write_email_to_file(path: &str) -> std::io::Result<()> {
        let email_content = concat!(
            "From: sender@example.com\r\n",
            "To: recipient@example.com\r\n",
            "Subject: Test Email\r\n",
            "Date: Mon, 15 Jan 2024 10:30:00 +0000\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: text/plain; charset=UTF-8\r\n",
            "\r\n",
            "This is the email body.\r\n"
        );

        let mut file = File::create(path)?;
        file.write_all(email_content.as_bytes())?;
        Ok(())
    }

}


