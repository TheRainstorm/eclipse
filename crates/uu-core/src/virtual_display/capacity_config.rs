//! Lossless driver configuration edits. Only the monitor count is owned here;
//! mode lists, comments, encoding, whitespace and line endings remain user data.

use super::{Error, Result};

const MAX_XML: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Change {
    pub before: Vec<u8>,
    pub after: Vec<u8>,
    pub original_count: u32,
    pub count: u32,
}

#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Little,
    Big,
}

fn fail(message: &str) -> Error {
    Error::Platform(message.into())
}

fn decode(bytes: &[u8]) -> Result<(String, Encoding)> {
    if bytes.is_empty() || bytes.len() > MAX_XML {
        return Err(fail(
            "Mtt configuration must be a nonempty bounded XML file",
        ));
    }
    let (encoding, body) = if bytes.starts_with(&[0xff, 0xfe]) {
        (Encoding::Little, &bytes[2..])
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (Encoding::Big, &bytes[2..])
    } else {
        (
            Encoding::Utf8,
            bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes),
        )
    };
    let value = match encoding {
        Encoding::Utf8 => std::str::from_utf8(body)
            .map(str::to_owned)
            .map_err(|_| fail("Mtt XML is not valid UTF-8"))?,
        Encoding::Little | Encoding::Big => {
            if body.len() % 2 != 0 {
                return Err(fail("Mtt UTF-16 XML has an incomplete code unit"));
            }
            let units: Vec<_> = body
                .chunks_exact(2)
                .map(|c| match encoding {
                    Encoding::Little => u16::from_le_bytes([c[0], c[1]]),
                    _ => u16::from_be_bytes([c[0], c[1]]),
                })
                .collect();
            String::from_utf16(&units).map_err(|_| fail("Mtt XML is not valid UTF-16"))?
        }
    };
    if value.contains('\0') {
        return Err(fail("Mtt XML contains a NUL character"));
    }
    Ok((value, encoding))
}

fn encode(value: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 => value.as_bytes().to_vec(),
        Encoding::Little => value.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        Encoding::Big => value.encode_utf16().flat_map(u16::to_be_bytes).collect(),
    }
}

// The pinned driver uses XmlLite and reads vdd_settings/monitors/count. Require
// that exact unqualified element, reject entities/DTD, and do not confuse a
// commented-out token or another element's count with the live setting.
fn count_range(xml: &str) -> Result<std::ops::Range<usize>> {
    let mut stack: Vec<&str> = Vec::new();
    let mut count = None;
    let mut at = 0;
    while let Some(relative) = xml[at..].find('<') {
        let start = at + relative;
        if xml[start..].starts_with("<!--") {
            at = start
                + 4
                + xml[start + 4..]
                    .find("-->")
                    .ok_or_else(|| fail("unterminated XML comment"))?
                + 3;
            continue;
        }
        if xml[start..].starts_with("<?") {
            at = start
                + 2
                + xml[start + 2..]
                    .find("?>")
                    .ok_or_else(|| fail("unterminated XML declaration"))?
                + 2;
            continue;
        }
        if xml[start..].starts_with("<!") {
            return Err(fail(
                "Mtt XML DTD and CDATA are not supported for a lossless capacity edit",
            ));
        }
        let end = start
            + xml[start..]
                .find('>')
                .ok_or_else(|| fail("unterminated XML tag"))?;
        let token = xml[start + 1..end].trim();
        if let Some(name) = token.strip_prefix('/') {
            if stack.pop() != Some(name.trim()) {
                return Err(fail("Mtt XML elements are not balanced"));
            }
        } else {
            let empty = token.ends_with('/');
            let name = token
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('/');
            if name.is_empty() {
                return Err(fail("Mtt XML has an empty element name"));
            }
            if name == "count" {
                if empty
                    || stack != ["vdd_settings", "monitors"]
                    || token != "count"
                    || count.is_some()
                {
                    return Err(fail(
                        "Mtt XML must contain one unambiguous monitors/count element",
                    ));
                }
                let text_start = end + 1;
                let next = text_start
                    + xml[text_start..]
                        .find('<')
                        .ok_or_else(|| fail("Mtt count has no closing tag"))?;
                if !xml[next..].starts_with("</count>") {
                    return Err(fail("Mtt count is not plain decimal text"));
                }
                let text = &xml[text_start..next];
                let trimmed = text.trim();
                if trimmed.is_empty() || !trimmed.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(fail("Mtt monitor count must be a positive decimal integer"));
                }
                let padding = text.len() - text.trim_start().len();
                count = Some(text_start + padding..text_start + padding + trimmed.len());
            }
            if !empty {
                stack.push(name);
            }
        }
        at = end + 1;
    }
    if !stack.is_empty() {
        return Err(fail("Mtt XML elements are not balanced"));
    }
    count.ok_or_else(|| fail("Mtt XML does not contain monitors/count"))
}

pub(super) fn expand(bytes: &[u8], minimum: u32) -> Result<Change> {
    edit_count(bytes, minimum, false)
}
pub(super) fn exact_count(bytes: &[u8], count: u32) -> Result<Change> {
    edit_count(bytes, count, true)
}
fn edit_count(bytes: &[u8], minimum: u32, exact: bool) -> Result<Change> {
    if minimum == 0 {
        return Err(fail("requested Mtt capacity must be positive"));
    }
    let (xml, encoding) = decode(bytes)?;
    let range = count_range(&xml)?;
    let original_count: u32 = xml[range.clone()]
        .parse()
        .map_err(|_| fail("Mtt monitor count exceeds the supported integer range"))?;
    if original_count == 0 {
        return Err(fail("Mtt monitor count must be positive"));
    }
    let count = if exact { minimum } else { minimum.max(original_count) };
    let mut after = bytes.to_vec();
    if count != original_count {
        // Translate the UTF-8 string index back into original encoded bytes.
        let bom = match encoding {
            Encoding::Utf8 => usize::from(bytes.starts_with(&[0xef, 0xbb, 0xbf])) * 3,
            _ => 2,
        };
        let start = bom + encode(&xml[..range.start], encoding).len();
        let end = start + encode(&xml[range], encoding).len();
        after.splice(start..end, encode(&count.to_string(), encoding));
    }
    Ok(Change {
        before: bytes.to_vec(),
        after,
        original_count,
        count,
    })
}

/// Compensation owns only the exact preimage and its own exact postimage.
pub(super) fn rollback_image<'a>(current: &[u8], change: &'a Change) -> Result<&'a [u8]> {
    if current != change.before && current != change.after {
        return Err(fail(
            "Mtt XML was changed by another writer; its bytes were not overwritten",
        ));
    }
    Ok(&change.before)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_every_mode_comment_bom_and_line_ending() {
        let source = "<?xml version=\"1.0\"?><vdd_settings>\r\n<!-- <count>999</count> -->\r\n<monitors><count> 1 </count></monitors>\r\n<resolutions><resolution><width>3840</width><height>2160</height><refresh_rate>165</refresh_rate></resolution></resolutions></vdd_settings>";
        for (encoding, bom) in [
            (Encoding::Utf8, vec![]),
            (Encoding::Utf8, vec![0xef, 0xbb, 0xbf]),
            (Encoding::Little, vec![0xff, 0xfe]),
            (Encoding::Big, vec![0xfe, 0xff]),
        ] {
            let mut bytes = bom.clone();
            bytes.extend(encode(source, encoding));
            let mut expected = bom;
            expected.extend(encode(
                &source.replace("<count> 1 </count>", "<count> 16 </count>"),
                encoding,
            ));
            let change = expand(&bytes, 16).unwrap();
            assert_eq!(change.after, expected);
            assert_eq!(change.before, bytes);
        }
    }
    #[test]
    fn growth_is_monotonic_and_idempotent() {
        let bytes = b"<vdd_settings><monitors><count>32</count></monitors></vdd_settings>";
        assert_eq!(expand(bytes, 16).unwrap().after, bytes);
        let changed = expand(bytes, 33).unwrap();
        assert_eq!(expand(&changed.after, 33).unwrap().after, changed.after);
    }
    #[test]
    fn rejects_ambiguous_count_encoding_and_entities() {
        for xml in ["<count>1</count>","<vdd_settings><monitors><count>1</count><count>1</count></monitors></vdd_settings>","<!DOCTYPE vdd_settings><vdd_settings><monitors><count>1</count></monitors></vdd_settings>","<vdd_settings><monitors><count>&one;</count></monitors></vdd_settings>","<vdd_settings><monitors><count>0</count></monitors></vdd_settings>","<vdd_settings><monitors><count>4294967296</count></monitors></vdd_settings>"] {assert!(expand(xml.as_bytes(),16).is_err(),"{xml}");}
        assert!(expand(b"\0<count>1</count>", 16).is_err());
        assert!(expand(&[0xff, 0xfe, 0], 16).is_err());
    }
    #[test]
    fn compensation_does_not_erase_a_third_party_edit() {
        let change = expand(
            b"<vdd_settings><monitors><count>1</count></monitors></vdd_settings>",
            16,
        )
        .unwrap();
        assert_eq!(
            rollback_image(&change.before, &change).unwrap(),
            change.before
        );
        assert_eq!(
            rollback_image(&change.after, &change).unwrap(),
            change.before
        );
        assert!(rollback_image(b"different modes", &change).is_err());
    }
}
