/// SDK metadata that is used in a couple of places:
/// - added to assignment and bandit events
/// - sent in query when requesting config
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdkMetadata {
    /// SDK name. (Usually, language name.)
    pub name: &'static str,
    /// Version of SDK.
    pub version: &'static str,
}
