//! Publish complete saves without truncating the previous file.

use anyhow::{Context, Result, bail, ensure};
use std::fs;
use std::io::{BufWriter, ErrorKind, Write};
use std::path::Path;

/// Follow existing symlinks and preserve Unix permission bits when replacing a file.
pub(crate) fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut dyn Write) -> Result<()>,
) -> Result<()> {
    let permissions = match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            bail!("Save destination is a directory: {}", path.display())
        }
        Ok(metadata) if !metadata.is_file() => {
            // Devices, FIFOs, and pipes (/dev/null, /dev/stdout) cannot be replaced; write
            // through them as fs::write did. O_TRUNC is ignored for FIFOs and terminals.
            let file = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(path)
                .with_context(|| format!("Failed to open {} for writing", path.display()))?;
            let mut buffered = BufWriter::new(file);
            write(&mut buffered)?;
            buffered.flush()?;
            return Ok(());
        }
        Ok(metadata) => {
            // Read-only temporary files cannot be cleaned up on Windows after an error.
            ensure!(
                !metadata.permissions().readonly(),
                "Save destination is read-only: {}",
                path.display()
            );
            // Preserve fs::write's access check without creating or truncating the file.
            drop(
                fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .with_context(|| format!("Failed to open {} for writing", path.display()))?,
            );
            Some(metadata.permissions())
        }
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let resolved = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Some(fs::canonicalize(path)?),
        Ok(_) => None,
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let path = resolved.as_deref().unwrap_or(path);
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".xfr-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(
            permissions
                .clone()
                .unwrap_or_else(|| fs::Permissions::from_mode(0o666)),
        );
    }
    let mut temporary = builder
        .tempfile_in(parent)
        .with_context(|| format!("Failed to create temporary save in {}", parent.display()))?;
    if let Some(permissions) = permissions.as_ref() {
        temporary.as_file().set_permissions(permissions.clone())?;
    }
    {
        let mut buffered = BufWriter::new(temporary.as_file_mut());
        write(&mut buffered)?;
        buffered.flush()?;
    }
    // Unix writes can clear setuid/setgid bits, so restore the saved mode after flushing.
    if let Some(permissions) = permissions {
        temporary.as_file().set_permissions(permissions)?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_write_preserves_previous_save_and_cleans_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved");
        fs::write(&path, b"previous").unwrap();
        assert!(
            atomic_write(&path, |writer| {
                writer.write_all(b"partial")?;
                anyhow::bail!("simulated write failure");
            })
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn replaces_previous_contents_completely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved");
        fs::write(&path, b"a longer previous save").unwrap();
        atomic_write(&path, |writer| Ok(writer.write_all(b"new")?)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn rejects_directory_without_touching_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"previous").unwrap();
        assert!(atomic_write(&path, |_| panic!("must reject before writing")).is_err());
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"previous");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn rejects_readonly_destination_before_creating_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved");
        fs::write(&path, b"previous").unwrap();
        let original = path.metadata().unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        let result = atomic_write(&path, |_| panic!("must reject before writing"));
        fs::set_permissions(&path, original).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn preserves_permissions_and_follows_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved");
        let link = dir.path().join("link");
        fs::write(&path, b"previous").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o4640)).unwrap();
        symlink("saved", &link).unwrap();
        atomic_write(&link, |writer| {
            let temporary = fs::read_dir(dir.path())
                .unwrap()
                .map(|entry| entry.unwrap())
                .find(|entry| entry.file_name().to_string_lossy().starts_with(".xfr-"))
                .unwrap();
            assert_eq!(
                temporary.metadata().unwrap().permissions().mode() & 0o7777,
                0o4640
            );
            Ok(writer.write_all(b"new")?)
        })
        .unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            path.metadata().unwrap().permissions().mode() & 0o7777,
            0o4640
        );

        let plain = dir.path().join("plain");
        let new = dir.path().join("new");
        fs::File::create(&plain).unwrap();
        atomic_write(&new, |writer| Ok(writer.write_all(b"new")?)).unwrap();
        assert_eq!(
            new.metadata().unwrap().permissions().mode(),
            plain.metadata().unwrap().permissions().mode()
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_destination_the_owner_cannot_write() {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses the owner permission check this regression exercises.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved");
        fs::write(&path, b"previous").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o460)).unwrap();
        let result = atomic_write(&path, |_| panic!("must reject before writing"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_character_devices() {
        atomic_write(Path::new("/dev/null"), |writer| {
            Ok(writer.write_all(b"discarded")?)
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_fifos() {
        use std::io::Read;
        use std::os::unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, OpenOptionsExt},
        };
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        // A non-blocking reader lets the writer open without a second thread.
        let mut reader = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .unwrap();
        atomic_write(&fifo, |writer| Ok(writer.write_all(b"result")?)).unwrap();
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        assert_eq!(received, b"result");
        assert!(fifo.symlink_metadata().unwrap().file_type().is_fifo());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_broken_symlink_without_replacing_it() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink("missing", &link).unwrap();
        assert!(atomic_write(&link, |_| panic!("must reject before writing")).is_err());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(!dir.path().join("missing").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
