use crate::{
    common::{aliases, constants::PAGE_BUF_SIZE},
    page::overlays,
};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};
pub struct HeapFileHeaderPage<T> {
    data: T,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct Data {
    pub magic: [u8; 8],
    pub num_pages: u32,
    pub table_oid: aliases::OId,
    pub free_list_root: u32,
    pub version: u16,
    _pad: u16,
}

#[derive(Debug)]
pub enum Error {
    ConvertError(overlays::common::ConvertError),
}

impl<T> HeapFileHeaderPage<T>
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

    pub fn data(&self) -> Result<&Data, Error> {
        Data::ref_from_prefix(&self.data.as_ref())
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(Error::ConvertError)
    }
}

impl<T> HeapFileHeaderPage<T>
where
    T: AsMut<[u8]>,
{
    pub fn data_mut(&mut self) -> Result<&mut Data, Error> {
        Data::mut_from_prefix(self.data.as_mut())
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(|e| Error::ConvertError(e))
    }
}
