//! Where this PC's key lives: Windows Credential Manager (Keychain on Mac, Secret Service on Linux).
//! Never in a plain file, and never logged.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::{Error, Result};

const SERVICE: &str = "Sterling Quest Sync";

pub trait KeyStore: Send + Sync
{
    fn get(&self, server: &str) -> Result<Option<String>>;
    fn set(&self, server: &str, key: &str) -> Result<()>;
    fn delete(&self, server: &str) -> Result<()>;
}

/// One entry per server address, so a self-hosted server gets its own key.
fn user_for(server: &str) -> String
{
    server.trim_end_matches('/').to_ascii_lowercase()
}

pub struct SystemStore;

fn ks(e: keyring::Error) -> Error
{
    Error::Other(format!("couldn't use the system credential store: {e}"))
}

impl KeyStore for SystemStore
{
    fn get(&self, server: &str) -> Result<Option<String>>
    {
        let e = keyring::Entry::new(SERVICE, &user_for(server)).map_err(ks)?;

        match e.get_password()
        {
            Ok(k) => Ok(Some(k)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(x) => Err(ks(x)),
        }
    }

    fn set(&self, server: &str, key: &str) -> Result<()>
    {
        keyring::Entry::new(SERVICE, &user_for(server)).map_err(ks)?.set_password(key).map_err(ks)
    }

    fn delete(&self, server: &str) -> Result<()>
    {
        match keyring::Entry::new(SERVICE, &user_for(server)).map_err(ks)?.delete_credential()
        {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(x) => Err(ks(x)),
        }
    }
}

/// For tests and the end-to-end check.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl KeyStore for MemoryStore
{
    fn get(&self, server: &str) -> Result<Option<String>>
    {
        Ok(self.0.lock().unwrap().get(&user_for(server)).cloned())
    }

    fn set(&self, server: &str, key: &str) -> Result<()>
    {
        self.0.lock().unwrap().insert(user_for(server), key.to_string());
        Ok(())
    }

    fn delete(&self, server: &str) -> Result<()>
    {
        self.0.lock().unwrap().remove(&user_for(server));
        Ok(())
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn memory_store_round_trip()
    {
        let s = MemoryStore::default();

        assert_eq!(s.get("https://a").unwrap(), None);
        s.set("https://A/", "k").unwrap();
        assert_eq!(s.get("https://a").unwrap().as_deref(), Some("k"));
        s.delete("https://a").unwrap();
        assert_eq!(s.get("https://a").unwrap(), None);
    }
}
