//! Compiled-in plugins.
//!
//! Each wave C plugin agent adds exactly one line to the list below (and its crate as a
//! dependency through an api-request), for example:
//!
//! ```text
//! Box::new(insightful::InsightfulPlugin::new()),
//! ```

use ink_plugin::Plugin;

/// The first-party plugins built into the binary.
pub fn compiled_in() -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(kosync::KoSyncPlugin::new()),
        Box::new(opds::OpdsPlugin::new()),
        Box::new(calibre::CalibrePlugin::new()),
        // One line per plugin (wave C): insightful, dictionary, stats, vocab, opds,
        // kosync, calibre, wikipedia.
        Box::new(dictionary::DictionaryPlugin::new()),
        Box::new(ink_stats::StatsPlugin::new()),
        Box::new(insightful::InsightfulPlugin::new()),
        Box::new(wikipedia::WikipediaPlugin::new()),
        // Device access: USB Ethernet and the bundled SSH server (control tiles).
        Box::new(usbnet::UsbNetPlugin::new()),
        Box::new(ssh::SshPlugin::new()),
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
