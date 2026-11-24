//! Shared Argon2id v0x13 work and charge calculation.
use argon2::{Algorithm, Argon2, AssociatedData, Block, Params, ParamsBuilder, Version};
use zeroize::Zeroize;

#[derive(Debug)]
pub enum Error {
    Params,
    HostMemory,
}

pub struct Plan {
    builder: ParamsBuilder,
    blocks: usize,
    passes: u32,
}

impl Plan {
    /// Validate the public cost and output length before charging any work.
    pub fn new(memory_kib: u32, passes: u32, lanes: u32, len: u64) -> Result<Self, Error> {
        let len = usize::try_from(len).map_err(|_| Error::Params)?;
        let mut builder = ParamsBuilder::new();
        builder
            .m_cost(memory_kib)
            .t_cost(passes)
            .p_cost(lanes)
            .output_len(len);
        let params = builder.build().map_err(|_| Error::Params)?;
        Ok(Self {
            builder,
            blocks: params.block_count(),
            passes,
        })
    }

    pub fn steps(&self) -> u64 {
        u64::from(self.passes) * self.blocks as u64
    }

    pub fn memory_bytes(&self) -> u64 {
        Block::SIZE as u64 * self.blocks as u64
    }

    /// Hash into an exact-length output after the caller has charged steps and memory.
    pub fn hash(
        mut self,
        password: &[u8],
        salt: &[u8],
        key: &[u8],
        data: &[u8],
        out: &mut [u8],
    ) -> Result<(), Error> {
        if !valid_input_lengths([password, salt, key, data].map(|b| b.len() as u64)) {
            return Err(Error::Params);
        }
        self.builder
            .data(AssociatedData::new(data).map_err(|_| Error::Params)?);
        let params = self.builder.build().map_err(|_| Error::Params)?;
        let algorithm = Algorithm::Argon2id;
        let version = Version::V0x13;
        let hasher = if key.is_empty() {
            Argon2::new(algorithm, version, params)
        } else {
            Argon2::new_with_secret(key, algorithm, version, params).map_err(|_| Error::Params)?
        };
        let mut blocks = WorkingMemory(Vec::new());
        blocks
            .0
            .try_reserve_exact(self.blocks)
            .map_err(|_| Error::HostMemory)?;
        blocks.0.resize(self.blocks, Block::new());
        hasher
            .hash_password_into_with_memory(password, salt, out, &mut blocks.0)
            .map_err(|_| Error::Params)
    }
}

pub fn valid_input_lengths([password, salt, key, data]: [u64; 4]) -> bool {
    password <= argon2::MAX_PWD_LEN as u64
        && (argon2::MIN_SALT_LEN as u64..=argon2::MAX_SALT_LEN as u64).contains(&salt)
        && key <= argon2::MAX_SECRET_LEN as u64
        && data <= Params::MAX_DATA_LEN as u64
}

struct WorkingMemory(Vec<Block>);

impl Drop for WorkingMemory {
    fn drop(&mut self) {
        for block in &mut self.0 {
            block.as_mut().zeroize();
        }
    }
}
