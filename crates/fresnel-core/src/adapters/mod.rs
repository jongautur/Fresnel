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
#[cfg(target_os = "windows")]
pub mod windows;
// Kept platform-neutral so its correctness is covered by Linux CI too.
pub mod traits;
pub mod windows_convert;

pub use registry::{AdapterListing, AdapterRegistry, ProviderIssue};
pub use traits::WifiAdapterProvider;
