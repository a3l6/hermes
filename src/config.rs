use crate::Result;
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Key that encrypts the local database.
pub type Key = [u8; 32];

/// Account settings, kept in `<config dir>/hermes/config.toml`. Holds no
/// secrets: the IMAP password lives in the encrypted database.
#[derive(Serialize, Deserialize)]
pub struct Config {
    pub username: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// Accept self-signed certificates (self-hosted / test servers).
    #[serde(default)]
    pub insecure_tls: bool,
    salt: String,
    /// Hash of the hermes password, used to reject a wrong one.
    password_hash: String,
    /// IMAP/SMTP password, filled in after unlocking.
    #[serde(skip)]
    pub password: String,
}

impl Config {
    pub fn new(
        username: String,
        imap_host: String,
        smtp_host: String,
        hermes_password: &str,
    ) -> Result<Config> {
        let salt = hex(&rand::random::<[u8; 16]>());
        let (_, password_hash) = derive(hermes_password, &salt)?;
        Ok(Config {
            username,
            imap_host,
            imap_port: 993,
            smtp_host,
            smtp_port: 465,
            insecure_tls: false,
            salt,
            password_hash,
            password: String::new(),
        })
    }

    pub fn load() -> Result<Option<Config>> {
        let path = path()?;
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(toml::from_str(&std::fs::read_to_string(path)?)?))
    }

    pub fn save(&self) -> Result<()> {
        let path = path()?;
        std::fs::create_dir_all(path.parent().ok_or("config path has no parent")?)?;
        std::fs::write(path, toml::to_string(self)?)?;
        Ok(())
    }

    /// Checks the hermes password against the stored hash and returns the
    /// database key derived from it.
    pub fn unlock(&self, hermes_password: &str) -> Result<Key> {
        let (key, hash) = derive(hermes_password, &self.salt)?;
        if hash != self.password_hash {
            return Err("wrong password".into());
        }
        Ok(key)
    }

    /// One database per account, under the platform data directory.
    pub fn db_path(&self) -> Result<PathBuf> {
        let dir = dirs::data_dir().ok_or("no data directory")?.join("hermes");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("{}.db", self.username)))
    }
}

fn path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .ok_or("no config directory")?
        .join("hermes/config.toml"))
}

/// One Argon2id run yields 64 bytes: the first half is the database key,
/// the second half is the hash saved for checking the password. Neither
/// half reveals the other.
fn derive(hermes_password: &str, salt: &str) -> Result<(Key, String)> {
    let mut out = [0u8; 64];
    Argon2::default()
        .hash_password_into(hermes_password.as_bytes(), salt.as_bytes(), &mut out)
        .map_err(|e| e.to_string())?;
    let mut key = [0u8; 32];
    key.copy_from_slice(&out[..32]);
    Ok((key, hex(&out[32..])))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlock_accepts_only_the_right_password() {
        let config = Config::new("a@b.c".into(), "imap".into(), "smtp".into(), "secret").unwrap();
        let key = config.unlock("secret").unwrap();
        assert_eq!(key, config.unlock("secret").unwrap());
        assert!(config.unlock("Secret").is_err());
        assert!(!config.password_hash.contains(&hex(&key)));
    }

    #[test]
    fn saved_config_holds_no_passwords() {
        let mut config =
            Config::new("a@b.c".into(), "imap".into(), "smtp".into(), "secret").unwrap();
        config.password = "imap-app-password".into();
        let text = toml::to_string(&config).unwrap();
        assert!(!text.contains("imap-app-password"));
        assert!(!text.contains("secret"));
        let loaded: Config = toml::from_str(&text).unwrap();
        assert!(loaded.unlock("secret").is_ok());
    }
}
