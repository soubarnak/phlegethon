//! Body I/O executed outside the Core mutex (issue #31).

use crate::bandwidth::{SharedTokenBucket, TransferProgress, TransferWriter};
use crate::http::{FetchResult, HttpBackend};
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub struct SingleFetchParams {
    pub http: Arc<dyn HttpBackend>,
    pub url: String,
    pub part_path: PathBuf,
    pub range: Option<Range<u64>>,
    /// `If-Range` validator sent with a resume Range request.
    pub if_range: Option<String>,
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

    let fetch = params.http.fetch_conditional(
        &params.url,
        params.range.clone(),
        params.if_range.as_deref(),
        &mut limited,
    );
    let paused = limited.was_paused();
    drop(limited);

    if let Err(err) = part_file.flush().and_then(|()| part_file.sync_all()) {
        return SingleFetchOutcome {
            fetch: Err(format!("flush partial {:?}: {err}", params.part_path)),
            paused,
        };
    }

    SingleFetchOutcome { fetch, paused }
}

pub struct SegmentFetchParams {
    pub http: Arc<dyn HttpBackend>,
    pub url: String,
    pub part_path: PathBuf,
    /// The bytes still to fetch for this segment (absolute offsets).
    pub range: Range<u64>,
    /// `If-Range` validator sent when resuming an interrupted segment.
    pub if_range: Option<String>,
    /// Advanced by the bytes this fetch has written and synced to disk.
    pub written: Arc<AtomicU64>,
    pub global_bucket: Option<SharedTokenBucket>,
    pub job_bucket: Option<SharedTokenBucket>,
    pub pause_requested: Arc<AtomicBool>,
    pub progress: Option<TransferProgress>,
}

/// Why a segment fetch failed.
#[derive(Debug, PartialEq, Eq)]
pub enum SegmentError {
    /// The server did not answer the Range request with the requested slice.
    RangeIgnored,
    Failed(String),
}

impl From<String> for SegmentError {
    fn from(err: String) -> Self {
        Self::Failed(err)
    }
}

/// What a finished segment fetch learned about the object.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SegmentFetched {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Bytes written between `sync_data` calls; the segment's `written` counter
/// only advances at these points so a persisted table never claims data that
/// a power loss could take back.
const SEGMENT_SYNC_BYTES: u64 = 4 * 1024 * 1024;

pub fn execute_segment_fetch(params: SegmentFetchParams) -> Result<SegmentFetched, SegmentError> {
    let mut writer = OffsetWriter::new(
        &params.part_path,
        params.range.start,
        params.range.end - params.range.start,
        params.written.clone(),
    )?;
    let mut limited = TransferWriter::new(
        &mut writer,
        params.global_bucket,
        params.job_bucket,
        &params.pause_requested,
    );
    if let Some(progress) = params.progress.clone() {
        limited = limited.with_progress(progress);
    }
    let fetched = params.http.fetch_conditional(
        &params.url,
        Some(params.range.clone()),
        params.if_range.as_deref(),
        &mut limited,
    );
    let paused = limited.was_paused();
    drop(limited);
    // Count what reached the disk before judging the reply, so a failed or
    // paused segment keeps its progress.
    let committed = writer.commit();
    let result = match fetched {
        Ok(result) => result,
        Err(_) if writer.overflowed => return Err(SegmentError::RangeIgnored),
        // A pause makes the sink refuse data, which the backend reports as an error.
        Err(_) if paused => return Ok(SegmentFetched::default()),
        Err(err) => return Err(SegmentError::Failed(err)),
    };
    committed.map_err(|e| format!("flush partial {:?}: {e}", params.part_path))?;
    if result.range_ignored(params.range.start) {
        return Err(SegmentError::RangeIgnored);
    }
    if !paused && writer.remaining != 0 {
        return Err(SegmentError::Failed(format!(
            "segment {}..{} ended {} bytes early",
            params.range.start, params.range.end, writer.remaining
        )));
    }
    Ok(SegmentFetched {
        etag: result.etag,
        last_modified: result.last_modified,
    })
}

/// Writes to a fixed offset in an on-disk file, at most `limit` bytes.
///
/// A write that would pass the limit fails and sets `overflowed`: the server
/// sent more than the segment's range, so it ignored the Range header.
struct OffsetWriter {
    file: std::fs::File,
    remaining: u64,
    overflowed: bool,
    written: Arc<AtomicU64>,
    /// Bytes written since the last sync, not yet added to `written`.
    unsynced: u64,
}

impl OffsetWriter {
    fn new(path: &Path, offset: u64, limit: u64, written: Arc<AtomicU64>) -> Result<Self, String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| format!("open {path:?} for offset write: {e}"))?;
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| format!("seek {path:?} to {offset}: {e}"))?;
        Ok(Self {
            file,
            remaining: limit,
            overflowed: false,
            written,
            unsynced: 0,
        })
    }

    /// Sync the file and publish the bytes written so far to the counter.
    fn commit(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        self.file.sync_data()?;
        self.written.fetch_add(self.unsynced, Ordering::AcqRel);
        self.unsynced = 0;
        Ok(())
    }
}

impl Write for OffsetWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.remaining {
            self.overflowed = true;
            return Err(std::io::Error::other(
                "response longer than the requested segment",
            ));
        }
        let n = self.file.write(buf)?;
        self.remaining -= n as u64;
        self.unsynced += n as u64;
        if self.unsynced >= SEGMENT_SYNC_BYTES {
            self.commit()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
