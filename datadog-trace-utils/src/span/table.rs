use std::collections::HashMap;
use std::marker::PhantomData;
use crate::span::TraceData;

trait TraceDataType {
    type Data<T: TraceData>;
}
#[derive(Debug, Default, Eq, PartialEq, Hash)]
pub struct TraceDataBytes;
impl TraceDataType for TraceDataBytes {
    type Data<T: TraceData> = T::Bytes;
}
#[derive(Debug, Default, Eq, PartialEq, Hash)]
pub struct TraceDataText;
impl TraceDataType for TraceDataText {
    type Data<T: TraceData> = T::Text;
}

#[derive(Copy, Debug, Default, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct TraceDataRef<T: TraceDataType> {
    index: u32,
    _phantom: PhantomData<T>,
}

impl<T: TraceDataType> TraceDataRef<T> {
    fn new(r#ref: u32) -> Self {
        Self {
            index: r#ref,
            _phantom: PhantomData,
        }
    }
}

pub type TraceStringRef = TraceDataRef<TraceDataText>;
pub type TraceBytesRef = TraceDataRef<TraceDataBytes>;

struct StaticDataValue<T> {
    value: T,
    rc: u32,
}

pub struct StaticDataVec<T: TraceData, D: TraceDataType> {
    vec: Vec<StaticDataValue<D::Data::<T>>>,
    // This HashMap is probably the bottleneck. However we are required to ensure every string only exists once.
    table: HashMap<D::Data::<T>, TraceDataRef<D>>,
}

impl<T: TraceData, D: TraceDataType> Default for StaticDataVec<T, D> {
    fn default() -> Self {
        Self {
            vec: vec![StaticDataValue {
                value: D::Data::<T>::default(),
                rc: 1 << 30, // so that we can just have TraceDataRef::new(0) as default without the rc ever reaching 0
            }],
            table: HashMap::from([(D::Data::<T>::default(), TraceDataRef::new(0))]),
        }
    }
}

impl<T: TraceData, D: TraceDataType> StaticDataVec<T, D> {
    pub fn get(&self, r#ref: TraceDataRef<D>) -> &D::Data::<T> {
        &self.vec[r#ref.index as usize].value
    }

    pub fn add(&mut self, value: D::Data::<T>) -> TraceDataRef<D> {
        if let Some(r#ref) = self.table.get(&value) {
            self.vec[r#ref.index as usize].rc += 1;
            return *r#ref;
        }
        let index = self.vec.len() as u32;
        self.table.insert(value.clone(), TraceDataRef::new(index));
        self.vec.push(StaticDataValue {
            value,
            rc: 1,
        });
        TraceDataRef::new(index)
    }

    pub fn update(&mut self, r#ref: &mut TraceDataRef<D>, value: D::Data::<T>) {
        let entry = &mut self.vec[r#ref.index as usize];
        if entry.rc == 1 {
            self.table.remove(&entry.value);
            self.table.insert(value.clone(), *r#ref);
            entry.value = value;
        } else {
            entry.rc -= 1;
            *r#ref = self.add(entry.value);
        }
    }
}
