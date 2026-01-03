use crate::common::constants::PAGE_BUF_SIZE;
pub struct HeapFileHeaderPage<T> {
    data: T,
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
}

impl<T> HeapFileHeaderPage<T> where T: AsMut<[u8]> {}
