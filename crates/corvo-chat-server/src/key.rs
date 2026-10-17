//! The OPAQUE server key (`opaque.key`). Losing or replacing it breaks every
//! account, so it is written once and never overwritten.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::Path;

use anyhow::{Context, bail};
use corvo_chat_proto::opaque::Suite;
use opaque_ke::ServerSetup;

/// The key file inside the data directory.
pub const KEY_FILE: &str = "opaque.key";

/// Makes a new server key. Fails, leaving the file alone, if one exists.
pub fn generate(data_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let path = data_dir.join(KEY_FILE);
    let setup = ServerSetup::<Suite>::new(&mut rand_core::OsRng);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            bail!("opaque.key already exists and was left alone: overwriting it would break every account")
        }
        Err(e) => return Err(e).with_context(|| format!("writing {}", path.display())),
    };
    file.write_all(&setup.serialize())?;
    file.sync_all()?;
    Ok(())
}

/// Reads the server key.
pub fn load(data_dir: &Path) -> anyhow::Result<ServerSetup<Suite>> {
    let path = data_dir.join(KEY_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            bail!("no opaque.key in {}: run \"corvo-chat-server keygen\" first", data_dir.display())
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    ServerSetup::<Suite>::deserialize(&bytes)
        .map_err(|_| anyhow::anyhow!("{} is not a valid OPAQUE key", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_loads_and_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        generate(dir.path()).unwrap();
        load(dir.path()).unwrap();
        let err = generate(dir.path()).unwrap_err();
        assert!(err.to_string().contains("left alone"));
    }
}
