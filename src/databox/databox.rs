use std::convert::TryFrom;

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

#[derive(Debug, Clone)]
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
