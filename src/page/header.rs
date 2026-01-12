use crate::common::aliases::{Lsn, LPageId};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageType {
    Unused = 0,
    HeapPage = 1, // Table data
    BTreeInner = 2,
    BTreeLeaf = 3,
    HeapFileHeader = 32,
    IndexFileHeader = 33,
    DirectoryFileHeader = 34,
    DirectoryInner = 64,
    DirectoryLeaf = 65,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPageTypeError(pub u8);

impl std::fmt::Display for InvalidPageTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid PageType identifier: {}", self.0)
    }
}

impl TryFrom<u8> for PageType {
    type Error = InvalidPageTypeError;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(Self::Unused),
            1 => Ok(Self::HeapPage),
            2 => Ok(Self::BTreeInner),
            3 => Ok(Self::BTreeLeaf),
            32 => Ok(Self::HeapFileHeader),
            33 => Ok(Self::IndexFileHeader),
            34 => Ok(Self::DirectoryFileHeader),
            64 => Ok(Self::DirectoryInner),
            65 => Ok(Self::DirectoryLeaf),
            _ => Err(InvalidPageTypeError(v)),
        }
    }
}

// repr(C) to ensure fields are in order.
// derive AsBytes/FromBytes so we can cast a &[u8] directly to &PageHeader.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct UberPageHeader {
    pub page_id: LPageId,  // 8 bytes (u64)
    pub page_lsn: Lsn,    // 8 bytes (u64)
    pub page_type_id: u8, // 1 byte
    pub _pad: [u8; 7],    // 7 bytes
}

pub const PAGE_ID_OFFSET: usize = 0;
pub const PAGE_LSN_OFFSET: usize = 8;
pub const PAGE_TYPE_ID_OFFSET: usize = 16;
pub const UBER_HEADER_SIZE: usize = size_of::<UberPageHeader>(); // 24 bytes

impl UberPageHeader {
    pub fn get_type(&self) -> Result<PageType, InvalidPageTypeError> {
        PageType::try_from(self.page_type_id)
    }
}
