use zerocopy::ConvertError as ZcError;

#[derive(Clone, Copy, Debug)]
pub enum ConvertError {
    Alignment,
    Size,
    Validity,
}

impl<A, S, V> From<ZcError<A, S, V>> for ConvertError {
    fn from(value: ZcError<A, S, V>) -> Self {
        match value {
            zerocopy::ConvertError::Alignment(_) => ConvertError::Alignment,
            zerocopy::ConvertError::Size(_) => ConvertError::Size,
            zerocopy::ConvertError::Validity(_) => ConvertError::Validity,
        }
    }
}
