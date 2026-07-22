#![forbid(unsafe_code)]

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;
use thiserror::Error;
use zeroize::Zeroizing;

#[derive(Debug, Error)]
pub enum KeystoreError {
    #[error("keystore encryption failed")]
    Encrypt(#[source] age::EncryptError),
    #[error("keystore decryption or authentication failed")]
    Decrypt(#[source] age::DecryptError),
    #[error("keystore I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("refusing to overwrite an existing keystore")]
    Exists,
}

pub trait SignerProvider: Send + Sync {
    fn reference(&self) -> &str;
    fn public_key(&self) -> Result<String, KeystoreError>;
    fn sign_message(&self, message: &[u8]) -> Result<Vec<u8>, KeystoreError>;
}

pub fn encrypt_to_file(
    path: &Path,
    plaintext: Zeroizing<Vec<u8>>,
    passphrase: SecretString,
) -> Result<(), KeystoreError> {
    if path.exists() {
        return Err(KeystoreError::Exists);
    }
    let encryptor = age::Encryptor::with_user_passphrase(passphrase);
    let mut encrypted = Vec::new();
    {
        let mut writer =
            encryptor
                .wrap_output(&mut encrypted)
                .map_err(|source| KeystoreError::Io {
                    path: path.to_owned(),
                    source,
                })?;
        writer
            .write_all(&plaintext)
            .map_err(|source| KeystoreError::Io {
                path: path.to_owned(),
                source,
            })?;
        writer.finish().map_err(|source| KeystoreError::Io {
            path: path.to_owned(),
            source,
        })?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| KeystoreError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|source| KeystoreError::Io {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(&encrypted)
        .map_err(|source| KeystoreError::Io {
            path: path.to_owned(),
            source,
        })?;
    file.sync_all().map_err(|source| KeystoreError::Io {
        path: path.to_owned(),
        source,
    })?;
    Ok(())
}

pub fn decrypt_from_file(
    path: &Path,
    passphrase: SecretString,
) -> Result<Zeroizing<Vec<u8>>, KeystoreError> {
    let ciphertext = fs::read(path).map_err(|source| KeystoreError::Io {
        path: path.to_owned(),
        source,
    })?;
    let decryptor = age::Decryptor::new(&ciphertext[..]).map_err(KeystoreError::Decrypt)?;
    let identity = age::scrypt::Identity::new(passphrase);
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(KeystoreError::Decrypt)?;
    let mut plaintext = Zeroizing::new(Vec::new());
    reader
        .read_to_end(&mut plaintext)
        .map_err(|source| KeystoreError::Io {
            path: path.to_owned(),
            source,
        })?;
    Ok(plaintext)
}

pub fn redact(value: &str) -> String {
    if value.len() <= 8 {
        "[REDACTED]".into()
    } else {
        format!("{}…[REDACTED]", &value[..4])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(value: &str) -> SecretString {
        SecretString::from(value.to_owned())
    }

    #[test]
    fn ciphertext_does_not_contain_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signer.age");
        let secret = b"private-key-material-never-log".to_vec();
        encrypt_to_file(
            &path,
            Zeroizing::new(secret.clone()),
            pass("correct horse battery staple"),
        )
        .unwrap();
        let encrypted = fs::read(&path).unwrap();
        assert!(!encrypted.windows(secret.len()).any(|w| w == secret));
        assert_eq!(
            &*decrypt_from_file(&path, pass("correct horse battery staple")).unwrap(),
            &secret
        );
    }

    #[test]
    fn wrong_credentials_and_tampering_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signer.age");
        encrypt_to_file(
            &path,
            Zeroizing::new(vec![42; 64]),
            pass("correct-passphrase"),
        )
        .unwrap();
        assert!(decrypt_from_file(&path, pass("wrong-passphrase")).is_err());
        let mut encrypted = fs::read(&path).unwrap();
        let index = encrypted.len() - 1;
        encrypted[index] ^= 0x01;
        fs::write(&path, encrypted).unwrap();
        assert!(decrypt_from_file(&path, pass("correct-passphrase")).is_err());
    }

    #[test]
    fn redaction_never_returns_secret() {
        assert_ne!(redact("supersecretvalue"), "supersecretvalue");
    }
}
