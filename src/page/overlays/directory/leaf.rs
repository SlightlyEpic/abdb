use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::common::constants::PAGE_BUF_SIZE;
pub struct DirectoryLeafPage<T> {
    data: T,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct Header {
    pub last_offset: u64,
    pub next_offset: u64,
    pub num_entries: u16,
    _pad: [u8; 6],
}

impl<T> DirectoryLeafPage<T>
where
    T: AsRef<[u8]>,
{
    pub fn new(data: T) -> Self {
        if data.as_ref().len() != PAGE_BUF_SIZE {
            panic!(
                "new called with buffer of size {} (expected {})",
                data.as_ref().len(),
                PAGE_BUF_SIZE
            );
        }
        Self { data }
    }
}

impl<T> DirectoryLeafPage<T> where T: AsMut<[u8]> {}
