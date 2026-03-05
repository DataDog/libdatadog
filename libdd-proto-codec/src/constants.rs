use core::hash;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, hash::Hash, PartialOrd, Ord)]
pub enum WireType {
    Varint = 0,
    Fixed64 = 1,
    LengthDelimited = 2,
    StartGroup = 3, // Deprecated in proto3, but still used in proto2.
    EndGroup = 4,   // Deprecated in proto3, but still used in proto2.
    Fixed32 = 5,
}

impl WireType {
    #[inline]
    #[allow(unused)]
    pub(crate) const fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(WireType::Varint),
            1 => Some(WireType::Fixed64),
            2 => Some(WireType::LengthDelimited),
            3 => Some(WireType::StartGroup),
            4 => Some(WireType::EndGroup),
            5 => Some(WireType::Fixed32),
            _ => None,
        }
    }

    #[inline]
    pub(crate) const fn to_u32(self) -> u32 {
        self as u32
    }
}
