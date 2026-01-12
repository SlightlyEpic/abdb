use crate::common::constants;

/// Logical Page Id
/// Resolved to PhysicalId by page directory
pub type LPageId = u32;

/// The index of a slot within a heap page
pub type SlotId = u16;

/// Log Sequence Number
pub type Lsn = u64;

/// Logical File Id
/// This contains all the data required to obtain a file name
pub type FileId = u32;

/// Physical Page Id
/// Points to an aligned offset in a file
#[derive(Copy, Clone, Debug)]
pub struct PPageId {
    pub file: FileId,
    pub offset: u64,
}

/// Points to a logical tuple
#[derive(Copy, Clone, Debug)]
pub struct RecordId {
    pub page_id: LPageId,
    pub slot_id: SlotId,
}

pub type PageBuffer = [u8; constants::PAGE_BUF_SIZE];

/// Directory Page Id
/// Offset = DirPageId * PAGE_SIZE
pub type DirPageId = u32;

pub type TxnId = u64;

pub type OId = u32;

