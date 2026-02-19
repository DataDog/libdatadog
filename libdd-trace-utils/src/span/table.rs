use hashbrown::{HashMap, Equivalent};
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;
use serde::Serialize;
use crate::span::{IntoData, SpanDataContents, TraceData};

pub trait TraceDataType: Copy + Clone + Debug + Default + Eq + PartialEq + Hash + Serialize {
    type Data<T: TraceData>: SpanDataContents;
}
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Serialize)]
pub struct TraceDataBytes;
impl TraceDataType for TraceDataBytes {
    type Data<T: TraceData> = T::Bytes;
}
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Serialize)]
pub struct TraceDataText;
impl TraceDataType for TraceDataText {
    type Data<T: TraceData> = T::Text;
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct TraceDataRef<T: TraceDataType> {
    index: u32,
    _phantom: PhantomData<T>,
}

impl<T: TraceDataType> Serialize for TraceDataRef<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.index.serialize(serializer)
    }
}

impl<T: TraceDataType> TraceDataRef<T> {
    pub fn new(r#ref: u32) -> Self {
        Self {
            index: r#ref,
            _phantom: PhantomData,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.index == 0
    }

    pub fn get_index(&self) -> u32 {
        self.index
    }
}

pub type TraceStringRef = TraceDataRef<TraceDataText>;
pub type TraceBytesRef = TraceDataRef<TraceDataBytes>;

#[derive(Debug)]
struct StaticDataValue<T> {
    value: T,
    rc: u32,
}

#[derive(Debug)]
pub struct StaticDataVec<T: TraceData, D: TraceDataType> {
    vec: Vec<StaticDataValue<D::Data<T>>>,
    // This HashMap is probably the bottleneck. However we are required to ensure every string only exists once.
    table: HashMap<<D::Data::<T> as SpanDataContents>::RefCopy, TraceDataRef<D>>,
}

impl<T: TraceData, D: TraceDataType> Default for StaticDataVec<T, D> {
    fn default() -> Self {
        Self {
            vec: vec![StaticDataValue {
                value: D::Data::<T>::default(),
                rc: 1 << 30, // so that we can just have TraceDataRef::new(0) as default without the rc ever reaching 0
            }],
            table: HashMap::from([(D::Data::<T>::default().as_ref_copy(), TraceDataRef::new(0))]),
        }
    }
}

pub struct Shrunk<T> {
    pub table: Vec<T>,
    pub offsets: Vec<u32>,
}

impl<T: TraceData, D: TraceDataType> StaticDataVec<T, D> {
    pub fn get(&self, r#ref: TraceDataRef<D>) -> &D::Data<T> {
        &self.vec[r#ref.index as usize].value
    }

    pub fn add<V: IntoData<D::Data<T>>>(&mut self, value: V) -> TraceDataRef<D> {
        let value = value.into();
        let key = value.as_ref_copy();
        if let Some(r#ref) = self.table.get(&key) {
            self.vec[r#ref.index as usize].rc += 1;
            return *r#ref;
        }
        let index = self.vec.len() as u32;
        self.table.insert(key, TraceDataRef::new(index));
        self.vec.push(StaticDataValue {
            value,
            rc: 1,
        });
        TraceDataRef::new(index)
    }

    pub fn update<V: IntoData<D::Data<T>>>(&mut self, r#ref: &mut TraceDataRef<D>, value: V)
    {
        let entry = &mut self.vec[r#ref.index as usize];
        if entry.rc == 1 {
            self.table.remove(&entry.value.as_ref_copy());
            let value = value.into();
            let key = value.as_ref_copy();
            self.table.insert(key, *r#ref);
            entry.value = value;
        } else {
            entry.rc -= 1;
            *r#ref = self.add(value);
        }
    }

    pub fn reset(&mut self, r#ref: &mut TraceDataRef<D>) {
        let entry = &mut self.vec[r#ref.index as usize];
        if entry.rc == 1 {
            self.table.remove(&entry.value.as_ref_copy());
        } else {
            entry.rc -= 1;
        }
        *r#ref = TraceDataRef::default();
    }

    pub fn find<Q>(&self, value: &Q) -> Option<TraceDataRef<D>>
    where
        Q: ?Sized + Hash + Equivalent<<D::Data::<T> as SpanDataContents>::RefCopy>,
    {
        self.table.get(value).copied()
    }

    pub fn decref(&mut self, r#ref: TraceDataRef<D>) {
        let rc = &mut self.vec[r#ref.index as usize].rc;
        debug_assert!(*rc > 0);
        *rc -= 1;
    }

    pub fn incref(&mut self, r#ref: TraceDataRef<D>) {
        let rc = &mut self.vec[r#ref.index as usize].rc;
        *rc += 1;
    }

    pub fn shrink(self) -> Shrunk<D::Data<T>> {
        let mut offsets = Vec::with_capacity(self.vec.len());
        let mut table = Vec::with_capacity(self.vec.len());
        let mut i = 0;
        for entry in self.vec.into_iter() {
            offsets.push(i);
            if entry.rc == 0 {
                i += 1;
                table.push(entry.value)
            }
        }
        Shrunk {
            table,
            offsets,
        }
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }
}

impl<T: TraceData, D: TraceDataType> Serialize for StaticDataVec<T, D>
where
    D::Data<T>: Serialize,
{
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.vec.len()))?;
        for entry in &self.vec {
            seq.serialize_element(&entry.value)?;
        }
        seq.end()
    }
}

// Convenience methods for more natural access
impl<D: TraceDataType> TraceDataRef<D> {
    pub fn get<T: TraceData>(self, vec: &StaticDataVec<T, D>) -> &D::Data<T> {
        vec.get(self)
    }

    pub fn set<T: TraceData, V: IntoData<D::Data<T>>>(&mut self, vec: &mut StaticDataVec<T, D>, value: V)
    {
        vec.update(self, value)
    }

    pub fn reset<T: TraceData>(&mut self, vec: &mut StaticDataVec<T, D>)
    {
        vec.reset(self)
    }
}
