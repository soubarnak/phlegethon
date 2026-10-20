//! Integrity Check — post-completion hash verification.
//!
//! Verify only when an expected hash is available (metalink, sidecar, or
//! user-supplied). Mismatch marks the job Failed and leaves the file on disk.

use crate::metalink;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// Supported digest algorithms for Integrity Check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum HashAlgorithm {
    Md5,
    Sha1,
    #[serde(rename = "sha-256")]
    Sha256,
}

/// Expected digest for a completed download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ExpectedHash {
    pub algorithm: HashAlgorithm,
    /// Lowercase hex digest (no separators).
    pub digest: String,
}

impl ExpectedHash {
    /// Parse algorithm name (metalink `type` attribute) and hex digest.
    pub fn parse(algorithm: &str, digest: &str) -> Result<Self, String> {
        let algorithm = parse_algorithm(algorithm)?;
        let digest = normalize_hex_digest(digest)?;
        let want = digest_hex_len(algorithm);
        if digest.len() != want {
            return Err(format!(
                "{} digest must be {want} hex characters, got {}",
                algorithm_label(algorithm),
                digest.len()
            ));
        }
        Ok(Self { algorithm, digest })
    }

    /// Parse a GNU `sha256sum`-style sidecar line (`DIGEST  filename` or `DIGEST`).
    pub fn from_sidecar_line(line: &str) -> Result<Self, String> {
        let token = line
            .split_whitespace()
            .next()
            .ok_or_else(|| "empty sidecar line".to_string())?;
        let digest = normalize_hex_digest(token)?;
        let algorithm = infer_algorithm_from_digest_len(digest.len())?;
        Ok(Self { algorithm, digest })
    }

    /// Parse sidecar file contents (first non-empty line).
    pub fn from_sidecar_contents(text: &str) -> Result<Self, String> {
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .ok_or_else(|| "sidecar has no hash line".to_string())?;
        Self::from_sidecar_line(line)
    }

    /// Parse sidecar contents and return the digest listed for `filename`.
    ///
    /// Rows are GNU `sha256sum` style: `DIGEST  name` or `DIGEST *name` (binary
    /// marker). A row with no name is a bare single-file digest and applies to
    /// any file. Errors when no row matches, so a multi-file `SHA256SUMS` can
    /// never check a job against another file's digest.
    pub fn from_sidecar_for_file(text: &str, filename: &str) -> Result<Self, String> {
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (token, rest) = match line.split_once(char::is_whitespace) {
                Some((token, rest)) => (token, rest.trim_start()),
                None => (line, ""),
            };
            let name = rest.strip_prefix('*').unwrap_or(rest);
            if !name.is_empty() && !sidecar_name_matches(name, filename) {
                continue;
            }
            let digest = normalize_hex_digest(token)?;
            let algorithm = infer_algorithm_from_digest_len(digest.len())?;
            return Ok(Self { algorithm, digest });
        }
        Err(format!("sidecar has no checksum entry for {filename}"))
    }

    /// Verify `path` against this expected digest.
    pub fn verify_file(&self, path: &Path) -> Result<(), String> {
        let actual = compute_file_digest(path, self.algorithm)?;
        if actual == self.digest {
            Ok(())
        } else {
            Err(format!(
                "integrity check failed for {}: expected {} {}, got {}",
                path.display(),
                algorithm_label(self.algorithm),
                self.digest,
                actual
            ))
        }
    }
}

/// Extract the best usable hash from a Metalink / `.meta4` document.
///
/// Reads only `<hash>` elements that are direct children of `<file>` (or of
/// its Metalink 3 `<verification>` wrapper). `<pieces>` hashes, hashes without
/// a `type`, and digests whose length does not fit the algorithm are skipped.
/// When several remain, prefers sha-256, then sha-1, then md5.
pub fn hash_from_metalink(xml: &str) -> Option<ExpectedHash> {
    metalink::parse(xml)
        .hashes
        .into_iter()
        .filter_map(|h| ExpectedHash::parse(&h.algorithm?, &h.digest).ok())
        .min_by_key(|h| hash_preference(h.algorithm))
}

fn hash_preference(algorithm: HashAlgorithm) -> u8 {
    match algorithm {
        HashAlgorithm::Sha256 => 0,
        HashAlgorithm::Sha1 => 1,
        HashAlgorithm::Md5 => 2,
    }
}

/// Whether a sidecar row's name column refers to `filename` (exact, `./name`,
/// or the final path component of a relative path).
fn sidecar_name_matches(name: &str, filename: &str) -> bool {
    let name = name.strip_prefix("./").unwrap_or(name);
    name == filename || name.rsplit('/').next() == Some(filename)
}

fn digest_hex_len(algorithm: HashAlgorithm) -> usize {
    match algorithm {
        HashAlgorithm::Md5 => 32,
        HashAlgorithm::Sha1 => 40,
        HashAlgorithm::Sha256 => 64,
    }
}

fn parse_algorithm(name: &str) -> Result<HashAlgorithm, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "md5" => Ok(HashAlgorithm::Md5),
        "sha-1" | "sha1" => Ok(HashAlgorithm::Sha1),
        "sha-256" | "sha256" => Ok(HashAlgorithm::Sha256),
        other => Err(format!("unsupported hash algorithm: {other}")),
    }
}

fn infer_algorithm_from_digest_len(len: usize) -> Result<HashAlgorithm, String> {
    match len {
        32 => Ok(HashAlgorithm::Md5),
        40 => Ok(HashAlgorithm::Sha1),
        64 => Ok(HashAlgorithm::Sha256),
        n => Err(format!(
            "cannot infer hash algorithm from digest length {n}"
        )),
    }
}

fn normalize_hex_digest(raw: &str) -> Result<String, String> {
    let hex: String = raw
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if hex.is_empty() {
        return Err("empty hash digest".into());
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("hash digest is not hex: {raw}"));
    }
    Ok(hex)
}

fn algorithm_label(algorithm: HashAlgorithm) -> &'static str {
    match algorithm {
        HashAlgorithm::Md5 => "md5",
        HashAlgorithm::Sha1 => "sha-1",
        HashAlgorithm::Sha256 => "sha-256",
    }
}

fn compute_file_digest(path: &Path, algorithm: HashAlgorithm) -> Result<String, String> {
    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut buffer = [0u8; 8192];

    match algorithm {
        HashAlgorithm::Md5 => {
            use md5::{Digest, Md5};
            let mut hasher = Md5::new();
            loop {
                let n = reader
                    .read(&mut buffer)
                    .map_err(|e| format!("read {}: {e}", path.display()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(format!("{:x}", hasher.finalize()))
        }
        HashAlgorithm::Sha1 => {
            use sha1::{Digest, Sha1};
            let mut hasher = Sha1::new();
            loop {
                let n = reader
                    .read(&mut buffer)
                    .map_err(|e| format!("read {}: {e}", path.display()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(format!("{:x}", hasher.finalize()))
        }
        HashAlgorithm::Sha256 => {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            loop {
                let n = reader
                    .read(&mut buffer)
                    .map_err(|e| format!("read {}: {e}", path.display()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(format!("{:x}", hasher.finalize()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_hash_verify_file_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("payload.bin");
        std::fs::write(&path, b"integrity payload").unwrap();

        let expected = ExpectedHash::parse(
            "sha-256",
            "3b6d4a3a489a373b911a1de7befe6d679674f3f119b85a1c31a2054496c104ca",
        )
        .unwrap();
        expected.verify_file(&path).unwrap();
    }

    #[test]
    fn expected_hash_verify_file_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("payload.bin");
        std::fs::write(&path, b"wrong bytes").unwrap();

        let expected = ExpectedHash::parse(
            "sha-256",
            "3b6d4a3a489a373b911a1de7befe6d679674f3f119b85a1c31a2054496c104ca",
        )
        .unwrap();
        let err = expected.verify_file(&path).unwrap_err();
        assert!(err.contains("integrity check failed"), "got: {err}");
    }

    #[test]
    fn from_sidecar_contents_parses_sha256sum_line() {
        let hash = ExpectedHash::from_sidecar_contents(
            "3b6d4a3a489a373b911a1de7befe6d679674f3f119b85a1c31a2054496c104ca  payload.bin\n",
        )
        .unwrap();
        assert_eq!(hash.algorithm, HashAlgorithm::Sha256);
        assert_eq!(
            hash.digest,
            "3b6d4a3a489a373b911a1de7befe6d679674f3f119b85a1c31a2054496c104ca"
        );
    }

    #[test]
    fn hash_from_metalink_prefers_sha256() {
        let xml = r#"
            <metalink>
                <file name="test.zip">
                    <hash type="md5">098f6bcd4621d373cade4e832627b4f6</hash>
                    <hash type="sha-256">2c70e12b7a0646f92279e427156332ef536f35ba97c38fa0e0b0af397203f2e7</hash>
                </file>
            </metalink>
        "#;
        let hash = hash_from_metalink(xml).unwrap();
        assert_eq!(hash.algorithm, HashAlgorithm::Sha256);
    }

    #[test]
    fn hash_from_metalink_returns_none_without_hash_tags() {
        let xml = r#"<metalink><file name="x"><url>https://x</url></file></metalink>"#;
        assert!(hash_from_metalink(xml).is_none());
    }

    const SHA256_A: &str = "2c70e12b7a0646f92279e427156332ef536f35ba97c38fa0e0b0af397203f2e7";
    const SHA256_B: &str = "3b6d4a3a489a373b911a1de7befe6d679674f3f119b85a1c31a2054496c104ca";

    #[test]
    fn hash_from_metalink_keeps_file_hash_when_pieces_follow_without_type() {
        // H-15: the old scanner returned None for the whole document here.
        let xml = format!(
            r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
                <file name="a.iso">
                    <hash type="sha-256">{SHA256_A}</hash>
                    <pieces length="1024" type="sha-256">
                        <hash>{SHA256_B}</hash>
                        <hash>{SHA256_B}</hash>
                    </pieces>
                </file>
            </metalink>"#
        );
        let hash = hash_from_metalink(&xml).unwrap();
        assert_eq!(hash.algorithm, HashAlgorithm::Sha256);
        assert_eq!(hash.digest, SHA256_A);
    }

    #[test]
    fn hash_from_metalink_ignores_piece_hashes_with_a_later_type() {
        // H-15: a piece digest must never be stored under another hash's type.
        let xml = format!(
            r#"<metalink><file name="a.iso">
                <pieces length="1024" type="sha-1">
                    <hash>{SHA256_B}</hash>
                </pieces>
                <hash type="md5">098f6bcd4621d373cade4e832627b4f6</hash>
            </file></metalink>"#
        );
        let hash = hash_from_metalink(&xml).unwrap();
        assert_eq!(hash.algorithm, HashAlgorithm::Md5);
        assert_eq!(hash.digest, "098f6bcd4621d373cade4e832627b4f6");
    }

    #[test]
    fn hash_from_metalink_skips_hash_without_type() {
        let xml = format!(
            r#"<metalink><file name="a"><hash>{SHA256_B}</hash>
               <hash type="sha-256">{SHA256_A}</hash></file></metalink>"#
        );
        assert_eq!(hash_from_metalink(&xml).unwrap().digest, SHA256_A);
    }

    #[test]
    fn hash_from_metalink_rejects_wrong_digest_length_for_algorithm() {
        // A 40-char digest labelled sha-256 is not a sha-256 digest.
        let xml = r#"<metalink><file name="a">
            <hash type="sha-256">a9993e364706816aba3e25717850c26c9cd0d89d</hash>
        </file></metalink>"#;
        assert!(hash_from_metalink(xml).is_none());
    }

    #[test]
    fn hash_from_metalink_only_reads_direct_file_children() {
        let xml = format!(
            r#"<metalink><file name="a">
                <other><hash type="sha-256">{SHA256_B}</hash></other>
            </file></metalink>"#
        );
        assert!(hash_from_metalink(&xml).is_none());
    }

    // --- Checksum sidecar (M-17) ---

    #[test]
    fn sidecar_for_file_picks_the_row_for_the_job_filename() {
        let text = format!("{SHA256_A}  other.iso\n{SHA256_B}  file.zip\n");
        let hash = ExpectedHash::from_sidecar_for_file(&text, "file.zip").unwrap();
        assert_eq!(hash.digest, SHA256_B);
    }

    #[test]
    fn sidecar_for_file_handles_binary_marker() {
        let text = format!("{SHA256_A} *file.zip\n");
        let hash = ExpectedHash::from_sidecar_for_file(&text, "file.zip").unwrap();
        assert_eq!(hash.digest, SHA256_A);
    }

    #[test]
    fn sidecar_for_file_errors_when_no_row_matches() {
        let text = format!("{SHA256_A}  other.iso\n");
        let err = ExpectedHash::from_sidecar_for_file(&text, "file.zip").unwrap_err();
        assert!(err.contains("file.zip"), "got: {err}");
    }

    #[test]
    fn sidecar_for_file_accepts_bare_digest() {
        let hash =
            ExpectedHash::from_sidecar_for_file(&format!("{SHA256_A}\n"), "file.zip").unwrap();
        assert_eq!(hash.digest, SHA256_A);
    }

    #[test]
    fn sidecar_for_file_keeps_spaces_in_filename() {
        let text = format!("{SHA256_A}  my file.zip\n");
        assert!(ExpectedHash::from_sidecar_for_file(&text, "my file.zip").is_ok());
    }
}
