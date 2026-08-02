pub mod dashboard;
pub mod home;
#[cfg(target_arch = "wasm32")]
pub mod idp;

pub use dashboard::DashboardPanel;
pub use home::HomePanel;
#[cfg(target_arch = "wasm32")]
pub use idp::IdpPanel;
