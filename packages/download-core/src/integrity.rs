//! Integrity Check — post-completion hash verification.
//!
//! Verify only when an expected hash is available (metalink, sidecar, or
//! user-supplied). Mismatch marks the job Failed and leaves the file on disk.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// Supported digest algorithms for Integrity Check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HashAlgorithm {
    Md5,
    Sha1,
    #[serde(rename = "sha-256")]
    Sha256,
}

/// Expected digest for a completed download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Extract the first usable hash from a Metalink / `.meta4` document.
///
/// When multiple hashes are present, prefers sha-256, then sha-1, then md5.
pub fn hash_from_metalink(xml: &str) -> Option<ExpectedHash> {
    let mut found: Vec<ExpectedHash> = Vec::new();
    for tag_start in xml.match_indices("<hash").map(|(i, _)| i) {
        let tag = &xml[tag_start..];
        let type_start = tag.find("type=\"").or_else(|| tag.find("type='"))?;
        let quote_byte = tag.as_bytes()[type_start + 5];
        let quote = quote_byte as char;
        let value_start = type_start + 6;
        let rest = &tag[value_start..];
        let value_end = rest.find(quote)?;
        let algorithm = &rest[..value_end];

        let content_start = match tag.find('>') {
            Some(pos) => tag_start + pos + 1,
            None => continue,
        };
        let end_tag = match xml[content_start..].find("</hash>") {
            Some(pos) => content_start + pos,
            None => continue,
        };
        let digest = xml[content_start..end_tag].trim();
        if digest.is_empty() {
            continue;
        }
        if let Ok(hash) = ExpectedHash::parse(algorithm, digest) {
            found.push(hash);
        }
    }

    found.into_iter().min_by_key(|h| hash_preference(h.algorithm))
}

fn hash_preference(algorithm: HashAlgorithm) -> u8 {
    match algorithm {
        HashAlgorithm::Sha256 => 0,
        HashAlgorithm::Sha1 => 1,
        HashAlgorithm::Md5 => 2,
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
        n => Err(format!("cannot infer hash algorithm from digest length {n}")),
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
}
