use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::RwLock;

use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub type TxnId = u64;
pub type Timestamp = u64;

const ABORTED_TS: u64 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClogEntry {
    Committed(Timestamp),
    Aborted,
}

pub struct CommitLog {
    path: PathBuf,
    map: RwLock<HashMap<TxnId, ClogEntry>>,
}

impl CommitLog {
    pub async fn open(path: PathBuf) -> io::Result<Self> {
        let mut map: HashMap<TxnId, ClogEntry> = HashMap::new();

        if path.exists() {
            let mut file = tokio::fs::File::open(&path).await?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).await?;

            for chunk in buf.chunks_exact(16) {
                let txn_id = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
                let ts = u64::from_le_bytes(chunk[8..16].try_into().unwrap());
                let entry = if ts == ABORTED_TS {
                    ClogEntry::Aborted
                } else {
                    ClogEntry::Committed(ts)
                };
                map.insert(txn_id, entry);
            }
        }

        Ok(Self {
            path,
            map: RwLock::new(map),
        })
    }

    pub fn max_ts(&self) -> Timestamp {
        self.map
            .read()
            .unwrap()
            .values()
            .filter_map(|e| {
                if let ClogEntry::Committed(ts) = e {
                    Some(*ts)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0)
    }

    pub fn get(&self, txn_id: TxnId) -> Option<ClogEntry> {
        self.map.read().unwrap().get(&txn_id).copied()
    }

    pub async fn record_commit(&self, txn_id: TxnId, commit_ts: Timestamp) -> io::Result<()> {
        self.append_record(txn_id, commit_ts).await?;
        self.map
            .write()
            .unwrap()
            .insert(txn_id, ClogEntry::Committed(commit_ts));
        Ok(())
    }

    pub async fn record_abort(&self, txn_id: TxnId) -> io::Result<()> {
        self.append_record(txn_id, ABORTED_TS).await?;
        self.map.write().unwrap().insert(txn_id, ClogEntry::Aborted);
        Ok(())
    }

    async fn append_record(&self, txn_id: TxnId, ts: Timestamp) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        let mut buf = [0u8; 16];
        buf[0..8].copy_from_slice(&txn_id.to_le_bytes());
        buf[8..16].copy_from_slice(&ts.to_le_bytes());

        file.write_all(&buf).await?;
        file.sync_all().await?;
        Ok(())
    }
}
