use std::sync::Arc;
use crate::common::aliases;

pub trait OidAllocator: Send + Sync {
    fn next_oid(&self) -> aliases::OId;
    fn next_file_id(&self) -> aliases::FileId;
}

impl<T: OidAllocator> OidAllocator for Arc<T> {
    fn next_oid(&self) -> aliases::OId {
        (**self).next_oid()
    }

    fn next_file_id(&self) -> aliases::FileId {
        (**self).next_file_id()
    }
}
