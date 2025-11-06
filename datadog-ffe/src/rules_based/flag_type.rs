use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum FlagType {
    #[serde(alias = "BOOLEAN")]
    Boolean = 1,
    #[serde(alias = "STRING")]
    String = 1 << 1,
    #[serde(alias = "NUMERIC")]
    Float = 1 << 2,
    #[serde(alias = "INTEGER")]
    Integer = 1 << 3,
    #[serde(alias = "JSON")]
    Object = 1 << 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
#[non_exhaustive]
pub enum ExpectedFlagType {
    Boolean = FlagType::Boolean as u8,
    String = FlagType::String as u8,
    Float = FlagType::Float as u8,
    Integer = FlagType::Integer as u8,
    Object = FlagType::Object as u8,
    Number = (FlagType::Integer as u8) | (FlagType::Float as u8),
    Any = 0xff,
}

impl From<FlagType> for ExpectedFlagType {
    fn from(value: FlagType) -> Self {
        match value {
            FlagType::String => ExpectedFlagType::String,
            FlagType::Integer => ExpectedFlagType::Integer,
            FlagType::Float => ExpectedFlagType::Float,
            FlagType::Boolean => ExpectedFlagType::Boolean,
            FlagType::Object => ExpectedFlagType::Object,
        }
    }
}

impl ExpectedFlagType {
    pub(crate) fn is_compatible(self, ty: FlagType) -> bool {
        (self as u8) & (ty as u8) != 0
    }
}
