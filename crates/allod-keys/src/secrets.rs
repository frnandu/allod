//! Secrets kept as files, never in the database (SPEC §4.3): `{data_dir}/secrets/`,
//! directory 0700, files 0600, atomic writes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

/// Well-known secret file names.
pub mod names {
    /// The recovery phrase (24 words).
    pub const RECOVERY_PHRASE: &str = "recovery_phrase";
    /// The scoped ldk-server macaroon Allod uses (hex).
    pub const LDK_SERVER_MACAROON: &str = "ldk_server.macaroon";
}

/// A directory of secret files.
#[derive(Debug, Clone)]
pub struct SecretStore {
    dir: PathBuf,
}

impl SecretStore {
    /// Uses (and creates on first write) `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        SecretStore { dir: dir.into() }
    }

    /// The directory path.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> std::io::Result<PathBuf> {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad secret name"));
        }
        Ok(self.dir.join(name))
    }

    /// True when the secret exists.
    pub fn exists(&self, name: &str) -> bool {
        self.path(name).map(|p| p.is_file()).unwrap_or(false)
    }

    /// Reads a secret as UTF-8 text (trailing whitespace trimmed).
    pub fn read(&self, name: &str) -> std::io::Result<Option<Zeroizing<String>>> {
        match fs::read(self.path(name)?) {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                let text = std::str::from_utf8(&bytes).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "secret is not utf-8")
                })?;
                Ok(Some(Zeroizing::new(text.trim_end().to_string())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Writes a secret atomically: temp file (0600), fsync, rename, fsync directory.
    pub fn write(&self, name: &str, value: &str) -> std::io::Result<()> {
        let path = self.path(name)?;
        create_private_dir(&self.dir)?;
        let tmp = self.dir.join(format!(".{name}.tmp"));
        {
            let mut file = open_private(&tmp)?;
            file.write_all(value.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        if let Ok(dir) = fs::File::open(&self.dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

/// Creates `dir` (and parents) with mode 0700 on Unix.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes `value` to `path` with the given Unix mode (atomic rename). Used for the
/// handoff files shared with ldk-server.
pub fn write_file_with_mode(path: &Path, value: &str, mode: u32) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{name}.tmp"));
    {
        let mut file = open_private(&tmp)?;
        file.write_all(value.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    fs::rename(&tmp, path)
}

fn open_private(path: &Path) -> std::io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::new(dir.path().join("secrets"));
        assert!(store.read(names::RECOVERY_PHRASE).unwrap().is_none());
        store.write(names::RECOVERY_PHRASE, "abandon about").unwrap();
        store.write(names::RECOVERY_PHRASE, "zoo zoo").unwrap();
        assert_eq!(store.read(names::RECOVERY_PHRASE).unwrap().unwrap().as_str(), "zoo zoo");
        assert!(store.exists(names::RECOVERY_PHRASE));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = fs::metadata(store.dir().join(names::RECOVERY_PHRASE)).unwrap();
            assert_eq!(file.permissions().mode() & 0o777, 0o600);
            let d = fs::metadata(store.dir()).unwrap();
            assert_eq!(d.permissions().mode() & 0o777, 0o700);
        }
        assert!(store.write("../escape", "x").is_err());
        assert!(store.write(".hidden", "x").is_err());
    }

    #[test]
    fn handoff_file_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys_mnemonic");
        write_file_with_mode(&path, "words", 0o400).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "words\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o400);
        }
    }
}
