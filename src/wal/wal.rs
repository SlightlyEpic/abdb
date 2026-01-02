use crate::common::aliases;
use futures::Stream;

#[derive(Clone, Debug)]
pub enum Error {
    // TODO: WAL Errors
}

pub type Result<V> = std::result::Result<V, Error>;

pub enum WALEntry {
    // TODO: WAL Entry variants
}

pub trait WAL: Send + Sync + 'static {
    type LogIterator: Stream<Item = Result<WALEntry>>;

    fn add_entry<'a>(
        &self,
        entry: &'a WALEntry,
    ) -> impl Future<Output = Result<aliases::Lsn>> + '_ + Send;
    fn get_flushed_lsn(&self) -> aliases::Lsn;
    fn force_flush_lsn(&self, lsn: aliases::Lsn) -> impl Future<Output = Result<()>> + '_ + Send;
    fn read_from_lsn(&self, lsn: aliases::Lsn) -> Result<Self::LogIterator>;
}
