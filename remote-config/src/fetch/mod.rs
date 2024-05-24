#[cfg(any(test, feature = "test"))]
pub mod test_server;
mod fetcher;
mod single;
mod shared;
mod multitarget;

#[cfg_attr(test, allow(ambiguous_glob_reexports))] // ignore mod tests re-export
pub use fetcher::*;
pub use single::*;
pub use shared::*;
pub use multitarget::*;
