//! Secrets (the Discord channel link) live in the system's password store, never in the
//! settings file: Credential Manager on Windows, the login keychain on macOS, the Secret
//! Service (GNOME Keyring, KWallet) on Linux.

/// The Discord webhook that "Send to channel" posts to.
pub const WEBHOOK: &str = "discord-webhook";

/// The stored secret, if there is one. May wait for the user to unlock the store.
pub fn get(name: &str) -> Option<String> {
    imp::get(name).filter(|s| !s.is_empty())
}

/// Stores `value` under `name`, replacing what was there.
pub fn set(name: &str, value: &str) -> Result<(), String> {
    imp::set(name, value)
}

/// Forgets the secret.
pub fn remove(name: &str) {
    imp::remove(name)
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };

    fn target(name: &str) -> Vec<u16> {
        format!("Kairo/{name}")
            .encode_utf16()
            .chain(Some(0))
            .collect()
    }

    pub fn get(name: &str) -> Option<String> {
        let t = target(name);
        let mut p: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: CredReadW hands back a buffer it allocated; it's read, then freed once.
        unsafe {
            if CredReadW(t.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) == 0 || p.is_null() {
                return None;
            }
            let c = &*p;
            let bytes = if c.CredentialBlob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
            };
            CredFree(p as *const _);
            String::from_utf8(bytes).ok()
        }
    }

    pub fn set(name: &str, value: &str) -> Result<(), String> {
        let mut t = target(name);
        let mut user: Vec<u16> = "Kairo".encode_utf16().chain(Some(0)).collect();
        let mut blob = value.as_bytes().to_vec();
        // SAFETY: every pointer in the record outlives the call; the rest is zeroed.
        let ok = unsafe {
            let mut c: CREDENTIALW = std::mem::zeroed();
            c.Type = CRED_TYPE_GENERIC;
            c.TargetName = t.as_mut_ptr();
            c.UserName = user.as_mut_ptr();
            c.CredentialBlobSize = blob.len() as u32;
            c.CredentialBlob = blob.as_mut_ptr();
            c.Persist = CRED_PERSIST_LOCAL_MACHINE;
            CredWriteW(&c, 0)
        };
        if ok == 0 {
            Err(format!(
                "Couldn't keep it in Credential Manager ({}).",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    }

    pub fn remove(name: &str) {
        let t = target(name);
        // SAFETY: a plain call with a NUL-terminated name.
        unsafe { CredDeleteW(t.as_ptr(), CRED_TYPE_GENERIC, 0) };
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use security_framework::passwords::{
        PasswordOptions, delete_generic_password, generic_password, set_generic_password,
    };

    const SERVICE: &str = "Kairo";

    pub fn get(name: &str) -> Option<String> {
        generic_password(PasswordOptions::new_generic_password(SERVICE, name))
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
    }

    pub fn set(name: &str, value: &str) -> Result<(), String> {
        set_generic_password(SERVICE, name, value.as_bytes())
            .map_err(|e| format!("Couldn't keep it in the keychain ({e})."))
    }

    pub fn remove(name: &str) {
        let _ = delete_generic_password(SERVICE, name);
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;

    use dbus_secret_service::{EncryptionType, SecretService};

    fn attrs(name: &str) -> HashMap<&str, &str> {
        HashMap::from([("application", "kairo"), ("name", name)])
    }

    pub fn get(name: &str) -> Option<String> {
        let ss = SecretService::connect(EncryptionType::Plain).ok()?;
        let found = ss.search_items(attrs(name)).ok()?;
        let item = found.unlocked.first().or(found.locked.first())?;
        item.ensure_unlocked().ok()?;
        String::from_utf8(item.get_secret().ok()?).ok()
    }

    pub fn set(name: &str, value: &str) -> Result<(), String> {
        let missing = |e: dbus_secret_service::Error| {
            format!(
                "Couldn't keep it: no password store answered ({e}). Install GNOME Keyring or KWallet."
            )
        };
        let ss = SecretService::connect(EncryptionType::Plain).map_err(missing)?;
        let c = ss.get_default_collection().map_err(missing)?;
        c.ensure_unlocked().map_err(missing)?;
        c.create_item(
            &format!("Kairo {name}"),
            attrs(name),
            value.as_bytes(),
            true,
            "text/plain",
        )
        .map_err(missing)?;
        Ok(())
    }

    pub fn remove(name: &str) {
        let Ok(ss) = SecretService::connect(EncryptionType::Plain) else {
            return;
        };
        if let Ok(found) = ss.search_items(attrs(name)) {
            for item in found.unlocked.iter().chain(found.locked.iter()) {
                let _ = item.delete();
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod imp {
    pub fn get(_: &str) -> Option<String> {
        None
    }

    pub fn set(_: &str, _: &str) -> Result<(), String> {
        Err("There's no password store on this system.".into())
    }

    pub fn remove(_: &str) {}
}

#[cfg(test)]
mod tests {
    /// Writes, reads and removes a real entry in this machine's store.
    #[test]
    #[ignore = "touches the system password store"]
    fn round_trip() {
        let name = "test-entry";
        super::set(name, "https://example.com/a").unwrap();
        assert_eq!(super::get(name).as_deref(), Some("https://example.com/a"));
        super::set(name, "https://example.com/b").unwrap();
        assert_eq!(super::get(name).as_deref(), Some("https://example.com/b"));
        super::remove(name);
        assert_eq!(super::get(name), None);
    }
}
