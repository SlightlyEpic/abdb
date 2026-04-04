use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct AbdbConfig {
    pub port: usize,
    pub data_dir: PathBuf,
    pub buffer_frame_size: usize,
    pub evictor_lru_k_size: usize,
}
