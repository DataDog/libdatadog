use std::collections::{HashMap, VecDeque};

use prost::Message;

pub mod pb;

/// This is a minimal DDSketch implementation
///
/// It only supports a part of the standard (which is also only the parts dd backend supports :shrug:)
/// * max length contiguous bin store, with lower bin
/// collapse behavior.
/// * Positive or zero values
///
/// The default sketch has a 1% relative accuracy, and only accepts positive points
#[derive(Debug, Default, Clone)]
pub struct DDSketch {
    store: LowCollapsingDenseStore,
    zero_count: f64,
    mapping: LogMapping,
}

impl DDSketch {
    pub fn ordered_bins(&self) -> Vec<(f64, f64)> {
        let mut bins: Vec<_> = std::iter::once((0.0, self.zero_count))
            .chain(
                self.store
                    .buckets()
                    .map(|(b, v)| (self.mapping.value(b), v)),
            )
            .collect();
        bins.sort_by(|a, b| a.0.total_cmp(&b.0));
        bins
    }

    pub fn add(&mut self, point: f64) -> Option<()> {
        self.add_with_count(point, 1.0)
    }

    pub fn add_with_count(&mut self, point: f64, count: f64) -> Option<()> {
        if count.is_nan() || count.is_infinite() {
            return None;
        }
        if point < 0.0 || point.is_nan() || point.is_infinite() {
            return None;
        } else if point < self.mapping.min_indexable_value {
            self.zero_count += count;
        } else {
            let index = self.mapping.index(point);
            *self.store.bucket_mut(index) += count;
        }
        Some(())
    }

    pub fn into_pb(self) -> pb::DdSketch {
        let contiguous_bins: Vec<f64> = self.store.bins.into();
        pb::DdSketch {
            mapping: Some(pb::IndexMapping {
                gamma: self.mapping.gamma,
                index_offset: self.mapping.index_offset,
                interpolation: pb::index_mapping::Interpolation::None.into(),
            }),
            positive_values: Some(pb::Store {
                bin_counts: HashMap::new(),
                contiguous_bin_counts: contiguous_bins,
                contiguous_bin_index_offset: self.store.offset,
            }),
            zero_count: self.zero_count,
            negative_values: Some(pb::Store {
                bin_counts: HashMap::new(),
                contiguous_bin_counts: Vec::new(),
                contiguous_bin_index_offset: 0,
            }),
        }
    }

    pub fn encode_to_vec(self) -> Vec<u8> {
        self.into_pb().encode_to_vec()
    }
}

#[derive(Debug, Clone)]
struct LowCollapsingDenseStore {
    bins: VecDeque<f64>,
    offset: i32,
    max_size: i32,
}

impl LowCollapsingDenseStore {
    fn new(max_size: i32) -> Option<Self> {
        if max_size < 0 {
            return None;
        }
        Some(Self {
            bins: VecDeque::new(),
            offset: 0,
            max_size,
        })
    }

    fn buckets(&self) -> impl Iterator<Item = (i32, f64)> + '_ {
        self.bins
            .iter()
            .enumerate()
            .map(|(i, &v)| (i as i32 + self.offset, v))
    }

    fn bucket_mut(&mut self, index: i32) -> &mut f64 {
        let idx = self.bucket_idx_to_bin_idx(index);
        &mut self.bins[idx]
    }

    fn bucket_idx_to_bin_idx(&mut self, bucket_index: i32) -> usize {
        if self.bins.is_empty() {
            // If the bins are empty, start them at the index

            self.offset = bucket_index;
            self.bins.push_back(0.0);
            return 0;
        }

        // General case
        if bucket_index < self.offset {
            let additional_low_bins = self.offset - bucket_index;
            debug_assert!(additional_low_bins >= 0);

            let additional_low_bins = std::cmp::min(
                additional_low_bins as usize,
                self.max_size as usize - self.bins.len(),
            );
            self.bins.reserve(self.bins.len() + additional_low_bins);
            for _ in 0..additional_low_bins {
                self.bins.push_front(0.0);
            }
            self.offset = self.offset - additional_low_bins as i32;
            return 0;
        } else if self.offset + self.bins.len() as i32 <= bucket_index {
            let bin_range_size = bucket_index - self.offset + 1;
            if bin_range_size > self.max_size {
                self.collapse_low_bins(bin_range_size - self.max_size);
            }

            debug_assert!(self.bins.len() as i32 <= self.max_size);
            let bin_index = bucket_index - self.offset;
            for _ in 0..(bin_index - self.bins.len() as i32 + 1) {
                self.bins.push_back(0.0);
            }
            return (bucket_index - self.offset) as usize;
        } else {
            return (bucket_index - self.offset) as usize;
        }
    }

    fn collapse_low_bins(&mut self, bin_number: i32) {
        let mut count = 0.0;
        for _ in 0..bin_number {
            count += self.bins.pop_front().unwrap_or(0.0);
        }
        if let Some(lowest_bin) = self.bins.front_mut() {
            *lowest_bin += count;
        } else {
            self.bins.push_front(count);
        }
        self.offset += bin_number;
    }
}

impl Default for LowCollapsingDenseStore {
    fn default() -> Self {
        Self::new(2048).unwrap()
    }
}

#[derive(Debug, Clone, Copy)]
struct LogMapping {
    gamma: f64,
    multiplier: f64,
    min_indexable_value: f64,
    index_offset: f64,
}

impl LogMapping {
    fn new(gamma: f64, offset: f64) -> Option<Self> {
        if gamma <= 1.0 {
            return None;
        }
        let multiplier = Self::multiplier_from_gamma(gamma);
        Some(Self {
            gamma,
            multiplier: multiplier,
            min_indexable_value: max(
                std::f64::MIN_POSITIVE * gamma,
                ((i32::MIN as f64 - offset) / multiplier + 1.0).exp(),
            )?,
            index_offset: offset,
        })
    }
    fn multiplier_from_gamma(gamma: f64) -> f64 {
        1.0 / gamma.ln()
    }

    fn relative_accuracy(&self) -> f64 {
        1.0 - 2.0 / (1.0 + self.gamma)
    }

    fn index(&self, value: f64) -> i32 {
        (value.ln() * self.multiplier + self.index_offset).floor() as i32
    }
    fn value(&self, index: i32) -> f64 {
        ((index as f64 - self.index_offset) / self.multiplier).exp()
            * (1.0 + self.relative_accuracy())
    }
}

impl Default for LogMapping {
    fn default() -> Self {
        const RELATIVE_ACCURACY: f64 = 0.007751937984496124;
        const GAMMA: f64 = (1.0 + RELATIVE_ACCURACY) / (1.0 - RELATIVE_ACCURACY);

        const BACKEND_SKETCH_MIN_VALUE: f64 = 1e-9;
        // offset used in datadog's backend for sketches
        let offset: f64 = (1.0 - (BACKEND_SKETCH_MIN_VALUE.ln() / GAMMA.ln()).floor()) + 0.5;
        
        Self::new(GAMMA, offset).unwrap()
    }
}

fn max(a: f64, b: f64) -> Option<f64> {
    if a.is_nan() || b.is_nan() {
        None
    } else if a > b {
        Some(a)
    } else {
        Some(b)
    }
}

#[cfg(test)]
mod test {
    use prost::Message;

    use super::*;

    macro_rules! assert_within {
        ($x:expr, $y:expr, $tolerance:expr) => {
            let diff = $x - $y;
            assert!(
                -$tolerance < diff && diff < $tolerance,
                "x: {} y: {}",
                $x,
                $y,
            );
        };
    }

    #[test]
    fn test_exponential_mapping_within_tolerances() {
        let mapping = LogMapping::default();

        let values: &[f64] = &[1e-30, 0.1, 2.0, 10.0, 25.0, 10000.0];
        for &value in values {
            let index = mapping.index(value);
            let value_bucket = mapping.value(index);

            assert_within!(value_bucket / value, 1.0, 0.01);
        }
    }

    #[test]
    fn test_exponential_mapping_realtive_accuracy() {
        let mapping = LogMapping::default();

        assert_within!(mapping.relative_accuracy(), 0.01, f64::EPSILON);
    }

    #[test]
    fn test_skecth_add() {
        let mut sketch = DDSketch::default();
        let points: &[f64] = &[0.0, 1e-5, 0.1, 2.0, 10.0, 25.0, 10000.0];
        for (i, &point) in points.into_iter().enumerate() {
            assert!(sketch.add_with_count(point, i as f64 + 1.0).is_some());
        }

        dbg!(sketch.store.bins.len(), sketch.store.offset);

        for (i, (value, count)) in sketch
            .ordered_bins()
            .into_iter()
            .filter(|(_, p)| *p != 0.0)
            .enumerate()
        {
            if points[i] == 0.0 {
                assert_within!(value, 0.0, f64::EPSILON);
                assert_within!(count, i as f64 + 1.0, f64::EPSILON);
            } else {
                assert_within!(value / points[i], 1.0, 0.01);
                assert_within!(count, i as f64 + 1.0, f64::EPSILON);
            }
        }
    }

    #[test]
    fn test_skecth_add_negative() {
        let mut sketch = DDSketch::default();
        assert!(sketch.add(-1.0).is_none());
    }

    #[test]
    fn test_skecth_add_nan() {
        let mut sketch = DDSketch::default();
        assert!(sketch.add(f64::NAN).is_none());
    }

    #[test]
    fn test_skecth_encode() {
        let mut sketch = DDSketch::default();
        let points: &[f64] = &[0.0, 1e-30, 0.1, 2.0, 10.0, 25.0, 10000.0];
        for (i, &point) in points.into_iter().enumerate() {
            assert!(sketch.add_with_count(point, i as f64).is_some());
        }

        let pb_sketch = sketch.into_pb().encode_to_vec();
        assert!(pb_sketch.len() != 0);
    }

    #[test]
    fn test_low_collapsing_store() {
        let mut store = LowCollapsingDenseStore::new(5).unwrap();

        // Test initial push up to capacity
        for i in 0..5 {
            *store.bucket_mut(i + 10) = 1.0;
        }
        for (i, b) in store.buckets().enumerate() {
            assert_eq!(b.0, i as i32 + 10);
            assert_eq!(b.1, 1.0)
        }

        // Indexing existing bins
        for i in 0..5 {
            *store.bucket_mut(i + 10) += 1.0;
        }
        for (i, b) in store.buckets().enumerate() {
            assert_eq!(b.0, i as i32 + 10);
            assert_eq!(b.1, 2.0)
        }
    }

    #[test]
    fn test_low_collapsing_store_low_bins_are_collapsed() {
        let mut store = LowCollapsingDenseStore::new(5).unwrap();

        // Test initial push up to capacity to max
        for i in 0..5 {
            *store.bucket_mut(i + 10) = 1.0;
        }

        // Indexing low bins at max capacity
        for i in 0..3 {
            *store.bucket_mut(i) += 1.0;
        }
        for (i, b) in store.buckets().enumerate() {
            assert_eq!(b.0, i as i32 + 10);
            if i == 0 {
                assert_eq!(b.1, 4.0)
            } else {
                assert_eq!(b.1, 1.0)
            }
        }

        // Indexing higer bins collapses lower bins
        *store.bucket_mut(15) = 1.0;
        for (i, b) in store.buckets().enumerate() {
            assert_eq!(b.0, i as i32 + 11);
            if i == 0 {
                assert_eq!(b.1, 5.0)
            } else {
                assert_eq!(b.1, 1.0)
            }
        }
    }

    #[test]
    fn test_low_collapsing_store_up_expansion() {
        let mut store = LowCollapsingDenseStore::new(3).unwrap();

        *store.bucket_mut(1) = 1.0;
        *store.bucket_mut(3) = 1.0;
        assert_eq!(
            store.buckets().collect::<Vec<_>>(),
            &[(1, 1.0), (2, 0.0), (3, 1.0)]
        )
    }

    #[test]
    fn test_low_collapsing_store_down_expansion() {
        let mut store = LowCollapsingDenseStore::new(3).unwrap();

        *store.bucket_mut(3) = 1.0;
        *store.bucket_mut(1) = 1.0;
        assert_eq!(
            store.buckets().collect::<Vec<_>>(),
            &[(1, 1.0), (2, 0.0), (3, 1.0)]
        )
    }
}
