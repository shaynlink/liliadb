//! Access control of the endpoints and secrets only the current user may reach (the `ipc`
//! feature): on macOS, paths whose ACLs grant access beyond their mode bits are refused; on
//! Windows, named pipes and the daemon's token are created with a protected, current-user-only
//! security descriptor.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::reject_acl_grants;
#[cfg(windows)]
pub use windows::{Credentials, UserSecurity};

#[cfg(any(target_os = "macos", windows))]
fn denied(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, message)
}
