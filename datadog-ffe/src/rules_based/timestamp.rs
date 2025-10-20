//! A simple wrapper around `chrono`, so users don’t have to depend on it directly.
use chrono::{DateTime, Utc};

pub type Timestamp = DateTime<Utc>;

pub fn now() -> Timestamp {
    Utc::now()
}
