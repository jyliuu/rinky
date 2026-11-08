//! Compiled-in plugins.
//!
//! A plugin crate is compiled in by one line in the list below (and its crate as a
//! dependency of `ink-app`), for example:
//!
//! ```text
//! Box::new(insightful::InsightfulPlugin::default()),
//! ```

use ink_plugin::Plugin;

/// The first-party plugins built into the binary.
pub fn compiled_in() -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(kosync::KoSyncPlugin::default()),
        Box::new(opds::OpdsPlugin::default()),
        Box::new(calibre::CalibrePlugin::default()),
        Box::new(dictionary::DictionaryPlugin::default()),
        Box::new(ink_stats::StatsPlugin::default()),
        Box::new(insightful::InsightfulPlugin::default()),
        Box::new(wikipedia::WikipediaPlugin::default()),
        // Device access: USB Ethernet and the bundled SSH server (control tiles).
        Box::new(usbnet::UsbNetPlugin::default()),
        Box::new(ssh::SshPlugin::default()),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn list_loads() {
        let host = crate::plugin_host::PluginHost::new(super::compiled_in());
        assert!(host.manager.errors.is_empty());
    }
}
