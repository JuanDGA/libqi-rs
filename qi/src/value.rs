use crate::format;
pub use qi_value::*;
use serde::de::DeserializeSeed;

/// Deserializes the value held by binary formatted data, guided by its type when it is known.
pub(crate) fn deserialize<'a>(
    ty: Option<&Type>,
    data: &'a [u8],
) -> Result<Value<'a>, format::Error> {
    value::de::ValueType(ty).deserialize(&mut format::SliceDeserializer::new(data))
}

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error(transparent)]
    Conversion(#[from] value::FromValueError),

    #[error(transparent)]
    Format(#[from] format::Error),
}
