use std::mem::size_of;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::{
    common::{
        aliases::{LPageId, OId},
        constants::PAGE_BUF_SIZE,
    },
    page::{PageType, UberPageHeader, overlays},
};

pub struct IndexFileHeaderPage<T> {
    data: T,
}

/// Magic bytes identifying a valid index file header.
pub const MAGIC: [u8; 8] = *b"ABDB_IDX";

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct Data {
    pub magic: [u8; 8],
    pub num_pages: u32,
    pub index_oid: OId,
    pub root_page: LPageId,
    pub table_oid: OId,
    pub version: u16,
    _pad: u16,
}

#[derive(Debug)]
pub enum Error {
    ConvertError(overlays::common::ConvertError),
    TypeMismatch {
        expected: PageType,
        found: PageType,
    },
    InvalidPageType(u8),
}

const UBER_HEADER_SIZE: usize = size_of::<UberPageHeader>();

// ============================================================================
// READ-ONLY METHODS
// ============================================================================

impl<T> IndexFileHeaderPage<T>
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

    pub fn from_buffer(data: T) -> Result<Self, Error> {
        let page = Self::new(data);
        page.validate_type()?;
        Ok(page)
    }

    fn validate_type(&self) -> Result<(), Error> {
        let uber = self.uber_header().map_err(|_| Error::InvalidPageType(0))?;
        let page_type = PageType::try_from(uber.page_type_id)
            .map_err(|_| Error::InvalidPageType(uber.page_type_id))?;

        if page_type != PageType::IndexFileHeader {
            return Err(Error::TypeMismatch {
                expected: PageType::IndexFileHeader,
                found: page_type,
            });
        }
        Ok(())
    }

    #[inline]
    pub fn as_buffer(&self) -> &[u8] {
        self.data.as_ref()
    }

    pub fn uber_header(&self) -> Result<&UberPageHeader, Error> {
        UberPageHeader::ref_from_prefix(self.data.as_ref())
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(Error::ConvertError)
    }

    pub fn data(&self) -> Result<&Data, Error> {
        Data::ref_from_prefix(&self.data.as_ref()[UBER_HEADER_SIZE..])
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(Error::ConvertError)
    }

    pub fn into_inner(self) -> T {
        self.data
    }
}

// ============================================================================
// MUTABLE METHODS
// ============================================================================

impl<T> IndexFileHeaderPage<T>
where
    T: AsRef<[u8]> + AsMut<[u8]>,
{
    /// Initialize a new index file header page.
    ///
    /// Sets the page type to `IndexFileHeader`, writes magic bytes,
    /// and zeroes all other fields.
    pub fn init(mut data: T, page_id: LPageId, index_oid: OId, table_oid: OId) -> Self {
        let buffer = data.as_mut();
        buffer.fill(0);

        let mut uber = UberPageHeader::read_from_prefix(buffer)
            .expect("UberPageHeader must fit")
            .0;
        uber.page_lsn = 0;
        uber.page_id = page_id;
        uber.page_type_id = PageType::IndexFileHeader as u8;
        uber.write_to_prefix(buffer)
            .expect("UberPageHeader must fit in page buffer");

        let header_data = Data {
            magic: MAGIC,
            num_pages: 0,
            index_oid,
            root_page: 0,
            table_oid,
            version: 1,
            _pad: 0,
        };
        header_data
            .write_to_prefix(&mut data.as_mut()[UBER_HEADER_SIZE..])
            .expect("Data must fit after UberPageHeader");

        Self { data }
    }

    #[inline]
    pub fn as_buffer_mut(&mut self) -> &mut [u8] {
        self.data.as_mut()
    }

    pub fn uber_header_mut(&mut self) -> Result<&mut UberPageHeader, Error> {
        UberPageHeader::mut_from_prefix(self.data.as_mut())
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(Error::ConvertError)
    }

    pub fn data_mut(&mut self) -> Result<&mut Data, Error> {
        Data::mut_from_prefix(&mut self.data.as_mut()[UBER_HEADER_SIZE..])
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(Error::ConvertError)
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let page = IndexFileHeaderPage::init(buffer, 0, 5, 42);

        let uber = page.uber_header().unwrap();
        assert_eq!(uber.page_id, 0);
        assert_eq!(uber.page_type_id, PageType::IndexFileHeader as u8);

        let data = page.data().unwrap();
        assert_eq!(&data.magic, b"ABDB_IDX");
        assert_eq!(data.index_oid, 5);
        assert_eq!(data.table_oid, 42);
        assert_eq!(data.num_pages, 0);
        assert_eq!(data.version, 1);
    }

    #[test]
    fn test_from_buffer_validation() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let page = IndexFileHeaderPage::init(buffer, 0, 1, 1);

        let valid_buffer: [u8; PAGE_BUF_SIZE] = page.as_buffer().try_into().unwrap();
        let result = IndexFileHeaderPage::from_buffer(valid_buffer);
        assert!(result.is_ok());

        let bad_buffer = [0u8; PAGE_BUF_SIZE];
        let result = IndexFileHeaderPage::from_buffer(bad_buffer);
        assert!(result.is_err());
    }

    #[test]
    fn test_data_mutation() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = IndexFileHeaderPage::init(buffer, 0, 1, 1);

        page.data_mut().unwrap().root_page = 99;
        assert_eq!(page.data().unwrap().root_page, 99);

        page.data_mut().unwrap().num_pages = 50;
        assert_eq!(page.data().unwrap().num_pages, 50);
    }
}
