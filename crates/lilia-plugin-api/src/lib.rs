mod install;
mod loader;
mod manifest;
mod package;
mod packaging;
mod trust_store;
#[cfg(not(target_arch = "wasm32"))]
pub mod wasm_runtime;

pub use install::{discover_installed, find_installed, install_verified, InstalledPluginSummary};
pub use loader::{
    load_descriptor, load_descriptor_with_config, load_plugin, load_plugin_with_config,
    LoadedPlugin, PluginCodec, PluginSql,
};
pub use manifest::{
    host_target, is_wasm_target, platform_library_extension, Capability, CodecDescriptorV1,
    PluginDescriptorV1, PluginManifest, SqlDescriptorV1, ABI_MAJOR, ABI_MINOR, WASM_TARGET_UNKNOWN,
    WASM_TARGET_WASI, WASM_TARGET_WASIP1,
};
pub use package::{verify_package, PluginApiError};
pub use packaging::{generate_keypair, pack_plugin, sign_package, PackOptions};
pub use trust_store::{
    default_plugin_root, fingerprint as trust_store_fingerprint, official_release_key,
    official_release_signing_key, TrustEntry, TrustStore,
};
#[cfg(not(target_arch = "wasm32"))]
pub use wasm_runtime::WasmPluginConfig;
