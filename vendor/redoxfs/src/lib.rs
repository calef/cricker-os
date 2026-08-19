#![crate_name = "redoxfs"]
#![crate_type = "lib"]
#![cfg_attr(not(feature = "std"), no_std)]
// Used often in generating redox_syscall errors
#![allow(clippy::or_fun_call)]
#![allow(unexpected_cfgs)]

extern crate alloc;

use core::sync::atomic::AtomicUsize;

// The alloc log grows by 1 block about every 21 generations
pub const ALLOC_GC_THRESHOLD: u64 = 1024;
pub const BLOCK_SIZE: u64 = 4096;
// nife pin divergence (milestone 138): **the level a new file is created at**, lowered from
// upstream's 5 to 1, so a record is 4 KiB << 1 = 8 KiB rather than 128 KiB. Every file request in
// this system carries at most one 4 KiB page (`fs_proto::PAGE`), so a 128 KiB record fetched 32
// blocks to serve one of them and rewrote all 32 to change one. Measured on milestone 38's harness:
// a 4 KiB read goes from 1,458 us to 284 us. Level 1 rather than 0 because RedoxFS compresses a
// record only when it is larger than one block, so level 0 gives up lz4 for 8.7% more read speed
// and roughly twice the space overhead. See notes/benchmarks.md and vendor/README.md divergence 5.
pub const RECORD_LEVEL: usize = 1;

// nife pin divergence (milestone 138): **the largest level this build can read.** Upstream needed
// only one constant because the level a new file is CREATED at and the largest level a build can
// READ were the same number by construction. They stop being the same the moment the first is
// lowered, and the difference is not cosmetic: `record_level` is a per-node field in the on-disk
// format, so an image written by any other build carries whatever level that build chose, and the
// two `BlockTrait::empty` guards (`record.rs`, `htree.rs`) refuse a level above this constant,
// which `read_block` turns into `ENOENT` on a perfectly good image.
//
// Holding the ceiling at upstream's 5 is what makes lowering the default **reversible**. Nothing
// stored at any level from 0 to 5 becomes unreadable, and a future change to `RECORD_LEVEL` cannot
// orphan data written by this one. It is also half of what a genuine per-file level would need: the
// guards already compare against a maximum rather than against the default.
pub const RECORD_LEVEL_MAX: usize = 5;

// nife pin divergence (milestone 138): the **ceiling** rather than the default, because the one
// thing this sizes is the lz4 scratch buffer in `filesystem.rs`, which has to fit any record this
// build might rewrite, including one an older image created at level 5.
pub const RECORD_SIZE: u64 = BLOCK_SIZE << RECORD_LEVEL_MAX;
pub const SIGNATURE: &[u8; 8] = b"RedoxFS\0";
pub const VERSION: u64 = 8;
pub const DIR_ENTRY_MAX_LENGTH: usize = 252;

pub static IS_UMT: AtomicUsize = AtomicUsize::new(0);

pub use self::allocator::{AllocEntry, AllocList, Allocator, ReleaseList, ALLOC_LIST_ENTRIES};
#[cfg(feature = "std")]
pub use self::archive::{archive, archive_at};
pub use self::block::{
    BlockAddr, BlockData, BlockLevel, BlockList, BlockMeta, BlockPtr, BlockRaw, BlockTrait,
};
#[cfg(feature = "std")]
pub use self::clone::clone;
pub use self::dir::{DirEntry, DirList};
pub use self::disk::*;
pub use self::filesystem::FileSystem;
pub use self::header::{Header, HEADER_RING};
pub use self::key::{Key, KeySlot, Salt};
#[cfg(feature = "std")]
pub use self::mount::mount;
pub use self::node::{Node, NodeFlags, NodeLevel, NodeLevelData};
pub use self::record::RecordRaw;
pub use self::transaction::Transaction;
pub use self::tree::{Tree, TreeData, TreeList, TreePtr};
#[cfg(feature = "std")]
pub use self::unmount::unmount_path;

mod allocator;
#[cfg(feature = "std")]
mod archive;
mod block;
#[cfg(feature = "std")]
mod clone;
mod dir;
mod disk;
mod filesystem;
mod header;
mod htree;
mod key;
#[cfg(all(feature = "std", not(fuzzing)))]
mod mount;
#[cfg(all(feature = "std", fuzzing))]
pub mod mount;
mod node;
mod record;
mod transaction;
mod tree;
#[cfg(feature = "std")]
mod unmount;

#[cfg(all(feature = "std", test))]
mod tests;
