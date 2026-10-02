//! Hardware abstraction: everything that knows how to talk to a particular
//! kind of Wi-Fi hardware or OS service lives under this module.

#[cfg(test)]
pub mod fake;
#[cfg(target_os = "linux")]
pub mod networkmanager;
#[cfg(target_os = "linux")]
pub mod nl80211;
pub mod registry;
#[cfg(target_os = "linux")]
pub mod sysfs;
pub mod traits;

pub use registry::{AdapterListing, AdapterRegistry, ProviderIssue};
pub use traits::WifiAdapterProvider;
