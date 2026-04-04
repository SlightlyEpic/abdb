use std::path::{PathBuf};

pub struct AbdbConfig {
    pub data_dir: PathBuf,
    pub buffer_frame_size: usize,
    pub evictor_lru_k_size: usize,
}