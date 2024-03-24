use std::io;

use camino::{Utf8Path, Utf8PathBuf};

/// A PID file is a file that contains the PID of a process. It is used to
/// prevent multiple instances of a process from running at the same time,
/// or to provide a lock for a resource which should only be accessed by one
/// process at a time.
#[derive(Debug)]
pub struct PidFile {
    path: Utf8PathBuf,
}

/// Check if a PID file is in use.
///
/// If the PID file corresponds to a currently unused PID, the file
/// will be removed by this function.
fn pid_file_in_use(path: &Utf8Path) -> Result<bool, io::Error> {
    match std::fs::read_to_string(path) {
        Ok(info) => {
            let pid: libc::pid_t = info.trim().parse().map_err(|error| {
                tracing::debug!(%path, "Unable to parse PID file {path}: {error}");
                io::Error::new(io::ErrorKind::InvalidData, "expected a PID")
            })?;

            // SAFETY: I dunno? Libc is probably fine.
            let errno = unsafe { libc::kill(pid, 0) };

            if errno == 0 {
                tracing::debug!(?pid, "PID {pid} is still running", pid = pid);
                // This PID still exists, so the pid file is valid.
                return Ok(true);
            }

            if errno == -1 {
                tracing::debug!(?pid, "Unkonwn error checking PID file: {errno}");
                return Ok(false);
            };

            let error = io::Error::from_raw_os_error(errno);
            match error.kind() {
                io::ErrorKind::NotFound => Ok(false),
                _ => Err(error),
            }
        }
        Err(error) => match error.kind() {
            io::ErrorKind::NotFound => Ok(false),
            _ => Err(error),
        },
    }
}

impl PidFile {
    /// Create a new PID file at the given path for this process.
    ///
    /// If the PID file already exists, this function will check if the
    /// PID file is still in use. If the PID file is in use, this function
    /// will return Err(io::ErrorKind::AddrInUse). If the PID file is not
    /// in use, it will be removed and a new PID file will be created.
    pub fn new(path: Utf8PathBuf) -> Result<Self, io::Error> {
        if path.exists() {
            match pid_file_in_use(&path) {
                Ok(true) => {
                    tracing::error!("PID File {path} is already in use");
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("PID File {path} is already in use"),
                    ));
                }
                Ok(false) => {
                    tracing::warn!("Removing stale PID file at {path}");
                    let _ = std::fs::remove_file(&path);
                }
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    tracing::warn!("Removing invalid PID file at {path}");
                    let _ = std::fs::remove_file(&path);
                }
                Err(error) => {
                    tracing::error!(%path, "Unable to check PID file {path}: {error}");
                    return Err(error);
                }
            }
        }

        // SAFETY: What could go wrong?
        let pid = unsafe { libc::getpid() };

        if pid <= 0 {
            tracing::error!("libc::getpid() returned a negative PID: {pid}");
            return Err(io::Error::new(io::ErrorKind::Other, "negative PID"));
        }

        std::fs::write(&path, format!("{}", pid))?;
        tracing::trace!(?pid, "Locked PID file at {path}");

        Ok(Self { path })
    }

    /// Check if a PID file is in use at this path.
    pub fn is_locked(path: &Utf8Path) -> Result<bool, io::Error> {
        match pid_file_in_use(path) {
            Ok(true) => Ok(true),
            Ok(false) => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                tracing::warn!("Invalid PID file at {path}");
                Ok(false)
            }
            Err(error) => {
                tracing::error!(%path, "Unable to check PID file {path}: {error}");
                Err(error)
            }
        }
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            Ok(_) => {}
            Err(error) => eprintln!(
                "Encountered an error removing the PID file at {}: {}",
                self.path, error
            ),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use tracing_test::traced_test;

    #[test]
    fn test_pid_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(tmp.path().join("pidfile-test.pid")).unwrap();
        let pid_file = PidFile::new(path.clone()).unwrap();
        assert!(PidFile::is_locked(&path).unwrap());
        drop(pid_file);
        assert!(!PidFile::is_locked(&path).unwrap());
    }

    #[test]
    #[traced_test]
    fn test_invalid_file() {
        let path = Utf8Path::new("/tmp/pidfile-test.pid");
        std::fs::write(path, "not a pid").unwrap();
        assert!(
            !PidFile::is_locked(path).unwrap(),
            "Invalid file should not be locked."
        );
        assert!(
            path.exists(),
            "Invalid file should exist after checking for locks."
        );

        let pid_file = PidFile::new(path.into()).unwrap();
        assert!(
            PidFile::is_locked(path).unwrap(),
            "PID file should be locked after creation."
        );
        drop(pid_file);
        assert!(
            !PidFile::is_locked(path).unwrap(),
            "PID file should not be locked after drop."
        );
    }
}
