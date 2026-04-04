use std::sync::Arc;

use crate::{accessor::AccessorImpl, buffer::{evictor::LruKEvictor, r#impl::BufferPool}, server::config::AbdbConfig, storage::{DiskManagerImpl, allocator::SimpleAllocator, directory::BTreePageDirectory}};

pub struct TcpServer {
    accessor: Arc<AccessorImpl<BufferPool<DiskManagerImpl<BTreePageDirectory, SimpleAllocator>>>>,
}

impl TcpServer {
    pub async fn new(config: AbdbConfig) -> Self {
        let page_directory = Arc::new(BTreePageDirectory::open(config.data_dir.join("/page.dir")).await.expect("Could not create page directory"));
        let page_allocator = Arc::new(SimpleAllocator::new());
        let disk_manager = Arc::new(DiskManagerImpl::new(config.data_dir, page_directory, page_allocator, 0));
        let eviction_policy = Box::new(LruKEvictor::new(config.evictor_lru_k_size));
        let buffer_pool = Arc::new(BufferPool::new(config.buffer_frame_size, disk_manager, eviction_policy));
        let accessor = AccessorImpl::new(buffer_pool);
        

        Self {
            accessor: Arc::new(accessor),
        }
    }
}
