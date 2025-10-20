//! Sharder implementation.
use md5;

/// A sharder that has part of its hash pre-computed with the given salt.
#[derive(Clone)]
pub struct PreSaltedSharder {
    ctx: md5::Context,
    total_shards: u32,
}

impl PreSaltedSharder {
    pub fn new(salt: &[impl AsRef<[u8]>], total_shards: u32) -> PreSaltedSharder {
        let mut ctx = md5::Context::new();
        for s in salt {
            ctx.consume(s);
        }
        PreSaltedSharder { ctx, total_shards }
    }

    pub fn shard(&self, input: &[impl AsRef<[u8]>]) -> u32 {
        let mut ctx = self.ctx.clone();
        for i in input {
            ctx.consume(i);
        }
        let hash = ctx.compute();
        let value = u32::from_be_bytes(hash[0..4].try_into().unwrap());
        value % self.total_shards
    }
}

impl std::fmt::Debug for PreSaltedSharder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreSaltedSharder")
            .field("ctx", &"...")
            .field("total_shards", &self.total_shards)
            .finish()
    }
}

/// Compute md5 shard for the set of inputs.
///
/// This function accepts an array of inputs to allow the caller to avoid allocating memory when
/// input is compound from multiple segments.
pub fn get_md5_shard(input: &[impl AsRef<[u8]>], total_shards: u32) -> u32 {
    let hash = {
        let mut hasher = md5::Context::new();
        for i in input {
            hasher.consume(i);
        }
        hasher.compute()
    };
    let value = u32::from_be_bytes(hash[0..4].try_into().unwrap());
    value % total_shards
}
