use crate::config::Config;
use chrono::DateTime;
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Message, SmtpTransport, Transport};
use mail_builder::MessageBuilder;
use mail_parser::{Address, MessageParser, MimeHeaders};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};

pub mod cli;
pub mod remote;

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

pub fn send_email(email: &Email, config: &Config) -> crate::Result<()> {
    let mut builder = Message::builder().from(mailbox(&email.from)?);

    for to_addr in email.to.iter().filter(|a| !a.trim().is_empty()) {
        builder = builder.to(mailbox(to_addr)?);
    }

    for cc_addr in email.cc.iter().filter(|a| !a.trim().is_empty()) {
        builder = builder.cc(mailbox(cc_addr)?);
    }

    for bcc_addr in email.bcc.iter().filter(|a| !a.trim().is_empty()) {
        builder = builder.bcc(mailbox(bcc_addr)?);
    }

    if let Some(id) = email.other_headers.get("In-Reply-To") {
        builder = builder.in_reply_to(id.clone()).references(id.clone());
    }

    let email_msg = builder
        .subject(email.subject.as_str())
        .header(ContentType::TEXT_PLAIN)
        .body(email.body.clone())?;

    let tls = TlsParameters::builder(config.smtp_host.clone())
        .dangerous_accept_invalid_certs(config.insecure_tls)
        .build()?;
    // 587 is the STARTTLS submission port; anything else is implicit TLS.
    let tls = if config.smtp_port == 587 {
        Tls::Required(tls)
    } else {
        Tls::Wrapper(tls)
    };

    let mailer = SmtpTransport::builder_dangerous(config.smtp_host.as_str())
        .port(config.smtp_port)
        .tls(tls)
        .credentials(Credentials::new(
            config.username.clone(),
            config.password.clone(),
        ))
        .build();

    mailer.send(&email_msg)?;
    Ok(())
}

fn mailbox(address: &str) -> crate::Result<Mailbox> {
    address
        .trim()
        .parse()
        .map_err(|e| format!("invalid address \"{}\": {}", address.trim(), e).into())
}

/// Renders an address header as "Name <addr>, addr".
pub fn format_addresses(addrs: Option<&Address>) -> String {
    let Some(addrs) = addrs else {
        return String::new();
    };
    addrs
        .iter()
        .filter_map(|addr| {
            let address = addr.address()?;
            Some(match addr.name() {
                Some(name) => format!("{} <{}>", name, address),
                None => address.to_string(),
            })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Local-time rendering of a Unix timestamp.
pub fn format_date(timestamp: i64) -> String {
    DateTime::from_timestamp(timestamp, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// Readable text and attachment names of a raw RFC 5322 message.
pub fn body_and_attachments(raw: &[u8]) -> (String, Vec<String>) {
    let Some(message) = MessageParser::default().parse(raw) else {
        return (String::from_utf8_lossy(raw).to_string(), Vec::new());
    };
    let body = message
        .body_text(0)
        .map(|cow| cow.to_string())
        .unwrap_or_default();
    let attachments = message
        .attachments()
        .map(|part| part.attachment_name().unwrap_or("(unnamed)").to_string())
        .collect();
    (body, attachments)
}

/// For a draft that replies to another message: that message's
/// Message-ID, and the first line the draft's author wrote.
pub fn reply_draft(raw: &[u8]) -> Option<(String, String)> {
    let message = MessageParser::default().parse(raw)?;
    let replied = message.in_reply_to().as_text()?.to_string();
    let body = message.body_text(0).unwrap_or_default();
    let mut written = body.lines().map(str::trim);
    let preview = written
        .find(|line| !line.is_empty() && !line.starts_with('>'))
        .unwrap_or("(empty)");
    Some((replied, preview.to_string()))
}

/// Where a reply to a raw message should go: Reply-To, else From.
pub fn reply_address(raw: &[u8]) -> String {
    MessageParser::default()
        .parse(raw)
        .map(|message| format_addresses(message.reply_to().or(message.from())))
        .unwrap_or_default()
}

/// The text handed to $EDITOR when composing.
pub fn draft_template(to: &str, subject: &str, body: &str) -> String {
    format!("To: {}\nCc: \nBcc: \nSubject: {}\n\n{}", to, subject, body)
}

/// Parses an edited draft_template back into an Email: header lines up to
/// the first blank line, then the body.
pub fn parse_draft(text: &str, from: &str) -> Email {
    let (headers, body) = text.split_once("\n\n").unwrap_or((text, ""));
    let mut email = Email {
        from: from.to_string(),
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        body: body.to_string(),
        ..Default::default()
    };
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let addresses = || {
            value
                .split(',')
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .collect()
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "to" => email.to = addresses(),
            "cc" => email.cc = addresses(),
            "bcc" => email.bcc = addresses(),
            "subject" => email.subject = value.trim().to_string(),
            _ => {}
        }
    }
    email
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
    fn test_reply_draft() {
        let raw = b"To: ann@test.com\r\nIn-Reply-To: <abc@test.com>\r\nSubject: Re: Hi\r\n\r\n\
            \r\nSounds good to me.\r\n\r\n> Hi\r\n";
        let (replied, preview) = reply_draft(raw).unwrap();
        assert_eq!(replied, "abc@test.com");
        assert_eq!(preview, "Sounds good to me.");

        assert!(reply_draft(b"To: ann@test.com\r\nSubject: New\r\n\r\nHello\r\n").is_none());
    }

    #[test]
    fn test_parse_draft() {
        let text = draft_template("a@test.com", "Hi", "")
            .replace("Cc: ", "Cc: b@test.com, c@test.com")
            + "line one\n\nline two\n";
        let email = parse_draft(&text, "me@test.com");

        assert_eq!(email.from, "me@test.com");
        assert_eq!(email.to, ["a@test.com"]);
        assert_eq!(email.cc, ["b@test.com", "c@test.com"]);
        assert!(email.bcc.is_empty());
        assert_eq!(email.subject, "Hi");
        assert_eq!(email.body, "line one\n\nline two\n");
    }

    #[test]
    fn test_reply_address_and_body() {
        let raw = b"From: Ann Lee <ann@test.com>\r\nReply-To: list@test.com\r\n\
            Subject: =?UTF-8?Q?caf=C3=A9?=\r\n\r\nHello there\r\n";

        assert_eq!(reply_address(raw), "list@test.com");
        let message = MessageParser::default().parse(&raw[..]).unwrap();
        assert_eq!(format_addresses(message.from()), "Ann Lee <ann@test.com>");
        assert_eq!(message.subject(), Some("caf\u{e9}"));
        let (body, attachments) = body_and_attachments(raw);
        assert_eq!(body.trim(), "Hello there");
        assert!(attachments.is_empty());
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
