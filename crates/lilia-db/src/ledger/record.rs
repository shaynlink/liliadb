//! Stored forms of the ledger head and of ledger commits.

use lilia_core::{compute_commit_hash, LedgerAction, LedgerCommit, LedgerDeltaEntry, MerkleTree};

use super::mmr::{Hash, Peaks};
use crate::error::{corruption, Result};

const FORMAT: u8 = 1;

/// The ledger's frontier, rewritten by every ledger commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// First engine sequence the ledger attests: records with older versions predate it.
    pub start_sequence: u64,
    pub peaks: Peaks,
    /// Hash of the newest commit; zero before the first.
    pub last_commit: Hash,
}

/// One ledger commit: the record writes of one batch or transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRecord {
    /// Leaf index in the range (commit sequence minus one).
    pub index: u64,
    /// Engine sequence of the batch's first write.
    pub first_sequence: u64,
    pub timestamp_ms: i64,
    pub parent_hash: Hash,
    pub tx_root: Hash,
    pub commit_hash: Hash,
    /// Root of the range after this commit.
    pub merkle_root: Hash,
    pub deltas: Vec<LedgerDeltaEntry>,
}

impl CommitRecord {
    /// Builds the commit of `deltas` on top of `parent`.
    pub fn new(
        index: u64,
        first_sequence: u64,
        timestamp_ms: i64,
        parent_hash: Hash,
        deltas: Vec<LedgerDeltaEntry>,
    ) -> Self {
        let tx_root = tx_tree(&deltas).root();
        let commit_hash = compute_commit_hash(
            index + 1,
            timestamp_ms,
            &parent_hash,
            &tx_root,
            count(&deltas),
        );
        Self {
            index,
            first_sequence,
            timestamp_ms,
            parent_hash,
            tx_root,
            commit_hash,
            merkle_root: [0; 32],
            deltas,
        }
    }

    /// The commit header as the core ledger types describe it.
    pub fn header(&self) -> LedgerCommit {
        LedgerCommit {
            commit_seq: self.index + 1,
            timestamp_ms: self.timestamp_ms,
            parent_hash: self.parent_hash,
            tx_root: self.tx_root,
            commit_hash: self.commit_hash,
            merkle_root: self.merkle_root,
            operation_count: count(&self.deltas),
            metadata: None,
        }
    }
}

/// Number of deltas of a commit; a batch never holds `u32::MAX` writes.
pub fn count(deltas: &[LedgerDeltaEntry]) -> u32 {
    u32::try_from(deltas.len()).unwrap_or(u32::MAX)
}

/// The Merkle tree over the leaf hashes of a commit's deltas.
pub fn tx_tree(deltas: &[LedgerDeltaEntry]) -> MerkleTree {
    let leaves: Vec<Hash> = deltas
        .iter()
        .map(LedgerDeltaEntry::compute_leaf_hash)
        .collect();
    MerkleTree::build(&leaves)
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    data: &'a [u8],
    what: &'static str,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.data.len() < n {
            return Err(corruption(format!("{}: truncated", self.what)));
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn string(&mut self) -> Result<String> {
        let len = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        String::from_utf8(self.take(len)?.to_vec())
            .map_err(|_| corruption(format!("{}: text is not UTF-8", self.what)))
    }

    fn finish(&self) -> Result<()> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(corruption(format!("{}: trailing bytes", self.what)))
        }
    }
}

impl Head {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(57 + 32 * self.peaks.hashes.len());
        out.push(FORMAT);
        out.extend_from_slice(&self.start_sequence.to_le_bytes());
        out.extend_from_slice(&self.peaks.leaves.to_le_bytes());
        out.extend_from_slice(&self.last_commit);
        for hash in &self.peaks.hashes {
            out.extend_from_slice(hash);
        }
        out
    }

    /// # Errors
    ///
    /// [`crate::Error::Corruption`] for a head that does not decode or whose peaks do not match
    /// its leaf count.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut reader = Reader {
            data,
            what: "ledger head",
        };
        if reader.take(1)? != [FORMAT] {
            return Err(corruption("ledger head: unknown format"));
        }
        let start_sequence = reader.u64()?;
        let leaves = reader.u64()?;
        let last_commit = reader.array()?;
        let mut hashes = Vec::new();
        while !reader.data.is_empty() {
            hashes.push(reader.array()?);
        }
        let peaks = Peaks { leaves, hashes };
        if !peaks.is_consistent() {
            return Err(corruption("ledger head: peaks do not match the leaf count"));
        }
        Ok(Self {
            start_sequence,
            peaks,
            last_commit,
        })
    }
}

impl CommitRecord {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(170 + self.deltas.len() * 80);
        out.push(FORMAT);
        out.extend_from_slice(&self.index.to_le_bytes());
        out.extend_from_slice(&self.first_sequence.to_le_bytes());
        out.extend_from_slice(&self.timestamp_ms.to_le_bytes());
        for hash in [
            &self.parent_hash,
            &self.tx_root,
            &self.commit_hash,
            &self.merkle_root,
        ] {
            out.extend_from_slice(hash);
        }
        out.extend_from_slice(&count(&self.deltas).to_le_bytes());
        for delta in &self.deltas {
            put_bytes(&mut out, delta.model.as_bytes());
            put_bytes(&mut out, delta.space.as_bytes());
            put_bytes(&mut out, delta.id.as_bytes());
            out.extend_from_slice(&delta.version.to_le_bytes());
            out.push(match delta.action {
                LedgerAction::Put => 1,
                LedgerAction::Delete => 2,
            });
            out.extend_from_slice(&delta.payload_hash);
        }
        out
    }

    /// # Errors
    ///
    /// [`crate::Error::Corruption`] for a record that does not decode.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut reader = Reader {
            data,
            what: "ledger commit",
        };
        if reader.take(1)? != [FORMAT] {
            return Err(corruption("ledger commit: unknown format"));
        }
        let index = reader.u64()?;
        let first_sequence = reader.u64()?;
        let timestamp_ms = i64::from_le_bytes(reader.array()?);
        let (parent_hash, tx_root, commit_hash, merkle_root) = (
            reader.array()?,
            reader.array()?,
            reader.array()?,
            reader.array()?,
        );
        let count = reader.u32()?;
        let mut deltas =
            Vec::with_capacity(usize::try_from(count).unwrap_or_default().min(data.len()));
        for _ in 0..count {
            let model = reader.string()?;
            let space = reader.string()?;
            let id = reader.string()?;
            let version = reader.u64()?;
            let action = match reader.take(1)? {
                [1] => LedgerAction::Put,
                [2] => LedgerAction::Delete,
                _ => return Err(corruption("ledger commit: unknown action")),
            };
            let payload_hash = reader.array()?;
            deltas.push(LedgerDeltaEntry {
                model,
                space,
                id,
                version,
                action,
                payload_hash,
            });
        }
        reader.finish()?;
        Ok(Self {
            index,
            first_sequence,
            timestamp_ms,
            parent_hash,
            tx_root,
            commit_hash,
            merkle_root,
            deltas,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_and_commits_round_trip() {
        let mut peaks = Peaks::default();
        for i in 0..5u8 {
            peaks.append([i; 32]);
        }
        let head = Head {
            start_sequence: 9,
            peaks,
            last_commit: [7; 32],
        };
        assert_eq!(Head::decode(&head.encode()).unwrap(), head);
        let mut broken = head.encode();
        broken.truncate(broken.len() - 32);
        assert!(Head::decode(&broken).is_err(), "a missing peak is detected");

        let deltas = vec![
            LedgerDeltaEntry {
                model: "kv".into(),
                space: "ns".into(),
                id: "6b".into(),
                version: 12,
                action: LedgerAction::Put,
                payload_hash: [3; 32],
            },
            LedgerDeltaEntry {
                model: "json".into(),
                space: "users".into(),
                id: "u1".into(),
                version: 13,
                action: LedgerAction::Delete,
                payload_hash: [0; 32],
            },
        ];
        let mut commit = CommitRecord::new(4, 12, 1_700_000_000_000, [1; 32], deltas);
        commit.merkle_root = [2; 32];
        assert_eq!(CommitRecord::decode(&commit.encode()).unwrap(), commit);
        assert_eq!(commit.header().commit_seq, 5);
        let mut extra = commit.encode();
        extra.push(0);
        assert!(CommitRecord::decode(&extra).is_err());
    }
}
