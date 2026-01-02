use crate::common::aliases;

pub struct UnusedPage<'a> {
    data: &'a aliases::PageBuffer,
}

impl<'a> UnusedPage<'a> {
    pub fn new(data: &'a aliases::PageBuffer) -> Self {
        Self { data }
    }
}
