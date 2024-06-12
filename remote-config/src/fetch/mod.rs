mod fetcher;
mod multitarget;
mod shared;
mod single;
#[cfg(any(test, feature = "test"))]
pub mod test_server;

#[cfg_attr(test, allow(ambiguous_glob_reexports))] // ignore mod tests re-export
pub use fetcher::*;
pub use multitarget::*;
pub use shared::*;
pub use single::*;
