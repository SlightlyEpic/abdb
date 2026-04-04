This is `abdb`, a relational database which has the following layers:

- Server (Calls Parser with raw SQL and sends back output)
- Parser (Outputs AST)
- Binder (Outputs Bound AST)
- Planner (Outputs Logical Plan)
- Optimizer (Outputs Physical Plan)
- Executors (Executes physical plan)
  - Each physical node maps to an executor
- Accessor (Used by executors to access tables, indexes and catalog. Also handles caching the catalog)
- Buffer pool (Used by accessor, handles loading pages and enforces read/write exclusion)
- Storage (Used by buffer pool, handles file IO)
  - Sublayer: Allocator (Finds what pages are available to use in files)
  - Sublayer: Page Directory (Maintains Logical Page Id -> Physical file offset mapping)

Some other supporting components are:
- Databox: Provides an interface for working with raw byte slices as logical row oriented tuples.
- Overlays: Provides an interface for working with raw byte slices for pages. Each page variant has an overlay that makes it possible to operate using clean methods without touching raw bytes.
- WAL (Not implemented): The Write Ahead Log, used for durability.

Knowledge base:
- abdb is a row oriented, relational, SQL based DBMS
- abdb uses a multi file system, with one file per table and index
- abdb is async and uses tokio as the runtime
- abdb uses the volcano model for the executors by leveraging futures::Stream
- abdb uses direct IO
- abdb supports isolation levels upto snapshot isolation, using XMIN and XMAX values per tuple
- WAL will be implemented later
