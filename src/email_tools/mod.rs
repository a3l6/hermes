use native_tls::TlsConnector;
use std::net::TcpStream;

#[derive(Debug, Default)]
pub struct IMAP_Data {
    id: u32,
    subject: String,
    name: String,
    mailbox: String,
    host: String,
    content: String,
}

pub struct Inbox {
    inbox: Vec<IMAP_Data>,
}

pub fn get_inbox_one(id: u32) -> Result<IMAP_Data, Box<dyn std::error::Error>> {
    let domain = "imap.gmail.com";
    let tcp_stream = TcpStream::connect((domain, 993))?;

    let tls = TlsConnector::builder().build()?;
    let tls_stream = tls.connect(domain, tcp_stream)?;

    let client = imap::Client::new(tls_stream);

    let mut imap_session = client
        .login("emen3998@gmail.com", "peic fygg uoxq tjep")
        .map_err(|e| e.0)?;

    let mailbox = imap_session.select("INBOX")?;

    let fetch_range = id.to_string();

    let messages = imap_session.fetch(fetch_range, "ENVELOPE")?;

    let mut ret: Option<IMAP_Data> = None; // No email yet

    for message in messages.iter() {
        let envelope = message
            .envelope()
            .expect("message did not have an envelope");

        if let Some(from) = envelope.from.as_ref() {
            for address in from {
                ret = Some(IMAP_Data {
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
                    content: "".to_string(),
                })
            }
        }
    }

    if ret.is_none() {
        return Err("Could not find requested email".into());
    }

    // Logout
    imap_session.logout()?;

    println!("\nDisconnected successfully");
    Ok(ret.unwrap_or_default())
}

pub fn get_inbox_all() -> Result<Inbox, Box<dyn std::error::Error>> {
    let mut inbox = Inbox { inbox: Vec::new() };

    let domain = "imap.gmail.com";
    let tcp_stream = TcpStream::connect((domain, 993))?;

    let tls = TlsConnector::builder().build()?;
    let tls_stream = tls.connect(domain, tcp_stream)?;

    let client = imap::Client::new(tls_stream);

    let mut imap_session = client
        .login("emen3998@gmail.com", "peic fygg uoxq tjep")
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
                inbox.inbox.push(IMAP_Data {
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
                    content: "".to_string(),
                })
            }
        }
    }

    // Logout
    imap_session.logout()?;

    println!("\nDisconnected successfully");
    Ok(inbox)
}
