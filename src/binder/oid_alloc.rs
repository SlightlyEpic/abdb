use crate::common::aliases;

// where to put this
pub trait OidAllocator: Send + Sync {
    fn next_oid(&self) -> aliases::OId;
    fn next_file_id(&self) -> aliases::FileId;
}