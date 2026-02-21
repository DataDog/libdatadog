use std::marker::PhantomData;
use crate::span::{TraceDataLifetime, ImpliedPredicate, IntoData};
use super::{TraceProjector, IMMUT, MUT, as_mut};
use super::{
    TraceAttributeGetterTypes, TraceAttributeSetterTypes,
    AttributeAnyGetterContainer, AttributeAnySetterContainer, AttributeAnyContainer, AttributeAnyValueType,
    AttributeAnyValue,
    TraceAttributesMut, TraceAttributesMutOp,
    AttrOwned, TraceAttributes,
    TraceAttributesString, TraceAttributesBytes,
    TraceAttributesBoolean, TraceAttributesInteger, TraceAttributesDouble,
};

pub struct AttributeArray<'c, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, C, const ISMUT: u8 = IMMUT> {
    pub(crate) storage: &'s T::Storage,
    pub(crate) container: C,
    pub(crate) _phantom: PhantomData<&'c ()>,
}
#[allow(type_alias_bounds)]
pub type AttributeArrayMut<'c, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, C> = AttributeArray<'c, 's, T, D, C, MUT>;

impl<'c, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, C: Clone> Clone for AttributeArray<'c, 's, T, D, C> { // Note: not for MUT
    fn clone(&self) -> Self {
        AttributeArray {
            storage: self.storage,
            container: self.container.clone(),
            _phantom: PhantomData,
        }
    }
}
impl<'c, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, C: Copy> Copy for AttributeArray<'c, 's, T, D, C> {}

pub trait AttributeArrayOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: Sized + TraceAttributeGetterTypes<'container, 'storage, T, D, Self>
{
    fn get_attribute_array_len(&self, storage: &'storage T::Storage) -> usize;
    fn get_attribute_array_value(&'container self, storage: &'storage T::Storage, index: usize) -> AttributeAnyGetterContainer<'container, 'storage, AttributeArray<'container, 'storage, T, D, Self>, T, D, Self>;
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>> TraceAttributeGetterTypes<'container, 'storage, T, D, Self> for () {
    type Array = ();
    type Map = ();
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>> AttributeArrayOp<'container, 'storage, T, D> for () {
    fn get_attribute_array_len(&self, _storage: &'storage T::Storage) -> usize {
        0
    }

    fn get_attribute_array_value(&self, _storage: &'storage T::Storage, _index: usize) -> AttributeAnyGetterContainer<'container, 'storage, AttributeArray<'container, 'storage, T, D, ()>, T, D, ()> {
        panic!("AttributeArrayOp::get_attribute_array_value called on empty array")
    }
}

// AttributeArray<..., C> can serve as the getter type for container C
// (forwarding to C's own TraceAttributeGetterTypes impl).
impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: AttributeArrayOp<'container, 'storage, T, D>, const ISMUT: u8> TraceAttributeGetterTypes<'container, 'storage, T, D, C> for AttributeArray<'container, 'storage, T, D, C, ISMUT> {
    type Array = <C as TraceAttributeGetterTypes<'container, 'storage, T, D, C>>::Array;
    type Map = <C as TraceAttributeGetterTypes<'container, 'storage, T, D, C>>::Map;
}

// AttributeArray<..., C> also needs TraceAttributeGetterTypes with itself as the container type.
// This satisfies the AttributeArrayOp supertrait TraceAttributeGetterTypes<..., Self>.
impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: AttributeArrayOp<'container, 'storage, T, D>, const ISMUT: u8> TraceAttributeGetterTypes<'container, 'storage, T, D, AttributeArray<'container, 'storage, T, D, C, ISMUT>> for AttributeArray<'container, 'storage, T, D, C, ISMUT> {
    type Array = <C as TraceAttributeGetterTypes<'container, 'storage, T, D, C>>::Array;
    type Map = <C as TraceAttributeGetterTypes<'container, 'storage, T, D, C>>::Map;
}

// AttributeArray<..., C> implements AttributeArrayOp by delegating length to the inner container.
// This is needed to satisfy ArrayAttributesOp's ImpliedPredicate<AttributeArray<..., Self>, Impls: AttributeArrayOp>
// supertrait, which is required e.g. for (): ArrayAttributesOp to hold.
impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: AttributeArrayOp<'container, 'storage, T, D>, const ISMUT: u8> AttributeArrayOp<'container, 'storage, T, D> for AttributeArray<'container, 'storage, T, D, C, ISMUT> {
    fn get_attribute_array_len(&self, _storage: &'storage T::Storage) -> usize {
        self.container.get_attribute_array_len(self.storage)
    }

    fn get_attribute_array_value(&'container self, storage: &'storage T::Storage, index: usize) -> AttributeAnyGetterContainer<'container, 'storage, AttributeArray<'container, 'storage, T, D, Self>, T, D, Self> {
        self.container.get_attribute_array_value(storage, index)
    }
}

pub trait AttributeArrayMutOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: AttributeArrayOp<'container, 'storage, T, D> + TraceAttributeSetterTypes<'container, 'storage, T, D, Self> + ImpliedPredicate<TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, Impls: TraceAttributesMutOp<'container, 'storage, T, D, Self>> + 'container
{
    fn get_attribute_array_value_mut(&'container mut self, storage: &mut T::Storage, index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>>;
    fn set(&'container mut self, storage: &mut T::Storage, index: usize, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>;
    fn append_attribute_array_value(&'container mut self, storage: &mut T::Storage, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>;
    // We tried implementing fn retain_attribute_array_values<F: FnMut(AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>) -> bool>(&'container mut self, storage: &mut T::Storage, predicate: F); - but the rust trait solver is completely lost with that bound and tries to eagerly resolve the trait bounds of Self instead of first solving the recursive ImpliedBounds of TraceProjector. Boo, rust, boo.
    fn swap_attribute_array_values(&mut self, storage: &mut T::Storage, i: usize, j: usize);
    fn truncate_attribute_array_values(&mut self, storage: &mut T::Storage, len: usize);
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>> TraceAttributeSetterTypes<'container, 'storage, T, D, Self> for () {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>> AttributeArrayMutOp<'container, 'storage, T, D> for () {
    fn get_attribute_array_value_mut(&'container mut self, _storage: &mut T::Storage, _index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()>, T, D, Self>> {
        None
    }

    fn set(&'container mut self, _storage: &mut T::Storage, _index: usize, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()>, T, D, ()> {
        match value {
            AttributeAnyValueType::String => AttributeAnyContainer::String(()),
            AttributeAnyValueType::Bytes => AttributeAnyContainer::Bytes(()),
            AttributeAnyValueType::Boolean => AttributeAnyContainer::Boolean(()),
            AttributeAnyValueType::Integer => AttributeAnyContainer::Integer(()),
            AttributeAnyValueType::Double => AttributeAnyContainer::Double(()),
            AttributeAnyValueType::Array => AttributeAnyContainer::Array(()),
            AttributeAnyValueType::Map => AttributeAnyContainer::Map(()),
        }
    }

    fn append_attribute_array_value(&'container mut self, _storage: &mut T::Storage, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()>, T, D, ()> {
        match value {
            AttributeAnyValueType::String => AttributeAnyContainer::String(()),
            AttributeAnyValueType::Bytes => AttributeAnyContainer::Bytes(()),
            AttributeAnyValueType::Boolean => AttributeAnyContainer::Boolean(()),
            AttributeAnyValueType::Integer => AttributeAnyContainer::Integer(()),
            AttributeAnyValueType::Double => AttributeAnyContainer::Double(()),
            AttributeAnyValueType::Array => AttributeAnyContainer::Array(()),
            AttributeAnyValueType::Map => AttributeAnyContainer::Map(()),
        }
    }

    fn swap_attribute_array_values(&mut self, _storage: &mut T::Storage, _i: usize, _j: usize) {}
    fn truncate_attribute_array_values(&mut self, _storage: &mut T::Storage, _len: usize) {}
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C, const ISMUT: u8> AttributeArray<'container, 'storage, T, D, C, ISMUT>
where
    C: AttributeArrayOp<'container, 'storage, T, D>,
{
    pub fn len(&self) -> usize {
        self.container.get_attribute_array_len(self.storage)
    }

    pub fn get(&'container self, index: usize) -> AttributeAnyGetterContainer<'container, 'storage, AttributeArray<'container, 'storage, T, D, C>, T, D, C> {
        self.container.get_attribute_array_value(self.storage, index)
    }

    pub fn iter(&'container self) -> AttributeArrayIter<'container, 'storage, T, D, C> {
        AttributeArrayIter {
            storage: self.storage,
            container: &self.container,
            current_index: 0,
        }
    }
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> AttributeArrayMut<'container, 'storage, T, D, C>
where
    C: AttributeArrayMutOp<'container, 'storage, T, D>,
{
    pub fn get_mut(&'container mut self, index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<C>, C>, T, D, C>> {
        unsafe { self.container.get_attribute_array_value_mut(as_mut(self.storage), index) }
    }

    pub fn append(&'container mut self, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<C>, C>, T, D, C> {
        unsafe { self.container.append_attribute_array_value(as_mut(self.storage), value) }
    }

    pub fn iter_mut(&'container mut self) -> AttributeArrayMutIter<'container, 'storage, T, D, C> {
        AttributeArrayMutIter {
            storage: self.storage,
            container: &mut self.container as *mut C,
            current_index: 0,
            _phantom: PhantomData,
        }
    }

    #[allow(mutable_transmutes)]
    pub fn retain_mut<F>(&'container mut self, mut f: F)
    where
        F: FnMut(AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<C>, C>, T, D, C>) -> bool,
        TraceAttributesMut<'storage, T, D, AttrOwned<C>, C>: TraceAttributesMutOp<'container, 'storage, T, D, C>,
    {
        let len = self.container.get_attribute_array_len(self.storage);
        let mut write = 0;
        for read in 0..len {
            // SAFETY: same raw-pointer pattern as iter_mut. We access each element by index
            // exactly once for the predicate call; swap/truncate are separate calls that
            // don't overlap with the element access.
            let keep = unsafe {
                let container: &'container mut C = std::mem::transmute(&mut self.container);
                let storage: &'storage mut T::Storage = std::mem::transmute(self.storage);
                if let Some(item) = container.get_attribute_array_value_mut(storage, read) {
                    f(item)
                } else {
                    false
                }
            };
            if keep {
                if write != read {
                    unsafe { self.container.swap_attribute_array_values(as_mut(self.storage), write, read) };
                }
                write += 1;
            }
        }
        unsafe { self.container.truncate_attribute_array_values(as_mut(self.storage), write) };
    }
}

pub struct AttributeArrayIter<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> {
    storage: &'storage T::Storage,
    container: &'container C,
    current_index: usize,
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> Iterator for AttributeArrayIter<'container, 'storage, T, D, C>
where
    C: AttributeArrayOp<'container, 'storage, T, D>,
{
    type Item = AttributeAnyGetterContainer<'container, 'storage, AttributeArray<'container, 'storage, T, D, C>, T, D, C>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.current_index;
        if index >= self.container.get_attribute_array_len(self.storage) {
            return None;
        }
        self.current_index += 1;
        Some(self.container.get_attribute_array_value(self.storage, index))
    }
}

pub struct AttributeArrayMutIter<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> {
    storage: &'storage T::Storage,
    container: *mut C,
    current_index: usize,
    _phantom: PhantomData<&'container mut C>,
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> Iterator for AttributeArrayMutIter<'container, 'storage, T, D, C>
where
    C: AttributeArrayMutOp<'container, 'storage, T, D>,
{
    type Item = AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<C>, C>, T, D, C>;

    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: container pointer is valid for 'container; elements at distinct indices
        // are independent, so successive calls do not create aliasing mutable accessors.
        let container: &'container mut C = unsafe { &mut *self.container };
        let index = self.current_index;
        if index >= container.get_attribute_array_len(self.storage) {
            return None;
        }
        self.current_index += 1;
        unsafe { container.get_attribute_array_value_mut(as_mut(self.storage), index) }
    }
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> AttributeArray<'container, 'storage, T, D, C, MUT>
where
    C: AttributeArrayMutOp<'container, 'storage, T, D>,
{
    pub fn set_double(&'container mut self, index: usize, value: f64) {
        let AttributeAnyContainer::Double(container) = (unsafe { self.container.set(as_mut(self.storage), index, AttributeAnyValueType::Double) }) else { unreachable!() };
        container.set(value)
    }

    pub fn set_int(&'container mut self, index: usize, value: i64) {
        let AttributeAnyContainer::Integer(container) = (unsafe { self.container.set(as_mut(self.storage), index, AttributeAnyValueType::Integer) }) else { unreachable!() };
        container.set(value)
    }

    pub fn set_bool(&'container mut self, index: usize, value: bool) {
        let AttributeAnyContainer::Boolean(container) = (unsafe { self.container.set(as_mut(self.storage), index, AttributeAnyValueType::Boolean) }) else { unreachable!() };
        container.set(value)
    }

    pub fn set_string<Val: IntoData<D::Text>>(&'container mut self, index: usize, value: Val) {
        let AttributeAnyContainer::String(container) = (unsafe { self.container.set(as_mut(self.storage), index, AttributeAnyValueType::String) }) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    pub fn set_bytes<Val: IntoData<D::Bytes>>(&'container mut self, index: usize, value: Val) {
        let AttributeAnyContainer::Bytes(container) = (unsafe { self.container.set(as_mut(self.storage), index, AttributeAnyValueType::Bytes) }) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    pub fn set_empty_array(&'container mut self, index: usize) -> AttributeArrayMut<'container, 'storage, T, D, <TraceAttributesMut<'storage, T, D, AttrOwned<C>, C> as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutArray> {
        let AttributeAnyContainer::Array(container) = (unsafe { self.container.set(as_mut(self.storage), index, AttributeAnyValueType::Array) }) else { unreachable!() };
        AttributeArray {
            storage: self.storage,
            container,
            _phantom: PhantomData,
        }
    }

    pub fn set_empty_map(&'container mut self, index: usize) -> TraceAttributesMut<'storage, T, D, AttrOwned<<TraceAttributesMut<'storage, T, D, AttrOwned<C>, C> as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutMap>, <TraceAttributesMut<'storage, T, D, AttrOwned<C>, C> as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutMap> {
        let AttributeAnyContainer::Map(container) = (unsafe { self.container.set(as_mut(self.storage), index, AttributeAnyValueType::Map) }) else { unreachable!() };
        TraceAttributes {
            storage: self.storage,
            container: AttrOwned(container),
            _phantom: PhantomData,
        }
    }

    pub fn get_array_mut(&'container mut self, index: usize) -> Option<AttributeArrayMut<'container, 'storage, T, D, <TraceAttributesMut<'storage, T, D, AttrOwned<C>, C> as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutArray>>
    {
        if let Some(AttributeAnyContainer::Array(container)) = unsafe { self.container.get_attribute_array_value_mut(as_mut(self.storage), index) } {
            Some(AttributeArray {
                storage: self.storage,
                container,
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }

    pub fn get_map_mut(&'container mut self, index: usize) -> Option<TraceAttributesMut<'storage, T, D, AttrOwned<<TraceAttributesMut<'storage, T, D, AttrOwned<C>, C> as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutMap>, <TraceAttributesMut<'storage, T, D, AttrOwned<C>, C> as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutMap>>
    {
        if let Some(AttributeAnyContainer::Map(container)) = unsafe { self.container.get_attribute_array_value_mut(as_mut(self.storage), index) } {
            Some(TraceAttributes {
                storage: self.storage,
                container: AttrOwned(container),
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }

    pub fn pop(&'container mut self) {
        let len = self.container.get_attribute_array_len(self.storage);
        if len > 0 {
            unsafe { self.container.truncate_attribute_array_values(as_mut(self.storage), len - 1) };
        }
    }
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> AttributeArray<'container, 'storage, T, D, C>
where
    C: AttributeArrayOp<'container, 'storage, T, D>,
{
    fn fetch(&'container self, index: usize) -> Option<AttributeAnyGetterContainer<'container, 'storage, AttributeArray<'container, 'storage, T, D, C>, T, D, C>> {
        if index >= self.container.get_attribute_array_len(self.storage) {
            return None;
        }
        Some(self.container.get_attribute_array_value(self.storage, index))
    }

    pub fn get_value(&'container self, index: usize) -> Option<AttributeAnyValue<'container, 'storage, AttributeArray<'container, 'storage, T, D, C>, T, D, C>>
    {
        self.fetch(index).map(move |v| match v {
            AttributeAnyContainer::String(text) => AttributeAnyValue::<AttributeArray<'container, 'storage, T, D, C>, T, D, C>::String(text),
            AttributeAnyContainer::Bytes(bytes) => AttributeAnyValue::<AttributeArray<'container, 'storage, T, D, C>, T, D, C>::Bytes(bytes),
            AttributeAnyContainer::Boolean(boolean) => AttributeAnyValue::<AttributeArray<'container, 'storage, T, D, C>, T, D, C>::Boolean(boolean),
            AttributeAnyContainer::Integer(integer) => AttributeAnyValue::<AttributeArray<'container, 'storage, T, D, C>, T, D, C>::Integer(integer),
            AttributeAnyContainer::Double(double) => AttributeAnyValue::<AttributeArray<'container, 'storage, T, D, C>, T, D, C>::Double(double),
            AttributeAnyContainer::Array(array) => AttributeAnyValue::<AttributeArray<'container, 'storage, T, D, C>, T, D, C>::Array(AttributeArray {
                storage: self.storage,
                container: array,
                _phantom: PhantomData,
            }),
            AttributeAnyContainer::Map(map) => AttributeAnyValue::<AttributeArray<'container, 'storage, T, D, C>, T, D, C>::Map(TraceAttributes {
                storage: self.storage,
                container: AttrOwned(map),
                _phantom: PhantomData,
            }),
        })
    }

    pub fn get_string(&'container self, index: usize) -> Option<&'storage D::Text>
    {
        if let Some(AttributeAnyContainer::String(container)) = self.fetch(index) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_bytes(&'container self, index: usize) -> Option<&'storage D::Bytes>
    {
        if let Some(AttributeAnyContainer::Bytes(container)) = self.fetch(index) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_bool(&'container self, index: usize) -> Option<bool>
    {
        if let Some(AttributeAnyContainer::Boolean(container)) = self.fetch(index) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_int(&'container self, index: usize) -> Option<i64>
    {
        if let Some(AttributeAnyContainer::Integer(container)) = self.fetch(index) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_double(&'container self, index: usize) -> Option<f64>
    {
        if let Some(AttributeAnyContainer::Double(container)) = self.fetch(index) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_array(&'container self, index: usize) -> Option<AttributeArray<'container, 'storage, T, D, <AttributeArray<'container, 'storage, T, D, C> as TraceAttributeGetterTypes<'container, 'storage, T, D, C>>::Array>>
    {
        if let Some(AttributeAnyContainer::Array(container)) = self.fetch(index) {
            Some(AttributeArray {
                storage: self.storage,
                container,
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }

    pub fn get_map(&'container self, index: usize) -> Option<TraceAttributes<'storage, T, D, AttrOwned<<AttributeArray<'container, 'storage, T, D, C> as TraceAttributeGetterTypes<'container, 'storage, T, D, C>>::Map>, <AttributeArray<'container, 'storage, T, D, C> as TraceAttributeGetterTypes<'container, 'storage, T, D, C>>::Map>>
    {
        if let Some(AttributeAnyContainer::Map(container)) = self.fetch(index) {
            Some(TraceAttributes {
                storage: self.storage,
                container: AttrOwned(container),
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }
}
