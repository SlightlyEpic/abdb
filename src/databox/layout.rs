use crate::{catalog, databox::DataType};
use std::collections::HashMap;

pub const HEADER_SIZE: u16 = 16; // u64 XMIN + u64 XMAX
pub const VAR_PTR_SIZE: u16 = 4; // u16 length + u16 offset

#[derive(Debug, Clone)]
pub struct TupleLayout {
    pub count: u16,
    pub null_bitmap_offset: u16,
    pub offsets: HashMap<String, u16>,
    /// The total size of the fixed portion: Header + Bitmap + Fixed Cols + Pointers
    /// Can also be interpreted as start of variable heap for the tuple
    pub fixed_len: u16,
}

impl From<Vec<catalog::Column>> for TupleLayout {
    fn from(mut columns: Vec<catalog::Column>) -> Self {
        columns.sort_by_key(|c| c.position);

        let count = columns.len() as u16;

        let null_bitmap_offset = HEADER_SIZE;
        let null_bitmap_len = (count + 7) / 8;

        let mut current_offset = null_bitmap_offset + null_bitmap_len;
        let mut offsets = HashMap::new();

        for col in columns {
            let (size, align) = get_type_layout(&col.type_id);

            let padding = (align - (current_offset % align)) % align;
            current_offset += padding;

            offsets.insert(col.name.into_owned(), current_offset);

            current_offset += size;
        }

        TupleLayout {
            count,
            null_bitmap_offset,
            offsets,
            fixed_len: current_offset,
        }
    }
}

/// Returns (size, alignment)
fn get_type_layout(dtype: &DataType) -> (u16, u16) {
    match dtype {
        DataType::Bool => (1, 1),
        DataType::I8 => (1, 1),
        DataType::U8 => (1, 1),

        DataType::I16 => (2, 2),
        DataType::U16 => (2, 2),

        DataType::I32 => (4, 4),
        DataType::U32 => (4, 4),
        DataType::F32 => (4, 4),

        DataType::I64 => (8, 8),
        DataType::U64 => (8, 8),
        DataType::F64 => (8, 8),

        // (length, offset)
        DataType::String => (VAR_PTR_SIZE, 4),
    }
}

impl TupleLayout {
    // TODO: Methods for reading and writing to fields
}
