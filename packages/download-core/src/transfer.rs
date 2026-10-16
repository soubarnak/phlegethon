//! Body I/O executed outside the Core mutex (issue #31).

use crate::bandwidth::{shared_token_bucket, SharedTokenBucket, TransferProgress, TransferWriter};
use crate::http::{FetchResult, HttpBackend};
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct SingleFetchParams {
    pub http: Arc<dyn HttpBackend>,
    pub url: String,
    pub part_path: PathBuf,
    pub range: Option<Range<u64>>,
    pub append: bool,
    pub truncate: bool,
    pub global_bucket: Option<SharedTokenBucket>,
    pub job_bucket: Option<SharedTokenBucket>,
    pub pause_requested: Arc<AtomicBool>,
    pub transfer_active: Arc<AtomicBool>,
    pub progress: Option<TransferProgress>,
}

pub struct SingleFetchOutcome {
    pub fetch: Result<FetchResult, String>,
    pub bytes_written: u64,
    pub paused: bool,
}

pub fn execute_single_fetch(params: SingleFetchParams) -> SingleFetchOutcome {
    params.transfer_active.store(true, Ordering::Release);
    let result = run_single_fetch(&params);
    params.transfer_active.store(false, Ordering::Release);
    result
}

fn run_single_fetch(params: &SingleFetchParams) -> SingleFetchOutcome {
    let open = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(params.append)
        .truncate(params.truncate)
        .open(&params.part_path);

    let mut part_file = match open {
        Ok(file) => file,
        Err(e) => {
            return SingleFetchOutcome {
                fetch: Err(format!("open partial {:?}: {e}", params.part_path)),
                bytes_written: 0,
                paused: false,
            };
        }
    };

    let mut limited = TransferWriter::new(
        &mut part_file,
        params.global_bucket.clone(),
        params.job_bucket.clone(),
        &params.pause_requested,
    );
    if let Some(progress) = params.progress.clone() {
        limited = limited.with_progress(progress);
    }

    let fetch = params
        .http
        .fetch(&params.url, params.range.clone(), &mut limited)
        .map_err(|e| e);
    let bytes_written = limited.bytes_written();
    let paused = limited.was_paused();
    drop(limited);

    if let Err(err) = part_file.flush() {
        return SingleFetchOutcome {
            fetch: Err(format!("flush partial {:?}: {err}", params.part_path)),
            bytes_written,
            paused,
        };
    }

    SingleFetchOutcome {
        fetch,
        bytes_written,
        paused,
    }
}

pub struct SegmentFetchParams {
    pub http: Arc<dyn HttpBackend>,
    pub url: String,
    pub part_path: PathBuf,
    pub range: Range<u64>,
    pub global_bucket: Option<SharedTokenBucket>,
    pub job_bucket: Option<SharedTokenBucket>,
    pub pause_requested: Arc<AtomicBool>,
    pub progress: Option<TransferProgress>,
}

pub fn execute_segment_fetch(params: SegmentFetchParams) -> Result<u64, String> {
    let mut writer = OffsetWriter::new(&params.part_path, params.range.start)?;
    let mut limited = TransferWriter::new(
        &mut writer,
        params.global_bucket,
        params.job_bucket,
        &params.pause_requested,
    );
    if let Some(progress) = params.progress.clone() {
        limited = limited.with_progress(progress);
    }
    let end = params.range.end;
    let result = params
        .http
        .fetch(&params.url, Some(params.range.start..end), &mut limited)?;
    let bytes_written = limited.bytes_written();
    let paused = limited.was_paused();
    drop(limited);
    writer
        .flush()
        .map_err(|e| format!("flush partial {:?}: {e}", params.part_path))?;
    if paused {
        return Ok(bytes_written);
    }
    Ok(result.bytes_written)
}

/// Writes to a fixed offset in an on-disk file.
struct OffsetWriter {
    file: std::fs::File,
}

impl OffsetWriter {
    fn new(path: &Path, offset: u64) -> Result<Self, String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| format!("open {path:?} for offset write: {e}"))?;
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| format!("seek {path:?} to {offset}: {e}"))?;
        Ok(Self { file })
    }
}

impl Write for OffsetWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

pub fn make_job_bucket(rate: u64, clock: Arc<dyn crate::bandwidth::Clock>) -> SharedTokenBucket {
    shared_token_bucket(rate, clock)
}
