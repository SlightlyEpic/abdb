use std::cmp::Ordering;
use std::convert::TryFrom;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DataType {
    Bool = 0,

    I8 = 1,
    I16 = 2,
    I32 = 3,
    I64 = 4,

    U8 = 5,
    U16 = 6,
    U32 = 7,
    U64 = 8,

    F32 = 9,
    F64 = 10,

    String = 11,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataTypeCreateError {
    Invalid(u8),
}

impl TryFrom<u8> for DataType {
    type Error = DataTypeCreateError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Bool),

            1 => Ok(Self::I8),
            2 => Ok(Self::I16),
            3 => Ok(Self::I32),
            4 => Ok(Self::I64),

            5 => Ok(Self::U8),
            6 => Ok(Self::U16),
            7 => Ok(Self::U32),
            8 => Ok(Self::U64),

            9 => Ok(Self::F32),
            10 => Ok(Self::F64),

            11 => Ok(Self::String),

            v => Err(DataTypeCreateError::Invalid(v)),
        }
    }
}

impl Into<u8> for DataType {
    fn into(self) -> u8 {
        self as u8
    }
}

impl Into<Value> for DataType {
    fn into(self) -> Value {
        Value::U8(self.into())
    }
}

impl DataType {
    /// Size in bytes for fixed-width types. Returns None for variable-width.
    pub fn fixed_size(&self) -> Option<usize> {
        match self {
            DataType::Bool | DataType::I8 | DataType::U8 => Some(1),
            DataType::I16 | DataType::U16 => Some(2),
            DataType::I32 | DataType::U32 | DataType::F32 => Some(4),
            DataType::I64 | DataType::U64 | DataType::F64 => Some(8),
            DataType::String => None,
        }
    }

    /// Whether this type supports arithmetic operations.
    pub fn is_numeric(&self) -> bool {
        !matches!(self, DataType::Bool | DataType::String)
    }

    /// Whether this type is a signed integer.
    pub fn is_signed_int(&self) -> bool {
        matches!(
            self,
            DataType::I8 | DataType::I16 | DataType::I32 | DataType::I64
        )
    }

    /// Whether this type is an unsigned integer.
    pub fn is_unsigned_int(&self) -> bool {
        matches!(
            self,
            DataType::U8 | DataType::U16 | DataType::U32 | DataType::U64
        )
    }

    /// Whether this type is a floating point type.
    pub fn is_float(&self) -> bool {
        matches!(self, DataType::F32 | DataType::F64)
    }

    /// Convert from u8, returning Bool as default for invalid values.
    pub fn from_u8(value: u8) -> Self {
        Self::try_from(value).unwrap_or(DataType::Bool)
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataType::Bool => write!(f, "BOOL"),
            DataType::I8 => write!(f, "I8"),
            DataType::I16 => write!(f, "I16"),
            DataType::I32 => write!(f, "I32"),
            DataType::I64 => write!(f, "I64"),
            DataType::U8 => write!(f, "U8"),
            DataType::U16 => write!(f, "U16"),
            DataType::U32 => write!(f, "U32"),
            DataType::U64 => write!(f, "U64"),
            DataType::F32 => write!(f, "F32"),
            DataType::F64 => write!(f, "F64"),
            DataType::String => write!(f, "STRING"),
        }
    }
}

// ============================================================================
// Value
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),

    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),

    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),

    F32(f32),
    F64(f64),

    String(String),

    Null,
}

impl Value {
    /// Returns the DataType of this value, or None for Null.
    pub fn data_type(&self) -> Option<DataType> {
        match self {
            Value::Bool(_) => Some(DataType::Bool),
            Value::I8(_) => Some(DataType::I8),
            Value::I16(_) => Some(DataType::I16),
            Value::I32(_) => Some(DataType::I32),
            Value::I64(_) => Some(DataType::I64),
            Value::U8(_) => Some(DataType::U8),
            Value::U16(_) => Some(DataType::U16),
            Value::U32(_) => Some(DataType::U32),
            Value::U64(_) => Some(DataType::U64),
            Value::F32(_) => Some(DataType::F32),
            Value::F64(_) => Some(DataType::F64),
            Value::String(_) => Some(DataType::String),
            Value::Null => None,
        }
    }

    /// Returns true if this value is Null.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Attempt to cast this value to the target DataType.
    ///
    /// Supports widening numeric casts (e.g. I8 -> I64, U16 -> U32, any int -> F64).
    /// Returns None if the cast is not supported or the value is Null.
    pub fn cast(&self, target: DataType) -> Option<Value> {
        if self.is_null() {
            return Some(Value::Null);
        }
        // Same type — no-op.
        if self.data_type() == Some(target) {
            return Some(self.clone());
        }
        match target {
            DataType::I16 => self.to_i64().map(|v| Value::I16(v as i16)),
            DataType::I32 => self.to_i64().map(|v| Value::I32(v as i32)),
            DataType::I64 => self.to_i64().map(Value::I64),
            DataType::U16 => self.to_u64().map(|v| Value::U16(v as u16)),
            DataType::U32 => self.to_u64().map(|v| Value::U32(v as u32)),
            DataType::U64 => self.to_u64().map(Value::U64),
            DataType::F32 => self.to_f64().map(|v| Value::F32(v as f32)),
            DataType::F64 => self.to_f64().map(Value::F64),
            DataType::String => Some(Value::String(self.to_string())),
            DataType::Bool => match self {
                Value::I8(v) => Some(Value::Bool(*v != 0)),
                Value::I32(v) => Some(Value::Bool(*v != 0)),
                Value::I64(v) => Some(Value::Bool(*v != 0)),
                Value::U8(v) => Some(Value::Bool(*v != 0)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Lossless conversion to i64 for integer types.
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Value::I8(v) => Some(*v as i64),
            Value::I16(v) => Some(*v as i64),
            Value::I32(v) => Some(*v as i64),
            Value::I64(v) => Some(*v),
            Value::U8(v) => Some(*v as i64),
            Value::U16(v) => Some(*v as i64),
            Value::U32(v) => Some(*v as i64),
            Value::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }

    /// Lossless conversion to u64 for unsigned integer types.
    pub fn to_u64(&self) -> Option<u64> {
        match self {
            Value::U8(v) => Some(*v as u64),
            Value::U16(v) => Some(*v as u64),
            Value::U32(v) => Some(*v as u64),
            Value::U64(v) => Some(*v),
            Value::Bool(b) => Some(*b as u64),
            // Signed -> unsigned only if non-negative.
            Value::I8(v) if *v >= 0 => Some(*v as u64),
            Value::I16(v) if *v >= 0 => Some(*v as u64),
            Value::I32(v) if *v >= 0 => Some(*v as u64),
            Value::I64(v) if *v >= 0 => Some(*v as u64),
            _ => None,
        }
    }

    /// Conversion to f64 for all numeric types.
    pub fn to_f64(&self) -> Option<f64> {
        match self {
            Value::I8(v) => Some(*v as f64),
            Value::I16(v) => Some(*v as f64),
            Value::I32(v) => Some(*v as f64),
            Value::I64(v) => Some(*v as f64),
            Value::U8(v) => Some(*v as f64),
            Value::U16(v) => Some(*v as f64),
            Value::U32(v) => Some(*v as f64),
            Value::U64(v) => Some(*v as f64),
            Value::F32(v) => Some(*v as f64),
            Value::F64(v) => Some(*v),
            _ => None,
        }
    }

    /// Serialize this value to little-endian bytes.
    ///
    /// For fixed-width types, writes the raw LE bytes.
    /// For String, writes the raw UTF-8 bytes (no length prefix).
    /// For Null, returns an empty vec.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Value::Bool(v) => vec![*v as u8],
            Value::I8(v) => vec![*v as u8],
            Value::I16(v) => v.to_le_bytes().to_vec(),
            Value::I32(v) => v.to_le_bytes().to_vec(),
            Value::I64(v) => v.to_le_bytes().to_vec(),
            Value::U8(v) => vec![*v],
            Value::U16(v) => v.to_le_bytes().to_vec(),
            Value::U32(v) => v.to_le_bytes().to_vec(),
            Value::U64(v) => v.to_le_bytes().to_vec(),
            Value::F32(v) => v.to_le_bytes().to_vec(),
            Value::F64(v) => v.to_le_bytes().to_vec(),
            Value::String(s) => s.as_bytes().to_vec(),
            Value::Null => Vec::new(),
        }
    }

    /// Deserialize a value from little-endian bytes and a known DataType.
    ///
    /// Returns None if `data` is too short for the type.
    pub fn from_bytes(dtype: DataType, data: &[u8]) -> Option<Value> {
        match dtype {
            DataType::Bool => data.first().map(|&b| Value::Bool(b != 0)),
            DataType::I8 => data.first().map(|&b| Value::I8(b as i8)),
            DataType::I16 => data
                .get(..2)
                .map(|b| Value::I16(i16::from_le_bytes(b.try_into().unwrap()))),
            DataType::I32 => data
                .get(..4)
                .map(|b| Value::I32(i32::from_le_bytes(b.try_into().unwrap()))),
            DataType::I64 => data
                .get(..8)
                .map(|b| Value::I64(i64::from_le_bytes(b.try_into().unwrap()))),
            DataType::U8 => data.first().map(|&b| Value::U8(b)),
            DataType::U16 => data
                .get(..2)
                .map(|b| Value::U16(u16::from_le_bytes(b.try_into().unwrap()))),
            DataType::U32 => data
                .get(..4)
                .map(|b| Value::U32(u32::from_le_bytes(b.try_into().unwrap()))),
            DataType::U64 => data
                .get(..8)
                .map(|b| Value::U64(u64::from_le_bytes(b.try_into().unwrap()))),
            DataType::F32 => data
                .get(..4)
                .map(|b| Value::F32(f32::from_le_bytes(b.try_into().unwrap()))),
            DataType::F64 => data
                .get(..8)
                .map(|b| Value::F64(f64::from_le_bytes(b.try_into().unwrap()))),
            DataType::String => Some(Value::String(
                std::string::String::from_utf8_lossy(data).into_owned(),
            )),
        }
    }

    /// Extract as u8, converting if possible.
    pub fn as_u8(&self) -> Option<u8> {
        match self {
            Value::U8(v) => Some(*v),
            Value::I8(v) if *v >= 0 => Some(*v as u8),
            Value::Bool(b) => Some(*b as u8),
            _ => self.to_u64().and_then(|v| u8::try_from(v).ok()),
        }
    }

    /// Extract as u16, converting if possible.
    pub fn as_u16(&self) -> Option<u16> {
        match self {
            Value::U16(v) => Some(*v),
            Value::U8(v) => Some(*v as u16),
            _ => self.to_u64().and_then(|v| u16::try_from(v).ok()),
        }
    }

    /// Extract as u32, converting if possible.
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Value::U32(v) => Some(*v),
            Value::U16(v) => Some(*v as u32),
            Value::U8(v) => Some(*v as u32),
            _ => self.to_u64().and_then(|v| u32::try_from(v).ok()),
        }
    }

    /// Extract as String, converting to string representation.
    pub fn as_string(&self) -> Option<String> {
        match self {
            Value::String(s) => Some(s.clone()),
            Value::Null => None,
            other => Some(other.to_string()),
        }
    }

    /// Extract as bool.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::I8(v) => Some(*v != 0),
            Value::I64(v) => Some(*v != 0),
            Value::U8(v) => Some(*v != 0),
            _ => None,
        }
    }
}

// ============================================================================
// PartialOrd — SQL-style ordering (Null is less than everything)
// ============================================================================

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        // Null handling: Null < any non-null, Null == Null.
        match (self, other) {
            (Value::Null, Value::Null) => return Some(Ordering::Equal),
            (Value::Null, _) => return Some(Ordering::Less),
            (_, Value::Null) => return Some(Ordering::Greater),
            _ => {}
        }

        match (self, other) {
            (Value::Bool(a), Value::Bool(b)) => a.partial_cmp(b),
            (Value::I8(a), Value::I8(b)) => a.partial_cmp(b),
            (Value::I16(a), Value::I16(b)) => a.partial_cmp(b),
            (Value::I32(a), Value::I32(b)) => a.partial_cmp(b),
            (Value::I64(a), Value::I64(b)) => a.partial_cmp(b),
            (Value::U8(a), Value::U8(b)) => a.partial_cmp(b),
            (Value::U16(a), Value::U16(b)) => a.partial_cmp(b),
            (Value::U32(a), Value::U32(b)) => a.partial_cmp(b),
            (Value::U64(a), Value::U64(b)) => a.partial_cmp(b),
            (Value::F32(a), Value::F32(b)) => a.partial_cmp(b),
            (Value::F64(a), Value::F64(b)) => a.partial_cmp(b),
            (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
            // Cross-type: promote to f64
            _ => {
                let a = self.to_f64()?;
                let b = other.to_f64()?;
                a.partial_cmp(&b)
            }
        }
    }
}

// ============================================================================
// Display
// ============================================================================

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(v) => write!(f, "{}", v),
            Value::I8(v) => write!(f, "{}", v),
            Value::I16(v) => write!(f, "{}", v),
            Value::I32(v) => write!(f, "{}", v),
            Value::I64(v) => write!(f, "{}", v),
            Value::U8(v) => write!(f, "{}", v),
            Value::U16(v) => write!(f, "{}", v),
            Value::U32(v) => write!(f, "{}", v),
            Value::U64(v) => write!(f, "{}", v),
            Value::F32(v) => write!(f, "{}", v),
            Value::F64(v) => write!(f, "{}", v),
            Value::String(v) => write!(f, "{}", v),
            Value::Null => write!(f, "NULL"),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_type_roundtrip() {
        for id in 0..=11u8 {
            let dt = DataType::try_from(id).unwrap();
            let back: u8 = dt.into();
            assert_eq!(id, back);
        }
        assert!(DataType::try_from(12u8).is_err());
    }

    #[test]
    fn test_value_data_type() {
        assert_eq!(Value::I32(42).data_type(), Some(DataType::I32));
        assert_eq!(
            Value::String("hi".into()).data_type(),
            Some(DataType::String)
        );
        assert_eq!(Value::Null.data_type(), None);
    }

    #[test]
    fn test_value_to_bytes_from_bytes() {
        let cases: Vec<(Value, DataType)> = vec![
            (Value::Bool(true), DataType::Bool),
            (Value::I16(-300), DataType::I16),
            (Value::I32(100_000), DataType::I32),
            (Value::I64(-1), DataType::I64),
            (Value::U8(255), DataType::U8),
            (Value::U32(42), DataType::U32),
            (Value::F32(3.14), DataType::F32),
            (Value::F64(2.718281828), DataType::F64),
            (Value::String("hello".into()), DataType::String),
        ];
        for (val, dt) in cases {
            let bytes = val.to_bytes();
            let decoded = Value::from_bytes(dt, &bytes).unwrap();
            assert_eq!(val, decoded, "roundtrip failed for {:?}", dt);
        }
    }

    #[test]
    fn test_value_cast_widening() {
        assert_eq!(Value::I8(42).cast(DataType::I64), Some(Value::I64(42)));
        assert_eq!(
            Value::U16(1000).cast(DataType::F64),
            Some(Value::F64(1000.0))
        );
        assert_eq!(
            Value::I32(7).cast(DataType::String),
            Some(Value::String("7".into()))
        );
        assert_eq!(Value::Null.cast(DataType::I64), Some(Value::Null));
    }

    #[test]
    fn test_value_partial_ord() {
        assert!(Value::I32(1) < Value::I32(2));
        assert!(Value::Null < Value::I32(0));
        assert!(Value::String("a".into()) < Value::String("b".into()));
        assert_eq!(Value::Null.partial_cmp(&Value::Null), Some(Ordering::Equal));
    }

    #[test]
    fn test_data_type_helpers() {
        assert!(DataType::I32.is_numeric());
        assert!(!DataType::Bool.is_numeric());
        assert!(!DataType::String.is_numeric());
        assert!(DataType::I64.is_signed_int());
        assert!(DataType::U32.is_unsigned_int());
        assert!(DataType::F64.is_float());
        assert_eq!(DataType::I32.fixed_size(), Some(4));
        assert_eq!(DataType::String.fixed_size(), None);
    }
}
