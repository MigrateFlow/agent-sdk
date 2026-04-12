use fs2::FileExt;
use std::fs;
use std::path::{Path, PathBuf};

use sdk_core::error::{SdkError, SdkResult};

pub struct FileLock {
    file: fs::File,
    path: PathBuf,
}

impl FileLock {
    pub fn try_acquire(path: &Path) -> SdkResult<Option<Self>> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(SdkError::Io)?;

        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(FileLock {
                file,
                path: path.to_owned(),
            })),
            Err(_) => Ok(None),
        }
    }

    pub fn acquire_blocking(path: &Path) -> SdkResult<Self> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(SdkError::Io)?;

        file.lock_exclusive()
            .map_err(|_| SdkError::LockFailed {
                path: path.to_owned(),
            })?;

        Ok(FileLock {
            file,
            path: path.to_owned(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&self) -> &fs::File {
        &self.file
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
