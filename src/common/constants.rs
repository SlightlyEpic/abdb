use crate::common::aliases::{self, LPageId};

pub const PAGE_BUF_SIZE: usize = 4096;

pub const INVALID_PAGE_ID: LPageId = u32::MAX;

pub const SYS_TABLE_TABLES_OID: aliases::OId = 0;
pub const SYS_TABLE_COLUMNS_OID: aliases::OId = 1;
pub const SYS_TABLE_INDEXES_OID: aliases::OId = 2;

pub const SYS_TABLE_TABLES_FID: aliases::FileId = 0;
pub const SYS_TABLE_COLUMNS_FID: aliases::FileId = 1;
pub const SYS_TABLE_INDEXES_FID: aliases::FileId = 2;

/// FileId for the global page directory (.dir file).
/// Kept above the sys-table FIDs (0/1/2) so it cannot collide with
/// `SYS_TABLE_COLUMNS_FID` if ever routed through the disk manager's
/// `file_handles` map.
pub const DIRECTORY_FILE_ID: aliases::FileId = u32::MAX;
