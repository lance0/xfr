//! Publish complete saves without truncating the previous file.

use anyhow::Result;
use std::fs;
use std::io::{self, BufWriter, ErrorKind, Write};
use std::path::Path;
use tracing::warn;

/// Save `contents` to `path` so a failed save never truncates an existing regular file.
///
/// Regular files are replaced by renaming a synced temporary file over them, following
/// existing symlinks and preserving Unix permission bits. A writable file in a directory that
/// is not writable is overwritten in place instead, with a warning that the save is not atomic.
/// Devices, FIFOs, and pipes such as `/dev/null` or `/dev/stdout` are written in place.
/// Directories are rejected.
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    write_with(path, |writer| Ok(writer.write_all(contents)?))
}

/// Check that [`atomic_write`] can save to `path`, without creating or truncating anything.
///
/// The parent directory must exist, the destination must not be a directory, and an existing
/// regular file must be writable, or the directory writable for a new file. Devices, FIFOs,
/// and pipes are not checked. Errors match the ones a failed save reports.
pub fn check_destination(path: &Path) -> Result<()> {
    if let Destination::New = inspect(path)? {
        let parent = parent_dir(path);
        match fs::metadata(parent) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(cannot_save(path, ErrorKind::NotADirectory, "directory")),
            Err(error) => return Err(cannot_save(path, error.kind(), "directory")),
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let parent = std::ffi::CString::new(parent.as_os_str().as_bytes())?;
            // SAFETY: `parent` is a valid NUL-terminated path for the duration of the call.
            if unsafe { libc::access(parent.as_ptr(), libc::W_OK | libc::X_OK) } != 0 {
                let kind = io::Error::last_os_error().kind();
                return Err(cannot_save(path, kind, "directory"));
            }
        }
    }
    Ok(())
}

enum Destination {
    /// A device, FIFO, or pipe, written in place.
    Special,
    /// A writable regular file, replaced atomically.
    File(fs::Permissions),
    /// Nothing exists at the path yet.
    New,
}

fn inspect(path: &Path) -> Result<Destination> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            Err(cannot_save(path, ErrorKind::IsADirectory, "file"))
        }
        Ok(metadata) if !metadata.is_file() => Ok(Destination::Special),
        Ok(metadata) => {
            // Read-only temporary files cannot be cleaned up on Windows after an error.
            if metadata.permissions().readonly() {
                return Err(cannot_save(path, ErrorKind::PermissionDenied, "file"));
            }
            // Preserve fs::write's access check without creating or truncating the file.
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|error| cannot_save(path, error.kind(), "file"))?;
            Ok(Destination::File(metadata.permissions()))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink()) {
                return Err(io::Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "Cannot save to {}: symlink target does not exist",
                        path.display()
                    ),
                )
                .into());
            }
            Ok(Destination::New)
        }
        Err(error) => Err(cannot_save(path, error.kind(), "directory")),
    }
}

/// A save error naming the user's path and the plain cause, never a temporary file name.
/// `subject` ("file" or "directory") is what a permission error refers to. The
/// [`io::ErrorKind`] stays available through `downcast_ref::<io::Error>()`.
fn cannot_save(path: &Path, kind: ErrorKind, subject: &str) -> anyhow::Error {
    let reason = match kind {
        ErrorKind::NotFound => "directory does not exist".to_string(),
        ErrorKind::NotADirectory => "parent is not a directory".to_string(),
        ErrorKind::IsADirectory => "is a directory".to_string(),
        ErrorKind::PermissionDenied => format!("{subject} is not writable"),
        ErrorKind::ReadOnlyFilesystem => "file system is read-only".to_string(),
        kind => kind.to_string(),
    };
    io::Error::new(kind, format!("Cannot save to {}: {reason}", path.display())).into()
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Truncate and write `path` in place, as fs::write did.
fn write_in_place(path: &Path, write: impl FnOnce(&mut dyn Write) -> Result<()>) -> Result<()> {
    let file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|error| cannot_save(path, error.kind(), "file"))?;
    let mut buffered = BufWriter::new(file);
    write(&mut buffered)?;
    buffered.flush()?;
    Ok(())
}

fn write_with(path: &Path, write: impl FnOnce(&mut dyn Write) -> Result<()>) -> Result<()> {
    let permissions = match inspect(path)? {
        // Devices, FIFOs, and pipes (/dev/null, /dev/stdout) cannot be replaced; write
        // through them as fs::write did. O_TRUNC is ignored for FIFOs and terminals.
        Destination::Special => return write_in_place(path, write),
        Destination::File(permissions) => Some(permissions),
        Destination::New => None,
    };
    let resolved = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Some(fs::canonicalize(path)?),
        Ok(_) => None,
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let target = resolved.as_deref().unwrap_or(path);
    let parent = parent_dir(target);
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
    let mut temporary = match builder.tempfile_in(parent) {
        Ok(temporary) => temporary,
        // fs::write could overwrite a writable file in a directory that is not writable.
        // Keep that working, at the cost of atomicity.
        Err(error) if error.kind() == ErrorKind::PermissionDenied && permissions.is_some() => {
            warn!(
                "{} is not writable; saving {} in place, so the save is not atomic",
                parent.display(),
                path.display()
            );
            return write_in_place(target, write);
        }
        Err(error) => return Err(cannot_save(path, error.kind(), "directory")),
    };
    if let Some(permissions) = permissions {
        temporary.as_file().set_permissions(permissions)?;
    }
    {
        let mut buffered = BufWriter::new(temporary.as_file_mut());
        write(&mut buffered)?;
        buffered.flush()?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist(target)
        .map_err(|error| cannot_save(path, error.error.kind(), "directory"))?;
    // Make the rename itself durable. Best effort: the new contents are already in place, and
    // some filesystems refuse to open or fsync a directory.
    #[cfg(unix)]
    let _ = fs::File::open(parent).and_then(|dir| dir.sync_all());
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
            write_with(&path, |writer| {
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
        write_with(&path, |writer| Ok(writer.write_all(b"new")?)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn rejects_directory_without_touching_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"previous").unwrap();
        assert!(write_with(&path, |_| panic!("must reject before writing")).is_err());
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
        let result = write_with(&path, |_| panic!("must reject before writing"));
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
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        symlink("saved", &link).unwrap();
        write_with(&link, |writer| {
            let temporary = fs::read_dir(dir.path())
                .unwrap()
                .map(|entry| entry.unwrap())
                .find(|entry| entry.file_name().to_string_lossy().starts_with(".xfr-"))
                .unwrap();
            assert_eq!(
                temporary.metadata().unwrap().permissions().mode() & 0o7777,
                0o666
            );
            Ok(writer.write_all(b"new")?)
        })
        .unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            path.metadata().unwrap().permissions().mode() & 0o7777,
            0o666
        );

        let plain = dir.path().join("plain");
        let new = dir.path().join("new");
        fs::File::create(&plain).unwrap();
        write_with(&new, |writer| Ok(writer.write_all(b"new")?)).unwrap();
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
        let result = write_with(&path, |_| panic!("must reject before writing"));
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
        atomic_write(Path::new("/dev/null"), b"discarded").unwrap();
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
        write_with(&fifo, |writer| Ok(writer.write_all(b"result")?)).unwrap();
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
        assert!(write_with(&link, |_| panic!("must reject before writing")).is_err());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(!dir.path().join("missing").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn overwrites_writable_file_in_readonly_directory_in_place() {
        use std::os::unix::fs::PermissionsExt;
        // Root ignores directory permissions, so the temporary file would succeed.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved");
        fs::write(&path, b"a longer previous save").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let checked = check_destination(&path);
        let result = atomic_write(&path, b"new");
        let new_file = check_destination(&dir.path().join("new")).map_err(|e| e.to_string());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        checked.unwrap();
        result.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        let error = new_file.unwrap_err();
        assert!(error.ends_with(": directory is not writable"), "{error}");
    }

    #[test]
    fn errors_name_the_destination_and_plain_cause() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nodir").join("x.json");
        for result in [check_destination(&missing), atomic_write(&missing, b"new")] {
            let error = format!("{:#}", result.unwrap_err());
            assert_eq!(
                error,
                format!(
                    "Cannot save to {}: directory does not exist",
                    missing.display()
                )
            );
        }
        for result in [
            check_destination(dir.path()),
            atomic_write(dir.path(), b"new"),
        ] {
            let error = format!("{:#}", result.unwrap_err());
            assert!(error.ends_with(": is a directory"), "{error}");
        }
        assert!(!dir.path().join("nodir").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn check_destination_creates_and_truncates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("existing");
        fs::write(&existing, b"previous").unwrap();
        check_destination(&existing).unwrap();
        check_destination(&dir.path().join("new")).unwrap();
        assert_eq!(fs::read(&existing).unwrap(), b"previous");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);

        let mut readonly = existing.metadata().unwrap().permissions();
        readonly.set_readonly(true);
        fs::set_permissions(&existing, readonly).unwrap();
        let error = check_destination(&existing).unwrap_err().to_string();
        assert!(error.ends_with(": file is not writable"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn check_destination_skips_special_files() {
        // Opening a FIFO for writing with no reader would block.
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let c_path =
            std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(fifo.as_os_str()))
                .unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        check_destination(&fifo).unwrap();
        check_destination(Path::new("/dev/null")).unwrap();
    }
}
