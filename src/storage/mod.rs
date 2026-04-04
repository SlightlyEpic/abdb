mod disk;
pub use disk::{DiskError, DiskManager, DiskManagerImpl, FileType, Result, allocate_page_in_file};

mod aligned;
pub use aligned::AlignedBuffer;

pub mod allocator;
pub mod directory;
