mod disk;
pub use disk::{
    allocate_page_in_file, DiskError, DiskManager, DiskManagerImpl, FileType, Result,
};

mod aligned;
pub use aligned::AlignedBuffer;

pub mod allocator;
pub mod directory;
