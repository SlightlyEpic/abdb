use std::mem::size_of;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::{
    common::{
        aliases::{self, DirPageId, FileId, LPageId, Lsn, TxnId},
        constants::PAGE_BUF_SIZE,
    },
    page::{UberPageHeader, overlays},
};
pub struct DirectoryFileHeaderPage<T> {
    data: T,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct Data {
    pub magic: [u8; 8],
    pub next_tx_id: TxnId,
    pub last_checkpoint_lsn: Lsn,
    pub next_page_id: LPageId,
    pub dir_root_page: DirPageId,
    pub next_file_id: FileId,
    _pad: [u8; 4],
}

pub enum Error {
    ConvertError(overlays::common::ConvertError),
}

const UBER_HEADER_SIZE: usize = size_of::<UberPageHeader>();

impl<T> DirectoryFileHeaderPage<T>
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

    pub fn uber_header(&self) -> Result<&UberPageHeader, Error> {
        UberPageHeader::ref_from_prefix(self.data.as_ref())
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(|e| Error::ConvertError(e))
    }

    pub fn data(&self) -> Result<&Data, Error> {
        Data::ref_from_prefix(&self.data.as_ref()[UBER_HEADER_SIZE..])
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(Error::ConvertError)
    }
}

impl<T> DirectoryFileHeaderPage<T>
where
    T: AsMut<[u8]>,
{
    pub fn uber_header_mut(&mut self) -> Result<&mut UberPageHeader, Error> {
        UberPageHeader::mut_from_prefix(self.data.as_mut())
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(|e| Error::ConvertError(e))
    }

    pub fn data_mut(&mut self) -> Result<&mut Data, Error> {
        Data::mut_from_prefix(&mut self.data.as_mut()[UBER_HEADER_SIZE..])
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(Error::ConvertError)
    }
}
