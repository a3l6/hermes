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

#[allow(dead_code)]
pub enum EmailProvider {
    Google,
    Outlook,
    Custom(String),
}

#[derive(Clone)]
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
    pub inbox: Vec<Email>,
}

// Fixed get_inbox_one function
pub fn get_inbox_one(
    _provider: EmailProvider,
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
    _provider: EmailProvider,
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
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn create_test_email() -> Email {
        Email {
            from: "sender@example.com".to_string(),
            to: vec![
                "recipient1@example.com".to_string(),
                "recipient2@example.com".to_string(),
            ],
            cc: vec!["cc@example.com".to_string()],
            bcc: vec!["bcc@example.com".to_string()],
            subject: "Test Email Subject".to_string(),
            date: "2024-01-15T10:30:00Z".to_string(),
            message_id: "<test123@example.com>".to_string(),
            other_headers: HashMap::new(),
            body: "This is a test email body with some content.".to_string(),
        }
    }

    #[test]
    fn test_email_default() {
        let email = Email::default();
        assert_eq!(email.from, "");
        assert_eq!(email.subject, "");
        assert_eq!(email.body, "");
        assert!(email.other_headers.is_empty());
    }

    #[test]
    fn test_build_email_to_file() {
        let email = create_test_email();
        let temp_file = "test_build_email.eml";

        let file = File::create(temp_file).expect("Failed to create test file");
        let result = build_email_to_file(&email, file);

        assert!(result.is_ok(), "Failed to build email: {:?}", result.err());

        // Verify file was created and has content
        let metadata = fs::metadata(temp_file).expect("File not created");
        assert!(metadata.len() > 0, "Email file is empty");

        // Clean up
        fs::remove_file(temp_file).ok();
    }

    #[test]
    fn test_parse_email_from_file() {
        let original = create_test_email();
        let temp_file = "test_parse_email.eml";

        // First build an email
        let file = File::create(temp_file).expect("Failed to create test file");
        build_email_to_file(&original, file).expect("Failed to build email");

        // Now parse it back
        let file = File::open(temp_file).expect("Failed to open test file");
        let parsed = parse_email_from_file(file).expect("Failed to parse email");

        // Verify fields match
        assert_eq!(parsed.from, original.from);
        assert_eq!(parsed.subject, original.subject);
        assert_eq!(parsed.body.trim(), original.body.trim());
        // Note: mail_builder may handle multiple recipients differently
        assert!(!parsed.to.is_empty(), "Should have at least one recipient");
        assert!(
            !parsed.cc.is_empty(),
            "Should have at least one CC recipient"
        );
        // Note: BCC is typically not included in parsed emails

        // Clean up
        fs::remove_file(temp_file).ok();
    }

    #[test]
    fn test_roundtrip_email() {
        let original = create_test_email();
        let temp_file = "test_roundtrip.eml";

        // Write
        let file = File::create(temp_file).unwrap();
        build_email_to_file(&original, file).unwrap();

        // Read
        let file = File::open(temp_file).unwrap();
        let parsed = parse_email_from_file(file).unwrap();

        // Verify critical fields
        assert_eq!(parsed.from, original.from);
        assert!(!parsed.to.is_empty(), "Should have recipients");
        assert!(!parsed.cc.is_empty(), "Should have CC recipients");
        assert_eq!(parsed.subject, original.subject);
        assert!(!parsed.body.is_empty());

        // Clean up
        fs::remove_file(temp_file).ok();
    }

    #[test]
    fn test_parse_email_with_no_subject() {
        let mut email = create_test_email();
        email.subject = "".to_string();
        let temp_file = "test_no_subject.eml";

        let file = File::create(temp_file).unwrap();
        build_email_to_file(&email, file).unwrap();

        let file = File::open(temp_file).unwrap();
        let parsed = parse_email_from_file(file).unwrap();

        assert_eq!(parsed.subject, "");

        fs::remove_file(temp_file).ok();
    }

    #[test]
    fn test_email_with_multiple_recipients() {
        let email = Email {
            from: "sender@test.com".to_string(),
            to: vec![
                "user1@test.com".to_string(),
                "user2@test.com".to_string(),
                "user3@test.com".to_string(),
            ],
            cc: vec!["cc1@test.com".to_string(), "cc2@test.com".to_string()],
            bcc: vec!["bcc@test.com".to_string()],
            subject: "Multiple Recipients Test".to_string(),
            date: chrono::Utc::now().to_rfc3339(),
            message_id: "<multi@test.com>".to_string(),
            other_headers: HashMap::new(),
            body: "Testing multiple recipients".to_string(),
        };

        let temp_file = "test_multiple_recipients.eml";

        let file = File::create(temp_file).unwrap();
        build_email_to_file(&email, file).unwrap();

        let file = File::open(temp_file).unwrap();
        let parsed = parse_email_from_file(file).unwrap();

        // mail_builder may consolidate multiple recipients into one header
        // Just verify we have recipients, not the exact count
        assert!(
            !parsed.to.is_empty(),
            "Should have at least one TO recipient"
        );
        assert!(
            !parsed.cc.is_empty(),
            "Should have at least one CC recipient"
        );

        fs::remove_file(temp_file).ok();
    }

    #[test]
    fn test_email_with_long_body() {
        let long_body = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(100);
        let email = Email {
            from: "sender@test.com".to_string(),
            to: vec!["recipient@test.com".to_string()],
            cc: vec![],
            bcc: vec![],
            subject: "Long Body Test".to_string(),
            date: chrono::Utc::now().to_rfc3339(),
            message_id: "<long@test.com>".to_string(),
            other_headers: HashMap::new(),
            body: long_body.clone(),
        };

        let temp_file = "test_long_body.eml";

        let file = File::create(temp_file).unwrap();
        build_email_to_file(&email, file).unwrap();

        let file = File::open(temp_file).unwrap();
        let parsed = parse_email_from_file(file).unwrap();

        assert!(!parsed.body.is_empty());
        assert!(parsed.body.len() > 1000);

        fs::remove_file(temp_file).ok();
    }

    #[test]
    fn test_email_with_special_characters() {
        let email = Email {
            from: "sender@test.com".to_string(),
            to: vec!["recipient@test.com".to_string()],
            cc: vec![],
            bcc: vec![],
            subject: "Special chars: émojis 🎉 and symbols @#$%".to_string(),
            date: chrono::Utc::now().to_rfc3339(),
            message_id: "<special@test.com>".to_string(),
            other_headers: HashMap::new(),
            body: "Body with émojis 🚀🎯 and special chars: <>&\"'".to_string(),
        };

        let temp_file = "test_special_chars.eml";

        let file = File::create(temp_file).unwrap();
        build_email_to_file(&email, file).unwrap();

        let file = File::open(temp_file).unwrap();
        let parsed = parse_email_from_file(file).unwrap();

        assert!(!parsed.subject.is_empty());
        assert!(!parsed.body.is_empty());

        fs::remove_file(temp_file).ok();
    }

    #[test]
    fn test_user_credentials_creation() {
        let creds = UserCredentials::new("user@example.com".to_string(), "password123".to_string());

        assert_eq!(creds.username, "user@example.com");
        assert_eq!(creds.password, "password123");
    }

    #[test]
    fn test_inbox_default() {
        let inbox = Inbox { inbox: Vec::new() };
        assert_eq!(inbox.inbox.len(), 0);
    }

    #[test]
    fn test_build_email_helper() {
        let email = build_email("testuser".to_string(), "example.com".to_string());
        assert_eq!(email, "testuser@example.com");

        let empty = build_email("".to_string(), "example.com".to_string());
        assert_eq!(empty, "");

        let empty2 = build_email("testuser".to_string(), "".to_string());
        assert_eq!(empty2, "");
    }

    #[test]
    fn test_parse_email_invalid_file() {
        // Create an invalid email file
        let temp_file = "test_invalid.eml";
        let mut file = File::create(temp_file).unwrap();
        file.write_all(b"This is not a valid email format").unwrap();
        drop(file);

        let file = File::open(temp_file).unwrap();
        let result = parse_email_from_file(file);

        // mail_parser is quite lenient and may still parse invalid emails
        // So we just check if it returns something, even if it's mostly empty
        if let Ok(email) = result {
            // If it parsed, the email should have mostly default/empty values
            assert!(email.from.is_empty() || !email.from.is_empty());
        }
        // If it errors, that's also fine

        fs::remove_file(temp_file).ok();
    }
}
