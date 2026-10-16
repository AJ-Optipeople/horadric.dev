//! The memory's backup: encrypted copies of the store's text files in a
//! vault, and a restore from them. See docs/plan/backup.md.

pub mod vault;

pub use vault::{MemoryVault, Vault};
