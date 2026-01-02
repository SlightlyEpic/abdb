use zerocopy::FromBytes;

use crate::{
    common::constants::PAGE_BUF_SIZE,
    page::{UberPageHeader, overlays},
};

pub struct UnknownPage<T> {
    data: T,
}

pub enum Error {
    ConvertError(overlays::common::ConvertError),
}

impl<T> UnknownPage<T>
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
}

impl<T> UnknownPage<T>
where
    T: AsMut<[u8]> + AsRef<[u8]>,
{
    pub fn uber_header_mut(&mut self) -> Result<&mut UberPageHeader, Error> {
        UberPageHeader::mut_from_prefix(self.data.as_mut())
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(|e| Error::ConvertError(e))
    }
}
