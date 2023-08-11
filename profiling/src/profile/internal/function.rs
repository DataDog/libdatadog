pub use super::super::pprof;
pub use super::super::StringId;
use std::fmt::Debug;
use std::num::NonZeroU32;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Function {
    pub name: StringId,
    pub system_name: StringId,
    pub filename: StringId,
    pub start_line: u32,
}

impl Function {
    pub fn new<T>(name: StringId, system_name: StringId, filename: StringId, start_line: T) -> Self
    where
        T: TryInto<u32>,
        T::Error: Debug,
    {
        let start_line: u32 = start_line
            .try_into()
            .expect("file line number to fit into a u32");
        Self {
            name,
            system_name,
            filename,
            start_line,
        }
    }
    pub fn to_pprof(&self, id: u64) -> pprof::Function {
        pprof::Function {
            id,
            name: self.name.into(),
            system_name: self.system_name.into(),
            filename: self.filename.into(),
            start_line: self.start_line.into(),
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct FunctionId(NonZeroU32);

impl FunctionId {
    pub fn new<T>(v: T) -> Self
    where
        T: TryInto<u32>,
        T::Error: Debug,
    {
        let index: u32 = v.try_into().expect("FunctionId to fit into a u32");

        // PProf reserves location 0.
        // Both this, and the serialization of the table, add 1 to avoid the 0 element
        let index = index.checked_add(1).expect("FunctionId to fit into a u32");
        // Safety: the `checked_add(1).expect(...)` guards this from ever being zero.
        let index = unsafe { NonZeroU32::new_unchecked(index) };
        Self(index)
    }
}

impl From<FunctionId> for u64 {
    fn from(s: FunctionId) -> Self {
        Self::from(&s)
    }
}

impl From<&FunctionId> for u64 {
    fn from(s: &FunctionId) -> Self {
        s.0.get().into()
    }
}
