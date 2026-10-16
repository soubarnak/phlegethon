//! Shared Metalink / `.meta4` reader built on `quick-xml`.
//!
//! Feeds the Mirror Set (`<url>`) and the Integrity Check (`<hash>`). Only the
//! element positions that Metalink defines are read, so `<pieces>` hashes and
//! look-alike tags elsewhere in the document are ignored.

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;
use quick_xml::Reader;

/// A `<hash>` element found directly under a `<file>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawHash {
    /// The `type` attribute, if present.
    pub algorithm: Option<String>,
    /// Element text with entities decoded and whitespace trimmed.
    pub digest: String,
}

/// Mirror candidates and file hashes pulled from a Metalink document.
#[derive(Debug, Default)]
pub(crate) struct MetalinkDoc {
    pub urls: Vec<String>,
    pub hashes: Vec<RawHash>,
}

#[derive(PartialEq)]
enum Target {
    Url,
    Hash(Option<String>),
}

/// Parse `xml`, tolerating malformed trailing content (what was read before
/// the error is kept).
///
/// Accepted positions, matched on local names so any namespace prefix works:
/// - `file/url` (Metalink 4) and `file/resources/url` (Metalink 3)
/// - `file/hash` (Metalink 4) and `file/verification/hash` (Metalink 3)
pub(crate) fn parse(xml: &str) -> MetalinkDoc {
    let mut doc = MetalinkDoc::default();
    let mut reader = Reader::from_str(xml);
    let mut path: Vec<String> = Vec::new();
    let mut capture: Option<(Target, usize, String)> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                path.push(e.local_name().as_ref().to_string());
                if capture.is_none() {
                    if let Some(kind) = classify(&path) {
                        let target = match kind {
                            Kind::Url => Target::Url,
                            Kind::Hash => Target::Hash(type_attribute(&e)),
                        };
                        capture = Some((target, path.len(), String::new()));
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if let Some((_, _, buf)) = capture.as_mut() {
                    buf.push_str(&t);
                }
            }
            Ok(Event::CData(c)) => {
                if let Some((_, _, buf)) = capture.as_mut() {
                    buf.push_str(&c);
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if let Some((_, _, buf)) = capture.as_mut() {
                    match r.resolve_char_ref() {
                        Ok(Some(c)) => buf.push(c),
                        _ => {
                            if let Some(s) = resolve_predefined_entity(&r) {
                                buf.push_str(s);
                            }
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {
                if let Some((_, depth, _)) = capture.as_ref() {
                    if *depth == path.len() {
                        let (target, _, text) = capture.take().expect("capture checked above");
                        let text = text.trim().to_string();
                        match target {
                            Target::Url => {
                                if !text.is_empty() {
                                    doc.urls.push(text);
                                }
                            }
                            Target::Hash(algorithm) => doc.hashes.push(RawHash {
                                algorithm,
                                digest: text,
                            }),
                        }
                    }
                }
                path.pop();
            }
            Ok(Event::Eof) | Err(_) => break,
            // `<url/>` and `<hash/>` carry no text, so they yield nothing.
            Ok(_) => {}
        }
    }
    doc
}

enum Kind {
    Url,
    Hash,
}

fn classify(path: &[String]) -> Option<Kind> {
    let name = path.last()?.as_str();
    let kind = match name {
        "url" => Kind::Url,
        "hash" => Kind::Hash,
        _ => return None,
    };
    let parent = path.len().checked_sub(2).map(|i| path[i].as_str())?;
    let grandparent = path.len().checked_sub(3).map(|i| path[i].as_str());
    let wrapper = match kind {
        Kind::Url => "resources",
        Kind::Hash => "verification",
    };
    if parent == "file" || (parent == wrapper && grandparent == Some("file")) {
        Some(kind)
    } else {
        None
    }
}

fn type_attribute(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    for attr in e.attributes().flatten() {
        if attr.key.local_name().as_ref() == "type" {
            return attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
        }
    }
    None
}
