use crate::common::constants;

pub type PageId = u64; // Logical Page Id
pub type Lsn = u64; // Log Sequence Number

#[derive(Copy, Clone, Debug)]
pub struct PhysicalId {
    pub file: u32,
    pub offset: u64,
}

pub type PageBuffer = [u8; constants::PAGE_BUF_SIZE];
