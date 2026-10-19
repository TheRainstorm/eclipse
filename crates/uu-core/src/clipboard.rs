//! Bounded text clipboard wire codec, independently implemented from the
//! official Windows protobuf descriptors. TEXT is separate from input CONTROL.
use crate::control_wire::{bytes, decode_wire, envelope, int, WireValue};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};
pub const MAX_TEXT: usize = 30 * 1024;
const MAX_BYTES: usize = MAX_TEXT * 2 + 2;
#[derive(Default)]
pub struct Clipboard {
    pub available: AtomicBool,
    pub enabled: AtomicBool,
    pub active: AtomicBool,
    pub read_only: AtomicBool,
    pub stopped: AtomicBool,
    pub supported: AtomicBool,
    pub local: Mutex<Option<String>>,
    pub remote: Mutex<Option<String>>,
    pub last_sent: AtomicU64,
    pub sent: AtomicU64,
    pub received: AtomicU64,
    pub status: Mutex<String>,
    pub protocol: Mutex<Protocol>,
}
impl Clipboard {
    pub fn status(&self, text: impl Into<String>) {
        *self.status.lock().unwrap() = text.into();
    }
    pub fn ready(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
            && self.active.load(Ordering::Acquire)
            && !self.stopped.load(Ordering::Acquire)
    }
}
fn part<'a>(data: &'a [u8], field: u32) -> anyhow::Result<Option<&'a [u8]>> {
    let fields = decode_wire(data).map_err(|_|anyhow::anyhow!("invalid clipboard wire"))?;
    let mut found = None;
    for (n, value) in fields {
        if n == field {
            anyhow::ensure!(found.is_none(), "duplicate clipboard field");
            let WireValue::Bytes(value) = value else {
                anyhow::bail!("clipboard field type");
            };
            found = Some(value);
        }
    }
    Ok(found)
}
fn number(data: &[u8], field: u32) -> anyhow::Result<u64> {
    let mut result = None;
    for (n, value) in decode_wire(data).map_err(|_|anyhow::anyhow!("invalid clipboard wire"))? {
        if n == field {
            anyhow::ensure!(result.is_none(), "duplicate clipboard number");
            let WireValue::Varint(value) = value else {
                anyhow::bail!("clipboard number type");
            };
            result = Some(value);
        }
    }
    Ok(result.unwrap_or(0))
}
fn string(data: &[u8], field: u32) -> anyhow::Result<&str> {
    Ok(std::str::from_utf8(part(data, field)?.unwrap_or_default())?)
}
fn header(data: &[u8]) -> anyhow::Result<&[u8]> {
    let value = part(data, 1)?.ok_or_else(|| anyhow::anyhow!("missing clipboard header"))?;
    anyhow::ensure!(value.len() <= 32, "clipboard header limit");
    let _ = number(value, 1)?;
    Ok(value)
}
fn text(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.len() <= MAX_TEXT && !value.contains('\0'),
        "clipboard text limit or embedded NUL"
    );
    Ok(())
}
fn rpc(
    seq: u64,
    kind: u32,
    header: &[u8],
    clipboard_field: u32,
    body_kind: u32,
    body: &[u8],
) -> Vec<u8> {
    let mut clip = vec![];
    bytes(body_kind, body, &mut clip);
    let mut value = vec![];
    bytes(1, header, &mut value);
    bytes(clipboard_field, &clip, &mut value);
    envelope(seq, kind, &value)
}
pub fn is_clipboard(data:&[u8])->bool {
    part(data,17).ok().flatten().is_some() ||
    part(data,21).ok().flatten().is_some_and(|rpc|part(rpc,9).ok().flatten().is_some()||part(rpc,10).ok().flatten().is_some()) ||
    part(data,22).ok().flatten().is_some_and(|rpc|part(rpc,5).ok().flatten().is_some()||part(rpc,6).ok().flatten().is_some())
}
pub fn text_change(seq: u64, value: &str) -> anyhow::Result<Vec<u8>> {
    text(value)?;
    let mut h = vec![];
    int(1, seq, &mut h);
    let mut body = vec![];
    int(1, 1, &mut body);
    bytes(2, value.as_bytes(), &mut body);
    let mut request = vec![];
    bytes(1, &h, &mut request);
    bytes(10, &body, &mut request);
    Ok(envelope(seq, 21, &request))
}
struct Incoming {
    key: String,
    id: u64,
    created: Instant,
    count: Option<usize>,
    blocks: BTreeMap<usize, Vec<u8>>,
}
struct Fragments {
    id: u64,
    value: String,
    created: Instant,
}
#[derive(Default)]
pub struct Protocol {
    incoming: Option<Incoming>,
    fragments: Option<Fragments>,
}
#[derive(Default)]
pub struct Received {
    pub replies: Vec<Vec<u8>>,
    pub text: Option<String>,
    pub acknowledged: Option<u64>,
}
impl Protocol {
    pub fn cancel_pending(&mut self){self.incoming=None;self.fragments=None;}
    pub fn receive(&mut self, data: &[u8], sequence: &AtomicU64) -> anyhow::Result<Received> {
        let result = self.receive_inner(data, sequence);
        if result.is_err() {
            self.incoming = None;
            self.fragments = None;
        }
        result
    }
    fn receive_inner(&mut self, data: &[u8], sequence: &AtomicU64) -> anyhow::Result<Received> {
        anyhow::ensure!(data.len() <= MAX_BYTES + 2048, "clipboard message limit");
        let mut out = Received::default();
        let next = || sequence.fetch_add(1, Ordering::Relaxed);
        if self
            .incoming
            .as_ref()
            .is_some_and(|v| v.created.elapsed() > Duration::from_secs(5))
        {
            self.incoming = None;
        }
        if let Some(change) = part(data, 17)? {
            let id = number(change, 1)?;
            let value = string(change, 2)?;
            let more = number(change, 3)?;
            anyhow::ensure!(more <= 1, "clipboard continuation flag");
            text(value)?;
            let mut fragments = self
                .fragments
                .take()
                .filter(|f| f.id == id && f.created.elapsed() < Duration::from_secs(5))
                .unwrap_or(Fragments {
                    id,
                    value: String::new(),
                    created: Instant::now(),
                });
            fragments.value.push_str(value);
            text(&fragments.value)?;
            if more == 1 {
                self.fragments = Some(fragments);
            } else {
                out.text = Some(fragments.value);
            }
            return Ok(out);
        }
        if let Some(request) = part(data, 21)? {
            let h = header(request)?;
            if let Some(change) = part(request, 10)? {
                anyhow::ensure!(number(change, 1)? == 1, "unsupported clipboard text format");
                let value = string(change, 2)?;
                text(value)?;
                out.text = Some(value.into());
                let mut response = vec![];
                bytes(1, h, &mut response);
                bytes(6, &[8, 1], &mut response);
                out.replies.push(envelope(next(), 22, &response));
                return Ok(out);
            }
            if let Some(clip) = part(request, 9)? {
                if let Some(list) = part(clip, 1)? {
                    out.replies.push(rpc(next(), 22, h, 5, 1, &[]));
                    self.incoming = None;
                    let mut unicode = false;
                    for (n, value) in decode_wire(list).map_err(|_|anyhow::anyhow!("invalid clipboard format list"))?.into_iter().take(64) {
                        if let (1, WireValue::Bytes(format)) = (n, value) {
                            if number(format, 1)? == 13 {
                                unicode = true;
                            }
                        }
                    }
                    if unicode {
                        let id = next();
                        let key = uuid::Uuid::new_v4().to_string();
                        let mut ask = vec![];
                        int(1, 13, &mut ask);
                        bytes(2, key.as_bytes(), &mut ask);
                        bytes(3, b"CF_UNICODETEXT", &mut ask);
                        let mut ask_header = vec![];
                        int(1, id, &mut ask_header);
                        out.replies.push(rpc(next(), 21, &ask_header, 9, 2, &ask));
                        self.incoming = Some(Incoming {
                            key,
                            id,
                            created: Instant::now(),
                            count: None,
                            blocks: BTreeMap::new(),
                        });
                    }
                } else if let Some(block) = part(clip, 3)? {
                    let key = string(block, 1)?;
                    let index = usize::try_from(number(block, 2)?)?;
                    let value = part(block, 3)?.unwrap_or_default();
                    anyhow::ensure!(
                        index <= 8 && value.len() <= MAX_BYTES,
                        "clipboard block limit"
                    );
                    let incoming = self
                        .incoming
                        .as_mut()
                        .ok_or_else(|| anyhow::anyhow!("unsolicited clipboard block"))?;
                    anyhow::ensure!(key == incoming.key, "clipboard block key mismatch");
                    if let Some(previous) = incoming.blocks.get(&index) {
                        anyhow::ensure!(previous == value, "conflicting clipboard block");
                    } else {
                        anyhow::ensure!(
                            incoming.blocks.values().map(Vec::len).sum::<usize>() + value.len()
                                <= MAX_BYTES,
                            "clipboard assembly limit"
                        );
                        incoming.blocks.insert(index, value.to_vec());
                    }
                    let mut confirm = vec![];
                    bytes(1, key.as_bytes(), &mut confirm);
                    int(2, index as u64, &mut confirm);
                    int(3, 1, &mut confirm);
                    out.replies.push(rpc(next(), 22, h, 5, 3, &confirm));
                    out.text = self.complete()?;
                } else if let Some(ask) = part(clip, 2)? {
                    // We use Windows' direct text-change RPC and do not offer
                    // delayed-rendering placeholders or files/images.
                    let key = string(ask, 2)?;
                    anyhow::ensure!(key.len() <= 128, "clipboard key limit");
                    let mut failure = vec![];
                    int(1, 2, &mut failure);
                    bytes(2, key.as_bytes(), &mut failure);
                    out.replies.push(rpc(next(), 22, h, 5, 2, &failure));
                }
            }
        }
        if let Some(response) = part(data, 22)? {
            if let Some(confirmation) = part(response, 6)? {
                if number(confirmation,1)?==1 {out.acknowledged=Some(number(header(response)?,1)?);}
            }
            if let Some(clip) = part(response, 5)? {
                if let Some(confirm) = part(clip, 2)? {
                    if let Some(incoming) = self.incoming.as_mut() {
                        if number(header(response)?, 1)? != incoming.id
                            || string(confirm, 2)? != incoming.key
                        {
                            return Ok(out);
                        }
                        if number(confirm, 1)? != 1 {
                            self.incoming = None;
                            return Ok(out);
                        }
                        let count = usize::try_from(number(confirm, 3)?)?;
                        anyhow::ensure!((1..=8).contains(&count), "clipboard block count limit");
                        incoming.count = Some(count);
                        out.text = self.complete()?;
                    }
                }
            }
        }
        Ok(out)
    }
    fn complete(&mut self) -> anyhow::Result<Option<String>> {
        let Some(incoming) = self.incoming.as_ref() else {
            return Ok(None);
        };
        let Some(count) = incoming.count else {
            return Ok(None);
        };
        let first = if (0..count).all(|i| incoming.blocks.contains_key(&i)) {
            0
        } else if (1..=count).all(|i| incoming.blocks.contains_key(&i)) {
            1
        } else {
            return Ok(None);
        };
        let bytes: Vec<u8> = (first..first + count)
            .flat_map(|i| incoming.blocks[&i].iter().copied())
            .collect();
        self.incoming = None;
        anyhow::ensure!(
            bytes.len() % 2 == 0 && bytes.len() <= MAX_BYTES,
            "invalid clipboard Unicode length"
        );
        let mut units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|v| u16::from_le_bytes([v[0], v[1]]))
            .collect();
        if units.last() == Some(&0) {
            units.pop();
        }
        let value = String::from_utf16(&units)?;
        text(&value)?;
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn offer() -> Vec<u8> {
        let mut format = vec![];
        int(1, 13, &mut format);
        let mut list = vec![];
        bytes(1, &format, &mut list);
        rpc(1, 21, &[8, 1], 9, 1, &list)
    }
    #[test]
    fn windows_unicode_offer_asks_for_data_and_waits_for_confirm_in_either_block_order() {
        for base in [0, 1] {
            let mut state = Protocol::default();
            let seq = AtomicU64::new(100);
            let first = state.receive(&offer(), &seq).unwrap();
            assert_eq!(first.replies.len(), 2);
            let pending = state.incoming.as_ref().unwrap();
            let key = pending.key.clone();
            let id = pending.id;
            let value = "中文🙂\r\ntext";
            let data: Vec<u8> = value
                .encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes)
                .collect();
            let mut block = vec![];
            bytes(1, key.as_bytes(), &mut block);
            int(2, base, &mut block);
            bytes(3, &data, &mut block);
            assert!(state
                .receive(&rpc(2, 21, &[8, 2], 9, 3, &block), &seq)
                .unwrap()
                .text
                .is_none());
            let mut confirm = vec![];
            int(1, 1, &mut confirm);
            bytes(2, key.as_bytes(), &mut confirm);
            int(3, 1, &mut confirm);
            let mut h = vec![];
            int(1, id, &mut h);
            assert_eq!(
                state
                    .receive(&rpc(3, 22, &h, 5, 2, &confirm), &seq)
                    .unwrap()
                    .text
                    .as_deref(),
                Some(value)
            );
        }
    }
    #[test]
    fn direct_text_change_roundtrip_and_limits_are_enforced_without_real_clipboard() {
        let mut state = Protocol::default();
        let seq = AtomicU64::new(100);
        let result = state
            .receive(&text_change(42, "text中文").unwrap(), &seq)
            .unwrap();
        assert_eq!(result.text.as_deref(), Some("text中文"));
        assert_eq!(state.receive(&result.replies[0],&seq).unwrap().acknowledged,Some(42));
        assert!(text_change(1, "a\0b").is_err());
        assert!(text_change(1, &"a".repeat(MAX_TEXT + 1)).is_err());
        assert!(state
            .receive(&rpc(3, 21, &[8, 3], 9, 3, &[10, 1, b'x']), &seq)
            .is_err());
    }
    #[test]
    fn incomplete_or_malformed_fragments_do_not_replace_clipboard() {
        let mut state = Protocol::default();
        let seq = AtomicU64::new(100);
        let wrap = |id, value: &str, more| {
            let mut m = vec![];
            int(1, id, &mut m);
            bytes(2, value.as_bytes(), &mut m);
            int(3, more, &mut m);
            envelope(1, 17, &m)
        };
        assert!(state
            .receive(&wrap(1, "old", 1), &seq)
            .unwrap()
            .text
            .is_none());
        assert_eq!(
            state
                .receive(&wrap(2, "new", 0), &seq)
                .unwrap()
                .text
                .as_deref(),
            Some("new")
        );
        assert!(state.receive(&wrap(2, "invalid", 2), &seq).is_err());
    }
}
