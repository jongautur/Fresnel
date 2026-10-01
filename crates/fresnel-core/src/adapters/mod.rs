//! Hardware abstraction: everything that knows how to talk to a particular
//! kind of Wi-Fi hardware or OS service lives under this module.

pub mod networkmanager;
pub mod registry;
pub mod sysfs;
pub mod traits;

pub use registry::{AdapterListing, AdapterRegistry, ProviderIssue};
pub use traits::WifiAdapterProvider;
