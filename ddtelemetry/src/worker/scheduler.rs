// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Scheduler<T: Clone + Eq> {
    pub delays: Vec<(Duration, T)>,
    pub deadlines: Vec<(Instant, T)>,
    now: Now,
}

impl<T: Clone + Eq> Scheduler<T> {
    pub fn new(mut delays: Vec<(Duration, T)>) -> Self {
        delays.sort_by_key(|(d, _)| *d);
        Self {
            delays,
            deadlines: Vec::new(),
            now: Now::Std,
        }
    }
    pub fn next_deadline(&self) -> Option<(Instant, &T)> {
        let (i, key) = self.deadlines.first()?;
        Some((*i, key))
    }

    pub fn schedule_events(&mut self, events: &mut impl Iterator<Item = T>) -> Result<(), T> {
        let now = self.now.now();
        for ev in events {
            self.schedule_event_with_from(ev, now)?;
        }
        Ok(())
    }

    fn schedule_event_with_from(&mut self, event: T, from: Instant) -> Result<(), T> {
        let (delay, _) = match self.delays.iter().find(|(_, k)| k == &event) {
            Some(s) => s,
            None => return Err(event),
        };
        let deadline = from + *delay;
        if let Some((idx, _)) = self
            .deadlines
            .iter()
            .enumerate()
            .find(|(_, (_, k))| k == &event)
        {
            self.deadlines.remove(idx);
        }
        let insert_idx = self
            .deadlines
            .binary_search_by(|(d, _)| d.cmp(&deadline))
            .unwrap_or_else(|e| e);
        self.deadlines.insert(insert_idx, (deadline, event));
        Ok(())
    }

    pub fn schedule_event(&mut self, event: T) -> Result<(), T> {
        self.schedule_event_with_from(event, self.now.now())
    }

    pub fn clear_pending(&mut self) {
        self.deadlines.clear();
    }
}

#[derive(Debug)]
enum Now {
    Std,
    #[cfg(test)]
    Mock(Instant),
}

impl Now {
    fn now(&self) -> Instant {
        match self {
            Self::Std => Instant::now(),
            #[cfg(test)]
            Self::Mock(now) => *now,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use super::*;

    fn expect_scheduled<T: Clone + Eq + Debug>(
        scheduler: &Scheduler<T>,
        expected_event: T,
        expected_scheduled_after: Duration,
        now: Instant,
    ) {
        let next_deadline = scheduler.next_deadline().unwrap();
        let scheduled_in = next_deadline.0.duration_since(now);

        assert_eq!(next_deadline.1.clone(), expected_event);
        assert!(expected_scheduled_after - Duration::from_nanos(10) < scheduled_in);
    }

    #[test]
    fn test_schedule() {
        let start = Instant::now();
        let mut scheduler = Scheduler::new(vec![
            (Duration::from_millis(20), 0),
            (Duration::from_millis(10), 1),
            (Duration::from_millis(40), 2),
        ]);
        scheduler.now = Now::Mock(start);
        scheduler
            .schedule_events(&mut [0, 1, 2].into_iter())
            .unwrap();

        scheduler.now = Now::Mock(start + Duration::from_millis(9));
        expect_scheduled(
            &scheduler,
            1,
            Duration::from_millis(1),
            start + Duration::from_millis(9),
        );
        scheduler.now = Now::Mock(start + Duration::from_millis(11));
        scheduler.schedule_event(1).unwrap();

        expect_scheduled(
            &scheduler,
            0,
            Duration::from_millis(9),
            start + Duration::from_millis(11),
        );
        scheduler.schedule_event(0).unwrap();

        scheduler.now = Now::Mock(start + Duration::from_millis(19));
        expect_scheduled(
            &scheduler,
            1,
            Duration::from_millis(1),
            start + Duration::from_millis(19),
        );
    }
}
