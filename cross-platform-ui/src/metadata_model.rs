use crate::protocol::MetadataChunk;
use std::collections::BTreeMap;

/// Reassembles chunked M replies (kind/part/of/reply/payload).
#[derive(Default, Debug)]
pub struct MetadataAssembler {
    pending: BTreeMap<(String, i64, i64), BTreeMap<i64, String>>, // (kind,reply,of) -> part->payload
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataReply {
    pub kind: String,
    pub reply: i64,
    pub payload: String,
}

impl MetadataAssembler {
    pub fn feed(&mut self, chunk: MetadataChunk) -> Option<MetadataReply> {
        if chunk.of <= 0 { return None; }
        let key = (chunk.kind.clone(), chunk.reply, chunk.of);
        let parts = self.pending.entry(key.clone()).or_default();
        parts.insert(chunk.part, chunk.payload);
        if parts.len() as i64 >= chunk.of {
            let mut payload = String::new();
            for i in 1..=chunk.of {
                if let Some(p) = parts.get(&i) { payload.push_str(p); }
            }
            self.pending.remove(&key);
            return Some(MetadataReply { kind: chunk.kind, reply: chunk.reply, payload });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::MetadataChunk;

    #[test]
    fn reassembles_chunks() {
        let mut a = MetadataAssembler::default();
        assert!(a.feed(MetadataChunk { kind: "i".into(), part: 1, of: 2, reply: 1, payload: "hel".into() }).is_none());
        let done = a.feed(MetadataChunk { kind: "i".into(), part: 2, of: 2, reply: 1, payload: "lo".into() }).unwrap();
        assert_eq!(done.payload, "hello");
        assert_eq!(done.kind, "i");
    }
}
