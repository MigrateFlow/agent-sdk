use std::io::{BufRead, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use tracing::{debug, trace};

use crate::error::{SdkError, SdkResult};
use crate::types::message::Envelope;

pub struct Mailbox {
    inbox_path: PathBuf,
    lock_path: PathBuf,
    read_offset: u64,
}

impl Mailbox {
    pub fn new(mailbox_dir: &Path) -> SdkResult<Self> {
        std::fs::create_dir_all(mailbox_dir)?;
        let inbox_path = mailbox_dir.join("inbox.jsonl");
        let lock_path = mailbox_dir.join("inbox.lock");

        if !inbox_path.exists() {
            std::fs::File::create(&inbox_path)?;
        }

        Ok(Self {
            inbox_path,
            lock_path,
            read_offset: 0,
        })
    }

    pub fn send(&self, envelope: &Envelope) -> SdkResult<()> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.lock_path)
            .map_err(SdkError::Io)?;

        file.lock_exclusive()
            .map_err(|_| SdkError::LockFailed {
                path: self.lock_path.clone(),
            })?;

        let mut inbox = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.inbox_path)?;

        let mut line = serde_json::to_string(envelope)?;
        line.push('\n');
        inbox.write_all(line.as_bytes())?;

        file.unlock().ok();

        debug!(
            to = ?envelope.to,
            kind = ?envelope.kind,
            "Message sent"
        );

        Ok(())
    }

    pub fn recv(&mut self) -> SdkResult<Vec<Envelope>> {
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .map_err(SdkError::Io)?;

        lock_file.lock_shared().map_err(|_| SdkError::LockFailed {
            path: self.lock_path.clone(),
        })?;

        let mut file = std::fs::File::open(&self.inbox_path)?;
        file.seek(SeekFrom::Start(self.read_offset))?;

        let reader = std::io::BufReader::new(&file);
        let mut messages = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Envelope>(&line) {
                Ok(envelope) => messages.push(envelope),
                Err(e) => {
                    tracing::warn!("Failed to parse mailbox message: {}", e);
                }
            }
        }

        self.read_offset = std::fs::metadata(&self.inbox_path)?.len();

        lock_file.unlock().ok();

        if !messages.is_empty() {
            trace!(count = messages.len(), "Received messages");
        }

        Ok(messages)
    }

    pub fn clear(&mut self) -> SdkResult<()> {
        std::fs::write(&self.inbox_path, "")?;
        self.read_offset = 0;
        Ok(())
    }

    pub fn inbox_path(&self) -> &Path {
        &self.inbox_path
    }
}
