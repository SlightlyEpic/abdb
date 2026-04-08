use crate::common::aliases::{self, LPageId};

pub const PAGE_BUF_SIZE: usize = 4096;

pub const INVALID_PAGE_ID: LPageId = u32::MAX;

pub const SYS_TABLE_TABLES_OID: aliases::OId = 0;
pub const SYS_TABLE_COLUMNS_OID: aliases::OId = 1;
pub const SYS_TABLE_INDEXES_OID: aliases::OId = 2;

pub const SYS_TABLE_TABLES_FID: aliases::FileId = 0;
pub const SYS_TABLE_COLUMNS_FID: aliases::FileId = 1;
pub const SYS_TABLE_INDEXES_FID: aliases::FileId = 2;

/// First OId handed out for user objects (tables, columns, indexes).
/// System catalog OIDs live in [0, USER_OID_START).
pub const USER_OID_START: aliases::OId = 1000;

/// Sentinel FileId reserved for the global page directory (.dir file).
/// Must not collide with any real heap/index file id.
pub const DIRECTORY_FILE_ID: aliases::FileId = u32::MAX;
