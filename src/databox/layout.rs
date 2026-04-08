use crate::{
    catalog,
    databox::{DataType, Value},
};
use std::collections::HashMap;

pub const HEADER_SIZE: u16 = 16; // u64 XMIN + u64 XMAX
pub const VAR_PTR_SIZE: u16 = 4; // u16 length + u16 offset

#[derive(Debug, Clone)]
pub struct TupleLayout {
    pub count: u16,
    pub null_bitmap_offset: u16,
    pub offsets: HashMap<String, u16>,
    /// Maps column name to its DataType for field access.
    pub types: HashMap<String, DataType>,
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
        let mut types = HashMap::new();

        for col in columns {
            let (size, align) = get_type_layout(&col.type_id);

            let padding = (align - (current_offset % align)) % align;
            current_offset += padding;

            let name = col.name.into_owned();
            offsets.insert(name.clone(), current_offset);
            types.insert(name, col.type_id);

            current_offset += size;
        }

        TupleLayout {
            count,
            null_bitmap_offset,
            offsets,
            types,
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
    /// Read a typed field from a raw tuple buffer (including the 16-byte MVCC header).
    ///
    /// Returns `Value::Null` if the null bitmap bit is set.
    /// Returns `None` if the column name is not in the layout.
    pub fn read_field(&self, col_name: &str, col_index: usize, tuple: &[u8]) -> Option<Value> {
        let &offset = self.offsets.get(col_name)?;
        let dtype = self.types.get(col_name)?;
        let o = offset as usize;

        // Check null bitmap.
        let bitmap_start = self.null_bitmap_offset as usize;
        let byte_idx = bitmap_start + (col_index / 8);
        if byte_idx < tuple.len() && (tuple[byte_idx] >> (col_index % 8)) & 1 == 1 {
            return Some(Value::Null);
        }

        match dtype {
            DataType::Bool => tuple.get(o).map(|&b| Value::Bool(b != 0)),
            DataType::I8 => tuple.get(o).map(|&b| Value::I8(b as i8)),
            DataType::I16 => read_le_arr::<2>(tuple, o).map(|b| Value::I16(i16::from_le_bytes(b))),
            DataType::I32 => read_le_arr::<4>(tuple, o).map(|b| Value::I32(i32::from_le_bytes(b))),
            DataType::I64 => read_le_arr::<8>(tuple, o).map(|b| Value::I64(i64::from_le_bytes(b))),
            DataType::U8 => tuple.get(o).map(|&b| Value::U8(b)),
            DataType::U16 => read_le_arr::<2>(tuple, o).map(|b| Value::U16(u16::from_le_bytes(b))),
            DataType::U32 => read_le_arr::<4>(tuple, o).map(|b| Value::U32(u32::from_le_bytes(b))),
            DataType::U64 => read_le_arr::<8>(tuple, o).map(|b| Value::U64(u64::from_le_bytes(b))),
            DataType::F32 => read_le_arr::<4>(tuple, o).map(|b| Value::F32(f32::from_le_bytes(b))),
            DataType::F64 => read_le_arr::<8>(tuple, o).map(|b| Value::F64(f64::from_le_bytes(b))),
            DataType::String => {
                let len = u16::from_le_bytes(read_le_arr::<2>(tuple, o)?) as usize;
                let off = u16::from_le_bytes(read_le_arr::<2>(tuple, o + 2)?) as usize;
                let end = off + len;
                if end <= tuple.len() {
                    Some(Value::String(
                        String::from_utf8_lossy(&tuple[off..end]).into_owned(),
                    ))
                } else {
                    Some(Value::String(String::new()))
                }
            }
        }
    }

    /// Write a typed value into a raw tuple buffer (including the 16-byte MVCC header).
    ///
    /// For fixed-width types, writes directly at the column's offset.
    /// For Null, sets the null bitmap bit.
    ///
    /// **Does not handle variable-length writes** (String). String fields require
    /// rebuilding the tuple via the codec since the heap region must be appended.
    /// Returns false if the column is not in the layout or the type is String.
    pub fn write_field(
        &self,
        col_name: &str,
        col_index: usize,
        tuple: &mut [u8],
        value: &Value,
    ) -> bool {
        let Some(&offset) = self.offsets.get(col_name) else {
            return false;
        };
        let o = offset as usize;

        // Handle Null: set the bitmap bit and zero the field.
        let bitmap_start = self.null_bitmap_offset as usize;
        let byte_idx = bitmap_start + (col_index / 8);
        let bit_mask = 1u8 << (col_index % 8);

        if value.is_null() {
            if byte_idx < tuple.len() {
                tuple[byte_idx] |= bit_mask;
            }
            return true;
        }

        // Clear null bit.
        if byte_idx < tuple.len() {
            tuple[byte_idx] &= !bit_mask;
        }

        match value {
            Value::Bool(v) => {
                tuple.get_mut(o).map(|b| *b = *v as u8);
            }
            Value::I8(v) => {
                tuple.get_mut(o).map(|b| *b = *v as u8);
            }
            Value::I16(v) => write_le(tuple, o, &v.to_le_bytes()),
            Value::I32(v) => write_le(tuple, o, &v.to_le_bytes()),
            Value::I64(v) => write_le(tuple, o, &v.to_le_bytes()),
            Value::U8(v) => {
                tuple.get_mut(o).map(|b| *b = *v);
            }
            Value::U16(v) => write_le(tuple, o, &v.to_le_bytes()),
            Value::U32(v) => write_le(tuple, o, &v.to_le_bytes()),
            Value::U64(v) => write_le(tuple, o, &v.to_le_bytes()),
            Value::F32(v) => write_le(tuple, o, &v.to_le_bytes()),
            Value::F64(v) => write_le(tuple, o, &v.to_le_bytes()),
            Value::String(_) => return false, // Requires heap rebuild via codec.
            Value::Null => unreachable!(),
        }
        true
    }

    pub fn encode_tuple(
        &self,
        columns: &[&str],
        values: &[Value],
    ) -> Vec<u8> {
        assert_eq!(columns.len(), values.len(), "Columns and values must match");

        let mut heap_size = 0;
        for val in values {
            if let Value::String(s) = val {
                heap_size += s.len();
            }
        }

        let total_size = self.fixed_len as usize + heap_size;
        let mut tuple = vec![0u8; total_size];

        let mut current_heap_offset = self.fixed_len as usize;

        for (col_index, (&col_name, val)) in columns.iter().zip(values.iter()).enumerate() {
            if val.is_null() {
                self.write_field(col_name, col_index, &mut tuple, val);
                continue;
            }

            match val {
                Value::String(s) => {
                    let len = s.len();
                    let bytes = s.as_bytes();
                    
                    tuple[current_heap_offset..current_heap_offset + len]
                        .copy_from_slice(bytes);

                    let fixed_offset = *self.offsets.get(col_name).unwrap() as usize;
                    let len_bytes = (len as u16).to_le_bytes();
                    let off_bytes = (current_heap_offset as u16).to_le_bytes();
                    
                    tuple[fixed_offset..fixed_offset + 2].copy_from_slice(&len_bytes);
                    tuple[fixed_offset + 2..fixed_offset + 4].copy_from_slice(&off_bytes);

                    let bitmap_start = self.null_bitmap_offset as usize;
                    let byte_idx = bitmap_start + (col_index / 8);
                    let bit_mask = 1u8 << (col_index % 8);
                    tuple[byte_idx] &= !bit_mask;

                    current_heap_offset += len;
                }
                _ => {
                    self.write_field(col_name, col_index, &mut tuple, val);
                }
            }
        }

        tuple
    }
}

/// Read exactly N bytes from `data` at `offset`, returning None if out of bounds.
#[inline]
fn read_le_arr<const N: usize>(data: &[u8], offset: usize) -> Option<[u8; N]> {
    data.get(offset..offset + N)?.try_into().ok()
}

/// Write bytes into `data` at `offset`. Does nothing if out of bounds.
#[inline]
fn write_le(data: &mut [u8], offset: usize, bytes: &[u8]) {
    if offset + bytes.len() <= data.len() {
        data[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn test_columns() -> Vec<catalog::Column> {
        vec![
            catalog::Column {
                oid: 1,
                table_oid: 100,
                name: Cow::Borrowed("id"),
                type_id: DataType::I32,
                position: 0,
                nullable: false,
                is_primary_key: false,
                is_unique: false,
            },
            catalog::Column {
                oid: 2,
                table_oid: 100,
                name: Cow::Borrowed("name"),
                type_id: DataType::String,
                position: 1,
                nullable: true,
                is_primary_key: false,
                is_unique: false,
            },
            catalog::Column {
                oid: 3,
                table_oid: 100,
                name: Cow::Borrowed("age"),
                type_id: DataType::U8,
                position: 2,
                nullable: true,
                is_primary_key: false,
                is_unique: false,
            },
        ]
    }

    #[test]
    fn test_layout_offsets() {
        let layout = TupleLayout::from(test_columns());
        assert_eq!(layout.count, 3);
        // Header=16, bitmap=1byte (3 cols), then I32 aligned to 4.
        assert_eq!(layout.null_bitmap_offset, 16);
        // All offsets should be present.
        assert!(layout.offsets.contains_key("id"));
        assert!(layout.offsets.contains_key("name"));
        assert!(layout.offsets.contains_key("age"));
        // Types should be present.
        assert_eq!(layout.types.get("id"), Some(&DataType::I32));
        assert_eq!(layout.types.get("name"), Some(&DataType::String));
        assert_eq!(layout.types.get("age"), Some(&DataType::U8));
    }

    #[test]
    fn test_read_write_field_fixed() {
        let layout = TupleLayout::from(test_columns());
        // Allocate a tuple buffer large enough for the fixed portion.
        let mut tuple = vec![0u8; layout.fixed_len as usize + 32];

        // Write I32 field.
        assert!(layout.write_field("id", 0, &mut tuple, &Value::I32(42)));
        let val = layout.read_field("id", 0, &tuple).unwrap();
        assert_eq!(val, Value::I32(42));

        // Write U8 field.
        assert!(layout.write_field("age", 2, &mut tuple, &Value::U8(25)));
        let val = layout.read_field("age", 2, &tuple).unwrap();
        assert_eq!(val, Value::U8(25));

        // Write null.
        assert!(layout.write_field("age", 2, &mut tuple, &Value::Null));
        let val = layout.read_field("age", 2, &tuple).unwrap();
        assert_eq!(val, Value::Null);
    }

    #[test]
    fn test_write_field_string_returns_false() {
        let layout = TupleLayout::from(test_columns());
        let mut tuple = vec![0u8; layout.fixed_len as usize + 32];
        // String writes should return false (need codec).
        assert!(!layout.write_field("name", 1, &mut tuple, &Value::String("hi".into())));
    }
}
