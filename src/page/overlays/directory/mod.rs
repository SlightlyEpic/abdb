//! # Example Usage
//!
//! use abdb::page::overlays::directory::{DirectoryLeafPage, DirectoryInnerPage};
//!
//! // Reading from a leaf page
//! let page = DirectoryLeafPage::from_buffer(buffer)?;
//! if let Some((phys_page, slot)) = page.lookup(logical_page_id) {
//!     // Found the physical location
//! }
//!
//! // Inserting into a leaf page (requires exclusive latch)
//! let mut page = DirectoryLeafPage::new(&mut buffer);
//! page.insert_entry(logical_page_id, physical_page, slot)?;
//!
//! // Routing through an inner page
//! let page = DirectoryInnerPage::new(&buffer);
//! let child_page_id = page.find_child(search_key);

pub mod error;
pub mod inner;
pub mod leaf;

// Re-export main types for convenience
pub use error::OverlayError;
pub use inner::{DirectoryInnerEntry, DirectoryInnerPage, MAX_KEYS as INNER_MAX_KEYS};
pub use leaf::{DirectoryLeafEntry, DirectoryLeafPage, MAX_ENTRIES as LEAF_MAX_ENTRIES};
