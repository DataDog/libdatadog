use serde::Deserializer;
use serde_bytes::ByteBuf;

pub trait Deserialize<'de>: Sized {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>;
}

impl<'de> Deserialize<'de> for Vec<Vec<u8>> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
    {
        Deserialize::deserialize(deserializer).map(|v: Vec<ByteBuf>| v.into_iter().map(ByteBuf::into_vec).collect())
    }
}

impl<'de> Deserialize<'de> for Vec<ByteBuf> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
    {
        serde::Deserialize::deserialize(deserializer)
    }
}

pub fn deserialize<'de, T, D>(deserializer: D) -> Result<T, D::Error>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer)
}
