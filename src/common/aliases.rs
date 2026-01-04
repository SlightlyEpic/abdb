use crate::common::constants;

/// Logical Page Id
/// Resolved to PhysicalId by page directory
pub type PageId = u64;

/// Log Sequence Number
pub type Lsn = u64;

/// Logical File Id
/// This contains all the data required to obtain a file name
pub type FileId = u32;

#[derive(Copy, Clone, Debug)]
pub struct PhysicalId {
    pub file: FileId,
    pub offset: u64,
}

pub type PageBuffer = [u8; constants::PAGE_BUF_SIZE];

/// Directory Page Id
/// Offset = DirPageId * PAGE_SIZE
pub type DirPageId = u32;
