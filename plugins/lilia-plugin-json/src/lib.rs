use lilia_plugin_api::PluginDescriptorV1;

const NAME: &[u8] = b"lilia-json\0";

#[allow(unsafe_code)] // the plugin entry point must keep its exported symbol name
#[no_mangle]
pub extern "C" fn lilia_plugin_v1() -> PluginDescriptorV1 {
    PluginDescriptorV1::new(NAME.as_ptr().cast(), 2)
}
