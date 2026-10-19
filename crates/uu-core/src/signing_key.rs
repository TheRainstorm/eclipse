//! User-provided signing credentials. Never embed service keys in source or binaries.
use std::{fmt, fs::File, io::Read, ops::Range, path::{Path, PathBuf}};
use zeroize::Zeroizing;
use crate::{Config, error::CoreError};

pub struct SigningKey {
    data: Zeroizing<Vec<u8>>,
    content: Range<usize>,
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SigningKey([REDACTED])")
    }
}

impl SigningKey {
    pub fn path(config: &Config) -> PathBuf {
        std::env::var_os("UU_SIGNING_KEY_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| config.at("signing-key"))
    }

    pub fn load_file(path: &Path) -> Result<Self, CoreError> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(CoreError::SigningKeyMissing),
            Err(_) => return Err(CoreError::Config("无法读取连接签名配置，请检查文件访问权限".into())),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > 4096 {
            return Err(CoreError::Config("连接签名配置应为不超过 4096 字节的本地文本文件".into()));
        }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(CoreError::Config("连接签名配置仅允许本人访问，请设置文件权限为 600".into()));
            }
        }
        let mut data = Zeroizing::new(Vec::new());
        file.take(4097).read_to_end(&mut data)?;
        Self::from_data(data)
    }

    fn from_data(data: Zeroizing<Vec<u8>>) -> Result<Self, CoreError> {
        let start = if data.starts_with(&[0xef, 0xbb, 0xbf]) { 3 } else { 0 };
        let mut end = data.len();
        while end > start && matches!(data[end - 1], b'\r' | b'\n') { end -= 1; }
        if data.len() > 4096 || !(16..=4096).contains(&(end - start))
            || !data[start..end].iter().all(u8::is_ascii_graphic)
        {
            return Err(CoreError::Config("连接签名配置格式无效，应为单行有效凭据".into()));
        }
        Ok(Self { data, content: start..end })
    }

    pub fn as_bytes(&self) -> &[u8] { &self.data[self.content.clone()] }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEST_KEY: &[u8] = b"test-only-signing-key-0000";

    fn data(bytes: &[u8]) -> Zeroizing<Vec<u8>> { Zeroizing::new(bytes.to_vec()) }

    #[test]
    fn accepts_utf8_bom_and_windows_line_endings_without_logging_contents() {
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(TEST_KEY); bytes.extend_from_slice(b"\r\n");
        let key = SigningKey::from_data(Zeroizing::new(bytes)).unwrap();
        assert_eq!(key.as_bytes(), TEST_KEY);
        assert_eq!(format!("{key:?}"), "SigningKey([REDACTED])");
    }

    #[test]
    fn rejects_empty_multiline_binary_and_oversized_values() {
        for bytes in [b"".as_slice(), b"short", b"test-only-signing\nkey-0000", b"\0test-only-signing-key-0000", &[b'x'; 4097]] {
            let error = SigningKey::from_data(data(bytes)).unwrap_err().to_string();
            assert!(!error.contains("test-only"));
        }
    }

    #[test]
    fn missing_key_is_an_explicit_configuration_error() {
        let path = std::env::temp_dir().join(format!("uu-key-missing-{}", uuid::Uuid::new_v4()));
        assert!(matches!(SigningKey::load_file(&path), Err(CoreError::SigningKeyMissing)));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_shared_permissions_and_reads_private_file() {
        use std::{io::Write, os::unix::fs::{OpenOptionsExt, PermissionsExt}};
        let path = std::env::temp_dir().join(format!("uu-key-private-{}", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
        file.write_all(TEST_KEY).unwrap(); drop(file);
        let result = (|| {
            assert_eq!(SigningKey::load_file(&path).unwrap().as_bytes(), TEST_KEY);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(SigningKey::load_file(&path).is_err());
        });
        let outcome = std::panic::catch_unwind(result);
        std::fs::remove_file(path).unwrap();
        outcome.unwrap();
    }
}
