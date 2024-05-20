#![no_main]

use arbitrary::{Arbitrary, Result, Unstructured};
use libfuzzer_sys::fuzz_target;

use datadog_profiling::internal::observation::trimmed_observation::{
    ObservationLength, TrimmedObservation,
};

#[derive(Debug)]
struct Input {
    v: Vec<i64>,
    o: ObservationLength,
}

impl<'a> Arbitrary<'a> for Input {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let v = Vec::<i64>::arbitrary(u)?;
        let o = ObservationLength::new(v.len());
        Ok(Input { v, o })
    }
}

fuzz_target!(|input: Input| {
    let Input { v, o } = input;
    let t = TrimmedObservation::new(v, o);
    unsafe {
        t.consume(o);
    }
});
