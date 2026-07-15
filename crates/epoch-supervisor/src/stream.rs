use std::{
    io::{BufRead as _, BufReader, Read},
    sync::mpsc::SyncSender,
};

use epoch_protocol::{Envelope, MAX_JSONL_BYTES, ProtocolError, decode_line};

use crate::supervisor::{MAX_STDERR_BYTES, MAX_STDOUT_BYTES};

const MAX_JSONL_READ_BYTES: u64 = 1024 * 1024 + 1;
const MAX_STDERR_READ_BYTES: u64 = 1024 * 1024 + 1;

#[derive(Debug)]
pub(crate) struct ProtocolRecord {
    pub line: usize,
    pub raw: Vec<u8>,
    pub envelope: Envelope,
}

#[derive(Debug)]
pub(crate) enum ReaderMessage {
    StdoutRecord(ProtocolRecord),
    StdoutFinished,
    StdoutFailed(StreamError),
    StderrFinished(Vec<u8>),
    StderrFailed(StreamError),
}

#[derive(Debug)]
pub(crate) enum StreamError {
    Protocol { line: usize, source: ProtocolError },
    PartialFinalRecord { line: usize, bytes: usize },
    StdoutTooLarge,
    StderrTooLarge,
    Io(std::io::Error),
}

pub(crate) fn read_stdout(reader: impl Read, sender: &SyncSender<ReaderMessage>) {
    match read_stdout_inner(reader, sender) {
        Ok(()) => {
            let _ = sender.send(ReaderMessage::StdoutFinished);
        }
        Err(error) => {
            let _ = sender.send(ReaderMessage::StdoutFailed(error));
        }
    }
}

fn read_stdout_inner(
    reader: impl Read,
    sender: &SyncSender<ReaderMessage>,
) -> Result<(), StreamError> {
    let mut reader = BufReader::new(reader);
    let mut line_number = 1_usize;
    let mut total = 0_usize;
    loop {
        let mut raw = Vec::new();
        let mut bounded = (&mut reader).take(MAX_JSONL_READ_BYTES);
        let read = bounded
            .read_until(b'\n', &mut raw)
            .map_err(StreamError::Io)?;
        if read == 0 {
            return Ok(());
        }
        total = total.checked_add(read).ok_or(StreamError::StdoutTooLarge)?;
        if total > MAX_STDOUT_BYTES {
            return Err(StreamError::StdoutTooLarge);
        }
        if raw.last() != Some(&b'\n') && raw.len() <= MAX_JSONL_BYTES {
            return Err(StreamError::PartialFinalRecord {
                line: line_number,
                bytes: raw.len(),
            });
        }
        let envelope = decode_line(&raw).map_err(|source| StreamError::Protocol {
            line: line_number,
            source,
        })?;
        if sender
            .send(ReaderMessage::StdoutRecord(ProtocolRecord {
                line: line_number,
                raw,
                envelope,
            }))
            .is_err()
        {
            return Ok(());
        }
        line_number = line_number
            .checked_add(1)
            .ok_or(StreamError::StdoutTooLarge)?;
    }
}

pub(crate) fn read_stderr(mut reader: impl Read, sender: &SyncSender<ReaderMessage>) {
    let mut captured = Vec::with_capacity(MAX_STDERR_BYTES.min(8192));
    let result = reader
        .by_ref()
        .take(MAX_STDERR_READ_BYTES)
        .read_to_end(&mut captured);
    let message = match result {
        Err(error) => ReaderMessage::StderrFailed(StreamError::Io(error)),
        Ok(_) if captured.len() > MAX_STDERR_BYTES => {
            ReaderMessage::StderrFailed(StreamError::StderrTooLarge)
        }
        Ok(_) => ReaderMessage::StderrFinished(captured),
    };
    let _ = sender.send(message);
}
