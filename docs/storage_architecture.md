# Storage Architecture

## Directory structure
- abdb uses a multi-file structure
- Each file name is derived from a `FileId` (`u32`)
- Each table is allotted one `.heap` file for storing table data
- Each index is alloted one `.idx` file for storing index data
- The global page directory is allotted one `.dir` file for storing a BTree which maps `PageId`s to `PhysicalId`s

TODO: WAL files

## File Structure:
- `.heap`
    - Each page is identified by a `PageId`
    - The page at offset `0` will always be a `HeapFileHeader` page
    - All other pages will be `HeapPage`s
- `.idx`
    - Each page is identified by a `PageId`
    - The page at offset `0` will always be a `IndexFileHeader` page
    - All other pages will be either `BTreeInnerPage` or `BTreeLeafPage`
- `.dir`
    - Each page is identified by a `DirPageId`
    - A `DirPageId` can be converted to a file offset using `offset = dir_page_id * PAGE_SIZE`
    - The page at offset `0` will always be a `DirectoryFileHeader` page
    - All other pages will be either `DirectoryInnerPage` or `DirectoryLeafPage`

TODO: WAL file structure

## Table Page
- A `TablePage` is a slotted data page which contains entire tuples.
- Tuples with sizes greater than the space in a single page are not supported

## Page Directory
- The page directory is a B+Tree
- A single entry in the leaf node consists of
    - `PageId`
    - `PhysicalId`
    - `FreeSpaceBucket`: Indicates how much approximate free space is left in the page
