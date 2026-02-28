use std::hash::Hash;
use std::marker::PhantomData;
use hashbrown::Equivalent;
use crate::span::{TraceDataLifetime, ImpliedPredicate, SpanDataContents, IntoData};
use super::{TraceProjector, IMMUT, MUT, as_mut};
use super::{AttributeArray, AttributeArrayMut, AttributeArrayOp, AttributeArrayMutOp};

/// Discriminant used when creating or overwriting an attribute value.
///
/// Passed to `set` / `append` methods to specify which variant to allocate.
pub enum AttributeAnyValueType {
    String,
    Bytes,
    Boolean,
    Integer,
    Double,
    Array,
    Map,
}

/// A tagged-union attribute value, parameterised by the concrete holder types for each variant.
///
/// Used as the return type of attribute getters (via the [`AttributeAnyGetterContainer`] alias)
/// and the return/argument type of mutable setters (via [`AttributeAnySetterContainer`]).
pub enum AttributeAnyContainer<String, Bytes, Boolean, Integer, Double, Array, Map> {
    String(String),
    Bytes(Bytes),
    Boolean(Boolean),
    Integer(Integer),
    Double(Double),
    Array(Array),
    Map(Map),
}

/// Lightweight trait providing only `Array` and `Map` associated types.
/// Used by `AttributeAnyGetterContainer` instead of the full `TraceAttributesOp`,
/// so that array/map container types don't need to implement `fn get`.
pub trait TraceAttributeGetterTypes<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C> {
    type Array;
    type Map;
}

/// Lightweight trait providing only the 7 mutable attribute associated types.
/// Used by `AttributeAnySetterContainer` instead of the full `TraceAttributesMutOp`,
/// so that array/map container types don't need to implement `fn get_mut`/`fn set`/`fn remove`.
pub trait TraceAttributeSetterTypes<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container>: TraceAttributeGetterTypes<'container, 'storage, T, D, C>
where
    Self::MutString: TraceAttributesString<'storage, 'storage, T, D>,
    Self::MutBytes: TraceAttributesBytes<'storage, 'storage, T, D>,
    Self::MutBoolean: TraceAttributesBoolean,
    Self::MutInteger: TraceAttributesInteger,
    Self::MutDouble: TraceAttributesDouble,
{
    type MutString;
    type MutBytes;
    type MutBoolean;
    type MutInteger;
    type MutDouble;
    type MutArray: ArrayAttributesMutOp<'container, 'storage, T, D>;
    type MutMap;
}

#[allow(type_alias_bounds)]
pub type AttributeAnyGetterContainer<'container, 'storage, A: TraceAttributeGetterTypes<'container, 'storage, T, D, C>, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    &'storage D::Text,
    &'storage D::Bytes,
    bool,
    i64,
    f64,
    A::Array,
    A::Map,
>;

#[allow(type_alias_bounds)]
pub type AttributeAnySetterContainer<'container, 'storage, A: TraceAttributeSetterTypes<'container, 'storage, T, D, C>, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    A::MutString,
    A::MutBytes,
    A::MutBoolean,
    A::MutInteger,
    A::MutDouble,
    A::MutArray,
    A::MutMap,
>;

#[allow(type_alias_bounds)]
pub type AttributeAnyValue<'container, 'storage, A: TraceAttributeGetterTypes<'container, 'storage, T, D, C>, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    &'storage D::Text,
    &'storage D::Bytes,
    bool,
    i64,
    f64,
    AttributeArray<'container, 'storage, T, D, A::Array>,
    TraceAttributes<'storage, T, D, AttrOwned<A::Map>, A::Map>,
>;

/// Abstraction over owned vs. borrowed container references used inside [`TraceAttributes`].
///
/// The two implementations are [`AttrRef`] (a shared reference, used in the common path) and
/// [`AttrOwned`] (an owned value, used when a container is returned by value from a getter).
pub trait AttrVal<C> {
    unsafe fn as_mut(&self) -> &mut C;
    fn as_ref(&self) -> &C;
}

/// A shared-reference container holder for use in [`TraceAttributes`].
///
/// The inner reference can be unsafely widened to `&mut` when the caller holds exclusive
/// access at a higher level (see [`AttrVal::as_mut`]).
#[derive(Copy, Clone)]
pub struct AttrRef<'a, C>(pub(super) &'a C);
impl<'a, C> AttrVal<C> for AttrRef<'a, C> {
    unsafe fn as_mut(&self) -> &'a mut C {
        as_mut(self.0)
    }

    fn as_ref(&self) -> &'a C {
        self.0
    }
}

/// An owned container holder for use in [`TraceAttributes`].
///
/// Used when a sub-map or sub-array is returned by value from an attribute getter and needs
/// to be wrapped in a [`TraceAttributes`] or [`AttributeArray`] view.
pub struct AttrOwned<C>(pub(super) C);
impl<'a, C: 'a> AttrVal<C> for AttrOwned<C> {
    unsafe fn as_mut(&self) -> &mut C {
        as_mut(&self.0)
    }

    fn as_ref(&self) -> &C {
        &self.0
    }
}

impl<C: Clone> Clone for AttrOwned<C> {
    fn clone(&self) -> Self {
        AttrOwned(self.0.clone())
    }
}

impl<C: Copy> Copy for AttrOwned<C> {}

/// A key-value attribute map view tied to a storage and container.
///
/// Keys are always of type `D::Text`; values are one of the types enumerated by
/// [`AttributeAnyContainer`] (string, bytes, bool, integer, double, array, or nested map).
///
/// The `V` parameter is either [`AttrRef`] (borrowed) or [`AttrOwned`] (owned container).
/// [`TraceAttributesMut`] is the mutable variant, exposing `set_*`, `get_*_mut`, and `remove`.
///
/// Attribute access requires the container to implement [`TraceAttributesOp`] (read) and
/// [`TraceAttributesMutOp`] (write), which are typically derived from [`TraceProjector`]
/// bounds on the containing span/chunk/link/event.
pub struct TraceAttributes<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, V: AttrVal<C>, C, const ISMUT: u8 = IMMUT> {
    pub(super) storage: &'s T::Storage,
    pub(super) container: V,
    pub(super) _phantom: PhantomData<C>,
}
pub type TraceAttributesMut<'s, T, D, V, C> = TraceAttributes<'s, T, D, V, C, MUT>;

impl<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, V: AttrVal<C> + Clone, C> Clone for TraceAttributes<'s, T, D, V, C> { // Note: not for MUT
    fn clone(&self) -> Self {
        TraceAttributes {
            storage: self.storage,
            container: self.container.clone(),
            _phantom: PhantomData,
        }
    }
}
impl<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, A: AttrVal<C> + Copy, C> Copy for TraceAttributes<'s, T, D, A, C> {}

// Helper traits to break the recursion cycle in TraceAttributesOp
/// Helper trait that breaks the recursion cycle in [`TraceAttributesOp`].
///
/// Blanket-implemented for any type that implements [`AttributeArrayOp`].
pub trait ArrayAttributesOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: AttributeArrayOp<'container, 'storage, T, D> + ImpliedPredicate<AttributeArray<'container, 'storage, T, D, Self>, Impls: AttributeArrayOp<'container, 'storage, T, D>>
{}

/// Helper trait that breaks the recursion cycle in [`TraceAttributesOp`].
///
/// Blanket-implemented for any type whose [`TraceAttributes`] specialisation implements
/// [`TraceAttributesOp`] for the same container.
pub trait MapAttributesOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: ImpliedPredicate<TraceAttributes<'storage, T, D, AttrOwned<Self::Container>, Self::Container>, Impls: TraceAttributesOp<'container, 'storage, T, D, Self::Container>> {
    type Container: 'container;
}

// Blanket implementations - any type implementing the base trait gets the helper trait
impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C> ArrayAttributesOp<'container, 'storage, T, D> for C
where
    C: AttributeArrayOp<'container, 'storage, T, D> + 'container,
{}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> MapAttributesOp<'container, 'storage, T, D> for C
where
    TraceAttributes<'storage, T, D, AttrOwned<Self>, Self>: TraceAttributesOp<'container, 'storage, T, D, Self>
{
    type Container = Self;
}

/// Read-only operations on an attribute map for container type `C`.
///
/// Implemented by the projection's [`TraceAttributes`] specialisation for each concrete
/// container type. The single required method `get` performs a key lookup.
pub trait TraceAttributesOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container>:
    TraceAttributeGetterTypes<'container, 'storage, T, D, C>
{
    fn get<K>(container: &'container C, storage: &'storage T::Storage, key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, Self, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>;
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, const ISMUT: u8> TraceAttributeGetterTypes<'container, 'storage, T, D, ()> for TraceAttributes<'storage, T, D, AttrOwned<()>, (), ISMUT> {
    type Array = ();
    type Map = ();
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, const ISMUT: u8> TraceAttributesOp<'container, 'storage, T, D, ()> for TraceAttributes<'storage, T, D, AttrOwned<()>, (), ISMUT> {
    fn get<K>(_container: &'container (), _storage: &'storage T::Storage, _key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, Self, T, D, ()>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        None
    }
}

// Helper traits to break the recursion cycle in TraceAttributesMutOp
/// Helper trait that breaks the recursion cycle in [`TraceAttributesMutOp`].
///
/// Blanket-implemented for any type that implements [`AttributeArrayMutOp`].
pub trait ArrayAttributesMutOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: AttributeArrayMutOp<'container, 'storage, T, D>
{}

/// Helper trait that breaks the recursion cycle in [`TraceAttributesMutOp`].
///
/// Blanket-implemented for any type whose [`TraceAttributesMut`] specialisation implements
/// [`TraceAttributesMutOp`] for the same container.
pub trait MapAttributesMutOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: ImpliedPredicate<TraceAttributesMut<'storage, T, D, AttrOwned<Self::Container>, Self::Container>, Impls: TraceAttributesMutOp<'container, 'storage, T, D, Self::Container>> {
    type Container: 'container;
}

// Blanket implementations - any type implementing the base trait gets the helper trait
impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C> ArrayAttributesMutOp<'container, 'storage, T, D> for C
where
    C: AttributeArrayMutOp<'container, 'storage, T, D> + 'container,
{}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> MapAttributesMutOp<'container, 'storage, T, D> for C
where
    TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>: TraceAttributesMutOp<'container, 'storage, T, D, Self>
{
    type Container = Self;
}

/// Read-write operations on an attribute map for container type `C`.
///
/// Extends [`TraceAttributesOp`] with `get_mut`, `set` (insert/overwrite), and `remove`.
pub trait TraceAttributesMutOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container>: TraceAttributesOp<'container, 'storage, T, D, C> + TraceAttributeSetterTypes<'container, 'storage, T, D, C>
where
    Self::MutString: TraceAttributesString<'storage, 'storage, T, D>,
    Self::MutBytes: TraceAttributesBytes<'storage, 'storage, T, D>,
    Self::MutBoolean: TraceAttributesBoolean,
    Self::MutInteger: TraceAttributesInteger,
    Self::MutDouble: TraceAttributesDouble,
{
    fn get_mut<K>(container: &'container mut C, storage: &mut T::Storage, key: &K) -> Option<AttributeAnySetterContainer<'container, 'storage, Self, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>;
    fn set(container: &'container mut C, storage: &mut T::Storage, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, Self, T, D, C>;
    fn remove<K>(container: &mut C, storage: &mut T::Storage, key: &K)
    where
        K: ?Sized + Hash + Equivalent<D::Text>;
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>> TraceAttributeSetterTypes<'container, 'storage, T, D, ()> for TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>> TraceAttributesMutOp<'container, 'storage, T, D, ()> for TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()> {
    fn get_mut<K>(_container: &'container mut (), _storage: &mut T::Storage, _key: &K) -> Option<AttributeAnySetterContainer<'container, 'storage, Self, T, D, ()>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>,
    {
        None
    }

    fn set(_container: &'container mut (), _storage: &mut T::Storage, _key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, Self, T, D, ()> {
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

    fn remove<K>(_container: &mut (), _storage: &mut T::Storage, _key: &K)
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
    }
}

/// Accessor/mutator for a string-typed attribute slot.
pub trait TraceAttributesString<'s, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> {
    fn get(&self, storage: &'a T::Storage) -> &'s D::Text;
    fn set(self, storage: &mut T::Storage, value: D::Text);
}

impl<'s, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> TraceAttributesString<'s, 'a, T, D> for () {
    fn get(&self, _storage: &'a T::Storage) -> &'s D::Text {
        D::Text::default_ref()
    }

    fn set(self, _storage: &mut T::Storage, _value: D::Text) {
    }
}

/// Accessor/mutator for a bytes-typed attribute slot.
pub trait TraceAttributesBytes<'s, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> {
    fn get(&self, storage: &'a T::Storage) -> &'a D::Bytes;
    fn set(self, storage: &mut T::Storage, value: D::Bytes);
}

impl<'s, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> TraceAttributesBytes<'s, 'a, T, D> for () {
    fn get(&self, _storage: &'a T::Storage) -> &'a D::Bytes {
        D::Bytes::default_ref()
    }

    fn set(self, _storage: &mut T::Storage, _value: D::Bytes) {
    }
}


/// Accessor/mutator for an integer-typed attribute slot.
pub trait TraceAttributesInteger {
    fn get(&self) -> i64;
    fn set(self, value: i64);
}

impl TraceAttributesInteger for () {
    fn get(&self) -> i64 {
        0
    }

    fn set(self, _value: i64) {
    }
}

/// Accessor/mutator for a boolean-typed attribute slot.
pub trait TraceAttributesBoolean {
    fn get(&self) -> bool;
    fn set(self, value: bool);
}

impl TraceAttributesBoolean for () {
    fn get(&self) -> bool {
        false
    }

    fn set(self, _value: bool) {
    }
}

/// Accessor/mutator for a double-typed attribute slot.
pub trait TraceAttributesDouble {
    fn get(&self) -> f64;
    fn set(self, value: f64);
}

impl TraceAttributesDouble for () {
    fn get(&self) -> f64 {
        0.
    }

    fn set(self, _value: f64) {
    }
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> TraceAttributes<'storage, T, D, AttrRef<'container, C>, C, MUT>
where
    TraceAttributes<'storage, T, D, AttrRef<'container, C>, C, MUT>: TraceAttributesMutOp<'container, 'storage, T, D, C>,
{
    pub fn set_double<K: IntoData<D::Text>>(&'container mut self, key: K, value: f64) {
        let container_ref = unsafe { self.container.as_mut() };
        let storage_ref = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Double(container) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Double) else { unreachable!() };
        container.set(value)
    }

    pub fn set_int<K: IntoData<D::Text>>(&'container mut self, key: K, value: i64) {
        let container_ref = unsafe { self.container.as_mut() };
        let storage_ref = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Integer(container) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Integer) else { unreachable!() };
        container.set(value)
    }

    pub fn set_bool<K: IntoData<D::Text>>(&'container mut self, key: K, value: bool) {
        let container_ref = unsafe { self.container.as_mut() };
        let storage_ref = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Boolean(container) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Boolean) else { unreachable!() };
        container.set(value)
    }

    pub fn set_string<K: IntoData<D::Text>, Val: IntoData<D::Text>>(&'container mut self, key: K, value: Val) {
        let container_ref = unsafe { self.container.as_mut() };
        let storage_ref = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::String(container) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::String) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    pub fn set_bytes<K: IntoData<D::Text>, Val: IntoData<D::Bytes>>(&'container mut self, key: K, value: Val) {
        let container_ref = unsafe { self.container.as_mut() };
        let storage_ref = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Bytes(container) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Bytes) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    pub fn set_empty_array<K: IntoData<D::Text>>(&'container mut self, key: K) -> AttributeArrayMut<'container, 'storage, T, D, <Self as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutArray> {
        let container_ref = unsafe { self.container.as_mut() };
        let storage_ref = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Array(container) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Array) else { unreachable!() };
        AttributeArray {
            storage: self.storage,
            container,

            _phantom: PhantomData,
        }
    }

    pub fn set_empty_map<K: IntoData<D::Text>>(&'container mut self, key: K) -> TraceAttributesMut<'storage, T, D, AttrOwned<<Self as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutMap>, <Self as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutMap> {
        let container_ref = unsafe { self.container.as_mut() };
        let storage_ref = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Map(container) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Map) else { unreachable!() };
        TraceAttributes {
            storage: self.storage,
            container: AttrOwned(container),
            _phantom: PhantomData,
        }
    }

    pub fn get_array_mut<K>(&'container mut self, key: &K) -> Option<AttributeArrayMut<'container, 'storage, T, D, <Self as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutArray>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        let container_ref = unsafe { self.container.as_mut() };
        let storage_ref = unsafe { as_mut(self.storage) };
        if let Some(AttributeAnyContainer::Array(container)) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::get_mut(container_ref, storage_ref, key) {
            Some(AttributeArray {
                storage: self.storage,
                container,
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }

    pub fn get_map_mut<K>(&'container mut self, key: &K) -> Option<TraceAttributesMut<'storage, T, D, AttrOwned<<Self as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutMap>, <Self as TraceAttributeSetterTypes<'container, 'storage, T, D, C>>::MutMap>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        let container_ref = unsafe { self.container.as_mut() };
        let storage_ref = unsafe { as_mut(self.storage) };
        if let Some(AttributeAnyContainer::Map(container)) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::get_mut(container_ref, storage_ref, key) {
            Some(TraceAttributes {
                storage: self.storage,
                container: AttrOwned(container),
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }

    pub fn remove<K>(&'container mut self, key: &K)
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        let container_ref = unsafe { self.container.as_mut() };
        let storage_ref = unsafe { as_mut(self.storage) };
        <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::remove(container_ref, storage_ref, key);
    }
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, V: AttrVal<C>, C: 'container> TraceAttributes<'storage, T, D, V, C>
where
    TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>: TraceAttributesOp<'container, 'storage, T, D, C>,
{
    #[allow(invalid_reference_casting)]
    fn fetch<K>(&self, key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        let container_ref: &'container C = unsafe { &*(self.container.as_ref() as *const _) };
        <TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributesOp<'container, 'storage, T, D, C>>::get(container_ref, self.storage, key)
    }

    pub fn get<K>(&self, key: &K) -> Option<AttributeAnyValue<'container, 'storage, TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        self.fetch(key).map(move |v| match v {
            AttributeAnyContainer::String(text) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::String(text),
            AttributeAnyContainer::Bytes(bytes) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Bytes(bytes),
            AttributeAnyContainer::Boolean(boolean) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Boolean(boolean),
            AttributeAnyContainer::Integer(integer) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Integer(integer),
            AttributeAnyContainer::Double(double) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Double(double),
            AttributeAnyContainer::Array(array) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Array(AttributeArray {
                storage: self.storage,
                container: array,
                _phantom: PhantomData,
            }),
            AttributeAnyContainer::Map(map) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Map(TraceAttributes {
                storage: self.storage,
                container: AttrOwned(map),
                _phantom: PhantomData,
            }),
        })
    }

    pub fn get_string<K>(&self, key: &K) -> Option<&'storage D::Text>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        if let Some(AttributeAnyContainer::String(container)) = self.fetch(key) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_bytes<K>(&self, key: &K) -> Option<&'storage D::Bytes>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        if let Some(AttributeAnyContainer::Bytes(container)) = self.fetch(key) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_bool<K>(&self, key: &K) -> Option<bool>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        if let Some(AttributeAnyContainer::Boolean(container)) = self.fetch(key) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_int<K>(&self, key: &K) -> Option<i64>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        if let Some(AttributeAnyContainer::Integer(container)) = self.fetch(key) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_double<K>(self, key: &K) -> Option<f64>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        if let Some(AttributeAnyContainer::Double(container)) = self.fetch(key) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_array<K>(&self, key: &K) -> Option<AttributeArray<'container, 'storage, T, D, <TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributeGetterTypes<'container, 'storage, T, D, C>>::Array>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        if let Some(AttributeAnyContainer::Array(container)) = self.fetch(key) {
            Some(AttributeArray {
                storage: self.storage,
                container,
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }


    pub fn get_map<K>(&self, key: &K) -> Option<TraceAttributes<'storage, T, D, AttrOwned<<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributeGetterTypes<'container, 'storage, T, D, C>>::Map>, <TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributeGetterTypes<'container, 'storage, T, D, C>>::Map>>
    where
        K: ?Sized + Hash + Equivalent<D::Text>
    {
        if let Some(AttributeAnyContainer::Map(container)) = self.fetch(key) {
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
