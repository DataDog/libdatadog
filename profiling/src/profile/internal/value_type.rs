pub use super::super::pprof;
pub use super::super::StringId;
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ValueType {
    pub r#type: StringId,
    pub unit: StringId,
}

impl From<ValueType> for pprof::ValueType {
    fn from(vt: ValueType) -> Self {
        Self::from(&vt)
    }
}

impl From<&ValueType> for pprof::ValueType {
    fn from(vt: &ValueType) -> Self {
        pprof::ValueType {
            r#type: vt.r#type.into(),
            unit: vt.unit.into(),
        }
    }
}
