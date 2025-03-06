// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::iter::Sum;
use std::ops::Add;

#[derive(Debug, Default, Serialize, Deserialize)]
/// `EnqueuedTelemetryStats`contains the count of stored and unflushed dependencies, configurations,
/// and integrations. It also keeps track of the count of metrics, points, actions, and computed
/// dependencies.
pub struct EnqueuedTelemetryStats {
    pub dependencies_stored: u32,
    pub dependencies_unflushed: u32,
    pub configurations_stored: u32,
    pub configurations_unflushed: u32,
    pub integrations_stored: u32,
    pub integrations_unflushed: u32,
    pub metrics: u32,
    pub points: u32,
    pub actions: u32,
    pub computed_dependencies: u32,
}

impl Add for EnqueuedTelemetryStats {
    type Output = Self;

    /// Adds two `EnqueuedTelemetryStats` instances together.
    ///
    /// # Arguments
    ///
    /// * `rhs` - An `EnqueuedTelemetryStats` instance that needs to be added to the current
    ///   instance.
    ///
    /// # Returns
    ///
    /// * A new `EnqueuedTelemetryStats` instance that is the result of the addition.
    fn add(self, rhs: Self) -> Self::Output {
        EnqueuedTelemetryStats {
            dependencies_stored: self.dependencies_stored + rhs.dependencies_stored,
            dependencies_unflushed: self.dependencies_unflushed + rhs.dependencies_unflushed,
            configurations_stored: self.configurations_stored + rhs.configurations_stored,
            configurations_unflushed: self.configurations_unflushed + rhs.configurations_unflushed,
            integrations_stored: self.integrations_stored + rhs.integrations_stored,
            integrations_unflushed: self.integrations_unflushed + rhs.integrations_unflushed,
            metrics: self.metrics + rhs.metrics,
            points: self.points + rhs.points,
            actions: self.actions + rhs.actions,
            computed_dependencies: self.computed_dependencies + rhs.computed_dependencies,
        }
    }
}

impl Sum for EnqueuedTelemetryStats {
    /// Sums up a series of `EnqueuedTelemetryStats` instances.
    ///
    /// # Arguments
    ///
    /// * `iter` - An iterator that yields `EnqueuedTelemetryStats` instances.
    ///
    /// # Returns
    ///
    /// * A new `EnqueuedTelemetryStats` instance that is the result of the summation.
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |a, b| a + b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        let stats1 = EnqueuedTelemetryStats {
            dependencies_stored: 1,
            dependencies_unflushed: 2,
            configurations_stored: 3,
            configurations_unflushed: 4,
            integrations_stored: 5,
            integrations_unflushed: 6,
            metrics: 7,
            points: 8,
            actions: 9,
            computed_dependencies: 10,
        };

        let stats2 = EnqueuedTelemetryStats {
            dependencies_stored: 10,
            dependencies_unflushed: 20,
            configurations_stored: 30,
            configurations_unflushed: 40,
            integrations_stored: 50,
            integrations_unflushed: 60,
            metrics: 70,
            points: 80,
            actions: 90,
            computed_dependencies: 100,
        };

        let result = stats1 + stats2;

        assert_eq!(result.dependencies_stored, 11);
        assert_eq!(result.dependencies_unflushed, 22);
        assert_eq!(result.configurations_stored, 33);
        assert_eq!(result.configurations_unflushed, 44);
        assert_eq!(result.integrations_stored, 55);
        assert_eq!(result.integrations_unflushed, 66);
        assert_eq!(result.metrics, 77);
        assert_eq!(result.points, 88);
        assert_eq!(result.actions, 99);
        assert_eq!(result.computed_dependencies, 110);
    }

    #[test]
    fn test_sum() {
        let stats1 = EnqueuedTelemetryStats {
            dependencies_stored: 1,
            dependencies_unflushed: 2,
            configurations_stored: 3,
            configurations_unflushed: 4,
            integrations_stored: 5,
            integrations_unflushed: 6,
            metrics: 7,
            points: 8,
            actions: 9,
            computed_dependencies: 10,
        };

        let stats2 = EnqueuedTelemetryStats {
            dependencies_stored: 10,
            dependencies_unflushed: 20,
            configurations_stored: 30,
            configurations_unflushed: 40,
            integrations_stored: 50,
            integrations_unflushed: 60,
            metrics: 70,
            points: 80,
            actions: 90,
            computed_dependencies: 100,
        };

        let stats_vec = vec![stats1, stats2];
        let result: EnqueuedTelemetryStats = stats_vec.into_iter().sum();

        assert_eq!(result.dependencies_stored, 11);
        assert_eq!(result.dependencies_unflushed, 22);
        assert_eq!(result.configurations_stored, 33);
        assert_eq!(result.configurations_unflushed, 44);
        assert_eq!(result.integrations_stored, 55);
        assert_eq!(result.integrations_unflushed, 66);
        assert_eq!(result.metrics, 77);
        assert_eq!(result.points, 88);
        assert_eq!(result.actions, 99);
        assert_eq!(result.computed_dependencies, 110);
    }
}
