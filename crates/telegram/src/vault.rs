//! Authenticated encryption and atomic replacement. Only ciphertext touches disk.

use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

const MAGIC: &[u8] = b"TGBS\x01";
const NONCE_SIZE: usize = 24;
const MAX_FILE_SIZE: u64 = 16 * 1024 * 1024;
pub(crate) type SecretKey = Zeroizing<[u8; 32]>;

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("protected credential storage is unavailable")]
    Keyring,
    #[error("the session encryption key is missing")]
    MissingKey,
    #[error("the encrypted session is corrupt or has an unsupported version")]
    Corrupt,
    #[error("another application instance is using this session")]
    Busy,
    #[error("could not read or atomically save the encrypted session")]
    Io,
    #[error("secure session storage is unsupported on this platform")]
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Unsupported,
}

pub(crate) trait KeyStore: Send + Sync {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, VaultError>;
    fn store(&self, key: &[u8]) -> Result<(), VaultError>;
}

struct SystemKeyStore {
    account: String,
}

impl KeyStore for SystemKeyStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, VaultError> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let entry = keyring::Entry::new("org.tgbooks.TelegramBookDownloader", &self.account)
                .map_err(|_| VaultError::Keyring)?;
            match entry.get_secret() {
                Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(VaultError::Keyring),
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = &self.account;
            Err(VaultError::Unsupported)
        }
    }

    fn store(&self, key: &[u8]) -> Result<(), VaultError> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            keyring::Entry::new("org.tgbooks.TelegramBookDownloader", &self.account)
                .map_err(|_| VaultError::Keyring)?
                .set_secret(key)
                .map_err(|_| VaultError::Keyring)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = key;
            Err(VaultError::Unsupported)
        }
    }
}

pub(crate) struct Vault {
    directory: PathBuf,
    keys: Arc<dyn KeyStore>,
    // Kept alive by pending write tasks too: a cancelled RPC cannot release
    // the lock and let another process overwrite an unfinished snapshot.
    _lock: File,
}

impl Vault {
    pub fn open_system(directory: &Path) -> Result<Arc<Self>, VaultError> {
        let directory = directory.canonicalize().map_err(|_| VaultError::Io)?;
        let digest = Sha256::digest(directory.as_os_str().as_encoded_bytes());
        let account = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        Self::open(&directory, Arc::new(SystemKeyStore { account })).map(Arc::new)
    }

    pub fn open(directory: &Path, keys: Arc<dyn KeyStore>) -> Result<Self, VaultError> {
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let lock = options
            .open(directory.join("telegram.session.lock"))
            .map_err(|_| VaultError::Io)?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => VaultError::Busy,
            TryLockError::Error(_) => VaultError::Io,
        })?;
        Ok(Self {
            directory: directory.to_owned(),
            keys,
            _lock: lock,
        })
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join("telegram.session.enc")
    }

    fn key(&self, create: bool) -> Result<SecretKey, VaultError> {
        let mut key = Zeroizing::new([0; 32]);
        if let Some(stored) = self.keys.load()? {
            if stored.len() != key.len() {
                return Err(VaultError::MissingKey);
            }
            key.copy_from_slice(&stored);
        } else if create {
            getrandom::fill(key.as_mut()).map_err(|_| VaultError::Keyring)?;
            self.keys.store(key.as_ref())?;
        } else {
            return Err(VaultError::MissingKey);
        }
        Ok(key)
    }

    pub fn read<T: DeserializeOwned>(&self) -> Result<Option<(T, SecretKey)>, VaultError> {
        let file = match File::open(self.path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(VaultError::Io),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_SIZE + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| VaultError::Io)?;
        if bytes.len() as u64 > MAX_FILE_SIZE
            || bytes.len() < MAGIC.len() + NONCE_SIZE + 16
            || !bytes.starts_with(MAGIC)
        {
            return Err(VaultError::Corrupt);
        }
        let key = self.key(false)?;
        let cipher =
            XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| VaultError::Corrupt)?;
        let mut nonce = [0; NONCE_SIZE];
        nonce.copy_from_slice(&bytes[MAGIC.len()..MAGIC.len() + NONCE_SIZE]);
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    &XNonce::from(nonce),
                    Payload {
                        msg: &bytes[MAGIC.len() + NONCE_SIZE..],
                        aad: MAGIC,
                    },
                )
                .map_err(|_| VaultError::Corrupt)?,
        );
        let state = serde_json::from_slice(&plaintext).map_err(|_| VaultError::Corrupt)?;
        Ok(Some((state, key)))
    }

    /// Missing/corrupt existing sessions are never silently replaced with a new key.
    pub fn key_for_configuration<T: DeserializeOwned>(&self) -> Result<SecretKey, VaultError> {
        match self.read::<T>()? {
            Some((_, key)) => Ok(key),
            None => self.key(true),
        }
    }

    pub fn write(&self, value: &impl Serialize, key: &SecretKey) -> Result<(), VaultError> {
        let plaintext = Zeroizing::new(serde_json::to_vec(value).map_err(|_| VaultError::Corrupt)?);
        if plaintext.len() as u64 > MAX_FILE_SIZE - 128 {
            return Err(VaultError::Io);
        }
        let cipher =
            XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| VaultError::Corrupt)?;
        let mut nonce = [0; NONCE_SIZE];
        getrandom::fill(&mut nonce).map_err(|_| VaultError::Keyring)?;
        let encrypted = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plaintext,
                    aad: MAGIC,
                },
            )
            .map_err(|_| VaultError::Corrupt)?;
        let mut temporary =
            tempfile::NamedTempFile::with_prefix_in(".telegram.session.", &self.directory)
                .map_err(|_| VaultError::Io)?;
        temporary.write_all(MAGIC).map_err(|_| VaultError::Io)?;
        temporary.write_all(&nonce).map_err(|_| VaultError::Io)?;
        temporary
            .write_all(&encrypted)
            .map_err(|_| VaultError::Io)?;
        temporary.as_file().sync_all().map_err(|_| VaultError::Io)?;
        temporary.persist(self.path()).map_err(|_| VaultError::Io)?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    pub struct MemoryKeys(pub Mutex<Option<Vec<u8>>>);

    impl KeyStore for MemoryKeys {
        fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, VaultError> {
            Ok(self
                .0
                .lock()
                .map_err(|_| VaultError::Keyring)?
                .as_ref()
                .map(|key| Zeroizing::new(key.clone())))
        }

        fn store(&self, key: &[u8]) -> Result<(), VaultError> {
            *self.0.lock().map_err(|_| VaultError::Keyring)? = Some(key.to_vec());
            Ok(())
        }
    }

    #[test]
    fn ciphertext_survives_restart_and_uses_a_fresh_nonce() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let keys = Arc::new(MemoryKeys::default());
        let vault = Vault::open(directory.path(), keys.clone())?;
        let key = vault.key_for_configuration::<String>()?;
        vault.write(&"dummy session contents", &key)?;
        let first = std::fs::read(vault.path())?;
        assert!(!String::from_utf8_lossy(&first).contains("dummy session contents"));
        vault.write(&"dummy session contents", &key)?;
        assert_ne!(first, std::fs::read(vault.path())?);
        drop(vault);
        let reopened = Vault::open(directory.path(), keys)?;
        let Some((value, _)) = reopened.read::<String>()? else {
            panic!("snapshot was not saved")
        };
        assert_eq!(value, "dummy session contents");
        Ok(())
    }

    #[test]
    fn tampering_or_a_missing_key_never_creates_replacement_plaintext()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let keys = Arc::new(MemoryKeys::default());
        let vault = Vault::open(directory.path(), keys.clone())?;
        let key = vault.key_for_configuration::<String>()?;
        vault.write(&"dummy", &key)?;
        let mut bytes = std::fs::read(vault.path())?;
        bytes[MAGIC.len() + NONCE_SIZE] ^= 1;
        std::fs::write(vault.path(), &bytes)?;
        assert!(matches!(vault.read::<String>(), Err(VaultError::Corrupt)));
        assert!(matches!(
            vault.key_for_configuration::<String>(),
            Err(VaultError::Corrupt)
        ));
        *keys.0.lock().map_err(|_| "poisoned")? = None;
        assert!(matches!(
            vault.read::<String>(),
            Err(VaultError::MissingKey)
        ));
        assert!(keys.load()?.is_none());
        assert_eq!(std::fs::read(vault.path())?, bytes);
        Ok(())
    }

    #[test]
    fn another_instance_cannot_open_the_same_session() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let keys = Arc::new(MemoryKeys::default());
        let first = Vault::open(directory.path(), keys.clone())?;
        assert!(matches!(
            Vault::open(directory.path(), keys.clone()),
            Err(VaultError::Busy)
        ));
        drop(first);
        let _second = Vault::open(directory.path(), keys)?;
        Ok(())
    }

    #[test]
    fn unavailable_os_storage_does_not_fall_back_to_disk_keys()
    -> Result<(), Box<dyn std::error::Error>> {
        struct Denied;
        impl KeyStore for Denied {
            fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, VaultError> {
                Err(VaultError::Keyring)
            }
            fn store(&self, _: &[u8]) -> Result<(), VaultError> {
                Err(VaultError::Keyring)
            }
        }
        let directory = tempfile::tempdir()?;
        let vault = Vault::open(directory.path(), Arc::new(Denied))?;
        assert!(vault.key_for_configuration::<String>().is_err());
        assert!(!vault.path().exists());
        Ok(())
    }
}
