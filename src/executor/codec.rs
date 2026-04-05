use crate::{
    catalog,
    databox::{DataType, TupleLayout, Value},
};

use super::Row;

/// Decode a raw tuple (with MVCC header) into a Row of Values.
/// `columns` must be sorted by position (as returned by the catalog).
/// `layout` must have been built from the same columns.
pub fn decode_row(layout: &TupleLayout, columns: &[catalog::Column], raw: &[u8]) -> Row {
    columns
        .iter()
        .enumerate()
        .map(|(i, col)| layout.read_field(&col.name, i, raw).unwrap_or(Value::Null))
        .collect()
}

/// Encode a Row of Values into raw tuple bytes (with zeroed MVCC header).
/// `columns` must be sorted by position.
/// `layout` must have been built from the same columns.
/// The accessor/heap layer is responsible for setting XMIN/XMAX.
pub fn encode_row(layout: &TupleLayout, columns: &[catalog::Column], values: &[Value]) -> Vec<u8> {
    // Calculate variable-length data size
    let mut var_size: u16 = 0;
    for (i, col) in columns.iter().enumerate() {
        if col.type_id == DataType::String {
            if let Some(Value::String(s)) = values.get(i) {
                var_size += s.len() as u16;
            }
        }
    }

    let total = layout.fixed_len + var_size;
    let mut buf = vec![0u8; total as usize];
    let mut var_offset = layout.fixed_len;

    for (i, col) in columns.iter().enumerate() {
        let value = values.get(i).unwrap_or(&Value::Null);
        let col_name: &str = &col.name;

        if value.is_null() {
            layout.write_field(col_name, i, &mut buf, &Value::Null);
            continue;
        }

        if col.type_id == DataType::String {
            if let Value::String(s) = value {
                let bytes = s.as_bytes();
                let len = bytes.len() as u16;
                let off = var_offset;

                if let Some(&ptr_offset) = layout.offsets.get(col_name) {
                    let o = ptr_offset as usize;
                    if o + 4 <= buf.len() {
                        buf[o..o + 2].copy_from_slice(&len.to_le_bytes());
                        buf[o + 2..o + 4].copy_from_slice(&off.to_le_bytes());
                    }
                }

                let start = off as usize;
                let end = start + len as usize;
                if end <= buf.len() {
                    buf[start..end].copy_from_slice(bytes);
                }
                var_offset += len;

                // Clear null bit
                let bitmap_start = layout.null_bitmap_offset as usize;
                let byte_idx = bitmap_start + (i / 8);
                if byte_idx < buf.len() {
                    buf[byte_idx] &= !(1u8 << (i % 8));
                }
            }
        } else {
            layout.write_field(col_name, i, &mut buf, value);
        }
    }

    buf
}
