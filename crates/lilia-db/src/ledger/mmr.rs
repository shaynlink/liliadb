//! Merkle Mountain Range: an append-only Merkle accumulator.
//!
//! Nodes are numbered in post-order from 0: each leaf is followed by the parents it completes.
//! The range is a list of perfect trees (its *peaks*), one per set bit of the leaf count, highest
//! first. Appending a leaf writes the leaf and at most `log2(n)` parents; proving a leaf reads
//! the `log2(n)` siblings on the way to its peak. The root binds the leaf count and every peak.

use lilia_core::ledger::AuditHop;
use lilia_core::{hash_internal_node, verify_audit_path};
use serde::{Deserialize, Serialize};

pub type Hash = [u8; 32];

/// Domain of the root: distinct from leaves (`0x00`), nodes (`0x01`) and commits (`0x02`).
const DOMAIN_ROOT: u8 = 0x03;

/// Number of nodes of a range of `leaves` leaves.
#[must_use]
pub const fn size(leaves: u64) -> u64 {
    2 * leaves - leaves.count_ones() as u64
}

/// Position of leaf `index`.
#[must_use]
pub const fn leaf_position(index: u64) -> u64 {
    size(index)
}

/// Height of the node at `position` (0 for leaves).
#[must_use]
pub fn height(position: u64) -> u32 {
    // In 1-based post-order numbering, the nodes of height h at the left edge are 2^(h+1) - 1
    // (all ones); every other node is found by jumping left over whole perfect trees.
    let mut p = position + 1;
    while p & (p + 1) != 0 {
        let top = 1u64 << p.ilog2();
        p -= top - 1;
    }
    p.ilog2()
}

/// Positions of the peaks of a range of `leaves` leaves, highest (leftmost) first.
#[must_use]
pub fn peak_positions(leaves: u64) -> Vec<u64> {
    let mut peaks = Vec::new();
    let mut offset = 0;
    for h in (0..64).rev() {
        if leaves >> h & 1 == 1 {
            let tree = (1u64 << (h + 1)) - 1;
            peaks.push(offset + tree - 1);
            offset += tree;
        }
    }
    peaks
}

/// The root of a range from its leaf count and peaks.
#[must_use]
pub fn bag(leaves: u64, peaks: &[Hash]) -> Hash {
    if leaves == 0 {
        return [0; 32];
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[DOMAIN_ROOT]);
    hasher.update(&leaves.to_be_bytes());
    for peak in peaks {
        hasher.update(peak);
    }
    *hasher.finalize().as_bytes()
}

/// The frontier of a range: its leaf count and peak hashes, enough to append and to compute
/// the root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Peaks {
    pub leaves: u64,
    pub hashes: Vec<Hash>,
}

impl Peaks {
    /// Whether the peak count matches the leaf count.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.hashes.len() == self.leaves.count_ones() as usize
    }

    /// Appends `leaf`; returns the nodes written, with their positions.
    pub fn append(&mut self, leaf: Hash) -> Vec<(u64, Hash)> {
        let mut position = size(self.leaves);
        let mut hash = leaf;
        let mut nodes = vec![(position, hash)];
        // Each trailing one of the leaf count is a perfect tree of the same height to merge.
        for _ in 0..self.leaves.trailing_ones() {
            let Some(left) = self.hashes.pop() else { break };
            hash = hash_internal_node(&left, &hash);
            position += 1;
            nodes.push((position, hash));
        }
        self.hashes.push(hash);
        self.leaves += 1;
        nodes
    }

    #[must_use]
    pub fn root(&self) -> Hash {
        bag(self.leaves, &self.hashes)
    }
}

/// Positions of the siblings from leaf `index` up to its peak, in a range of `leaves` leaves,
/// each with whether the sibling is on the left; and the index of that peak.
#[must_use]
pub fn proof_positions(index: u64, leaves: u64) -> Option<(Vec<(u64, bool)>, usize)> {
    if index >= leaves {
        return None;
    }
    let peaks = peak_positions(leaves);
    let mut position = leaf_position(index);
    let mut h = 0u32;
    let mut path = Vec::new();
    loop {
        if let Some(peak) = peaks.iter().position(|&p| p == position) {
            return Some((path, peak));
        }
        let subtree = (1u64 << (h + 1)) - 1;
        if height(position + 1) == h + 1 {
            path.push((position - subtree, true));
            position += 1;
        } else {
            path.push((position + subtree, false));
            position += subtree + 1;
        }
        h += 1;
    }
}

mod hex_hashes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(hashes: &[[u8; 32]], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(hashes.iter().map(hex::encode))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<[u8; 32]>, D::Error> {
        let texts = Vec::<String>::deserialize(deserializer)?;
        texts
            .iter()
            .map(|text| {
                let bytes = hex::decode(text).map_err(serde::de::Error::custom)?;
                <[u8; 32]>::try_from(bytes.as_slice())
                    .map_err(|_| serde::de::Error::custom("a hash has 32 bytes"))
            })
            .collect()
    }
}

/// Proof that a leaf is in a range: the siblings up to its peak, and every peak.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MmrProof {
    pub leaf_index: u64,
    pub leaves: u64,
    pub path: Vec<AuditHop>,
    #[serde(with = "hex_hashes")]
    pub peaks: Vec<Hash>,
}

impl MmrProof {
    /// The root this proof yields for `leaf`; `None` if the proof is malformed or `leaf` does
    /// not hash up to its peak.
    #[must_use]
    pub fn root_for(&self, leaf: &Hash) -> Option<Hash> {
        let (path, peak) = proof_positions(self.leaf_index, self.leaves)?;
        if self.peaks.len() != self.leaves.count_ones() as usize
            || path.len() != self.path.len()
            || path
                .iter()
                .zip(&self.path)
                .any(|(&(_, left), hop)| left != hop.is_left)
        {
            return None;
        }
        (verify_audit_path(leaf, &self.path) == self.peaks[peak])
            .then(|| bag(self.leaves, &self.peaks))
    }

    #[must_use]
    pub fn verify(&self, leaf: &Hash, root: &Hash) -> bool {
        self.root_for(leaf).is_some_and(|r| &r == root)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn leaf(i: u64) -> Hash {
        *blake3::hash(&i.to_le_bytes()).as_bytes()
    }

    /// Builds the range by brute force: perfect trees over the binary decomposition of the
    /// leaf count.
    fn reference(leaves: u64) -> (HashMap<u64, Hash>, Vec<Hash>) {
        fn build(nodes: &mut HashMap<u64, Hash>, position: &mut u64, first: u64, h: u32) -> Hash {
            if h == 0 {
                let hash = leaf(first);
                nodes.insert(*position, hash);
                *position += 1;
                return hash;
            }
            let left = build(nodes, position, first, h - 1);
            let right = build(nodes, position, first + (1 << (h - 1)), h - 1);
            let hash = hash_internal_node(&left, &right);
            nodes.insert(*position, hash);
            *position += 1;
            hash
        }

        let mut nodes = HashMap::new();
        let mut peaks = Vec::new();
        let mut position = 0;
        let mut first_leaf = 0;
        for h in (0..64).rev() {
            if leaves >> h & 1 == 0 {
                continue;
            }
            peaks.push(build(&mut nodes, &mut position, first_leaf, h));
            first_leaf += 1 << h;
        }
        (nodes, peaks)
    }

    #[test]
    fn appends_match_the_reference_tree_and_proofs_verify() {
        let mut range = Peaks::default();
        let mut stored = HashMap::new();
        for n in 1..=70u64 {
            for (position, hash) in range.append(leaf(n - 1)) {
                assert!(
                    stored.insert(position, hash).is_none(),
                    "position {position} written twice"
                );
            }
            let (nodes, peaks) = reference(n);
            assert_eq!(stored, nodes, "nodes after {n} leaves");
            assert_eq!(range.hashes, peaks);
            assert_eq!(size(n), nodes.len() as u64);
            assert_eq!(
                peak_positions(n)
                    .iter()
                    .map(|p| nodes[p])
                    .collect::<Vec<_>>(),
                peaks
            );
            for i in 0..n {
                let (positions, _) = proof_positions(i, n).unwrap();
                let proof = MmrProof {
                    leaf_index: i,
                    leaves: n,
                    path: positions
                        .iter()
                        .map(|&(p, is_left)| AuditHop {
                            sibling_hash: stored[&p],
                            is_left,
                        })
                        .collect(),
                    peaks: range.hashes.clone(),
                };
                assert!(proof.verify(&leaf(i), &range.root()), "leaf {i} of {n}");
                assert!(!proof.verify(&leaf(i + 1), &range.root()));
                let mut moved = proof.clone();
                moved.leaf_index = (i + 1) % n;
                assert!(!moved.verify(&leaf(i), &range.root()) || n == 1);
            }
        }
        assert_eq!(bag(0, &[]), [0; 32]);
    }

    #[test]
    fn heights_follow_post_order() {
        let expected = [0, 0, 1, 0, 0, 1, 2, 0, 0, 1, 0, 0, 1, 2, 3, 0];
        for (position, &h) in expected.iter().enumerate() {
            assert_eq!(height(position as u64), h, "position {position}");
        }
    }
}
