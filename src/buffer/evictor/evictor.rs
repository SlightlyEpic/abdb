#[derive(Clone, Debug)]
pub enum Error {
    // TODO: Evictor Errors
}

pub type Result<V> = std::result::Result<V, Error>;

pub trait EvictionPolicy: Send + Sync + 'static {
    fn find_victim(&self) -> Result<usize>;

    fn record_access(&self, frame_id: usize);
    fn set_evictable(&self, frame_id: usize, evictable: bool);
}
