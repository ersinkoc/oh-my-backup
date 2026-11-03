//! Sidecar describing which blocks of a local file are known-good.
//! `hashes[i] == ZERO` means "block i content unknown/invalid".
//! Invariant: every non-zero hash equals the hash of local block i, and that block equals block i
//! of the remote version (remote_size, remote_mtime).

use crate::config::write_atomic;
use crate::i18n;
use crate::remote::{BlockHash, HASH_LEN};
use anyhow::{Result, bail};
use std::path::Path;

pub const ZERO: BlockHash = [0u8; HASH_LEN];
pub const BLOCK: u64 = 4 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"HBSBLK01";

#[derive(Clone, Debug)]
pub struct Blk {
    pub block: u64,
    pub remote_size: u64,
    pub remote_mtime: i64,
    pub hashes: Vec<BlockHash>,
}

pub fn block_count(size: u64, block: u64) -> usize {
    size.div_ceil(block) as usize
}

impl Blk {
    pub fn new(remote_size: u64, remote_mtime: i64) -> Blk {
        Blk {
            block: BLOCK,
            remote_size,
            remote_mtime,
            hashes: vec![ZERO; block_count(remote_size, BLOCK)],
        }
    }

    pub fn load(p: &Path) -> Result<Blk> {
        let b = std::fs::read(p)?;
        if b.len() < 40 || &b[..8] != MAGIC {
            bail!(
                "{}",
                i18n::choose("bozuk blk".to_string(), "corrupt block sidecar".to_string())
            );
        }
        let u = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let block = u(8);
        let remote_size = u(16);
        let remote_mtime = u(24) as i64;
        let count = u(32) as usize;
        if count != (b.len() - 40) / HASH_LEN
            || (b.len() - 40) % HASH_LEN != 0
            || block == 0
            || count != block_count(remote_size, block)
        {
            bail!(
                "{}",
                i18n::choose("bozuk blk".to_string(), "corrupt block sidecar".to_string())
            );
        }
        let hashes = b[40..]
            .chunks_exact(HASH_LEN)
            .map(|c| c.try_into().unwrap())
            .collect();
        Ok(Blk {
            block,
            remote_size,
            remote_mtime,
            hashes,
        })
    }

    pub fn save(&self, p: &Path) -> Result<()> {
        let mut b = Vec::with_capacity(40 + self.hashes.len() * HASH_LEN);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&self.block.to_le_bytes());
        b.extend_from_slice(&self.remote_size.to_le_bytes());
        b.extend_from_slice(&(self.remote_mtime as u64).to_le_bytes());
        b.extend_from_slice(&(self.hashes.len() as u64).to_le_bytes());
        for h in &self.hashes {
            b.extend_from_slice(h);
        }
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        write_atomic(p, &b)
    }

    pub fn valid_count(&self) -> usize {
        self.hashes.iter().filter(|h| **h != ZERO).count()
    }

    pub fn complete(&self) -> bool {
        self.hashes.iter().all(|h| *h != ZERO)
    }

    pub fn block_len(&self, i: usize) -> u64 {
        let off = i as u64 * self.block;
        (self.remote_size - off).min(self.block)
    }

    /// Re-target to a new remote version (size may change). Keeps hashes of overlapping full blocks.
    pub fn retarget(&mut self, size: u64, mtime: i64) {
        let n = block_count(size, self.block);
        let old_size = self.remote_size;
        self.hashes.resize(n, ZERO);
        // a block whose length changes (old/new tail) cannot be trusted
        let old_last = block_count(old_size, self.block);
        if old_last > 0 && old_last <= n && !old_size.is_multiple_of(self.block) {
            self.hashes[old_last - 1] = ZERO;
        }
        if n > 0 && !size.is_multiple_of(self.block) {
            self.hashes[n - 1] = ZERO;
        }
        self.remote_size = size;
        self.remote_mtime = mtime;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupt_block_count_returns_error_instead_of_panicking() {
        let p = std::env::temp_dir().join(format!("hbsync-blk-proof-{}.blk", std::process::id()));
        Blk::new(1, 0).save(&p).unwrap();
        assert!(Blk::load(&p).is_ok(), "unaffected control: valid sidecar");
        let mut bytes = std::fs::read(&p).unwrap();
        bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&p, &bytes).unwrap();
        let actual = std::panic::catch_unwind(|| Blk::load(&p));
        std::fs::remove_file(&p).unwrap();
        println!(
            "EXPECTED: Err without panic; ACTUAL: panicked={}",
            actual.is_err()
        );
        assert!(actual.is_ok(), "PROBLEM CONFIRMED: corrupt count panics");
        assert!(actual.unwrap().is_err());
        for bad in [vec![], b"HBSBLK01".to_vec(), vec![0; 40]] {
            std::fs::write(&p, bad).unwrap();
            assert!(Blk::load(&p).is_err());
        }
        std::fs::remove_file(p).unwrap();
        println!("PROBLEM NOT REPRODUCED; FIX VERIFIED");
    }
}
