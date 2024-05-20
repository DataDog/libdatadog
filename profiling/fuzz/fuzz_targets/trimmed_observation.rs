#![no_main]

use arbitrary::{Arbitrary, Result, Unstructured};
use libfuzzer_sys::fuzz_target;

use datadog_profiling::internal::observation::trimmed_observation::{
    ObservationLength, TrimmedObservation,
};

#[derive(Debug)]
struct TrimmedObservationInput {
    v: Vec<i64>,
    o: ObservationLength,
}

impl<'a> Arbitrary<'a> for TrimmedObservationInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let v = Vec::<i64>::arbitrary(u)?;
        let o = ObservationLength::new(v.len());
        Ok(TrimmedObservationInput { v, o })
    }
}

fuzz_target!(|input: TrimmedObservationInput| {
    let TrimmedObservationInput { v, o } = input;
    {
        let mut t = TrimmedObservation::new(v.clone(), o);
        unsafe {
            assert_eq!(t.as_mut_slice(o), &v);
            assert_eq!(t.into_boxed_slice(o).as_ref(), v.as_slice());
        }
    }
    {
        let mut t = TrimmedObservation::new(v.clone(), o);
        unsafe {
            assert_eq!(t.as_mut_slice(o), &v);
            t.consume(o);
        }
    }
    {
        let mut t = TrimmedObservation::new(v.clone(), o);
        unsafe {
            assert_eq!(t.as_mut_slice(o), &v);
            assert_eq!(t.into_vec(o), v);
        }
    }
});
