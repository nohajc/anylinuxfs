use std::{borrow::Cow, ptr::null_mut};

use anyhow::Context;
use objc2_core_foundation::{CFArray, CFDictionary, CFString};
use objc2_system_configuration::SCDynamicStore;

use crate::utils::cfdict_get_value;

const DEFAULT_DNS_SERVER: &str = "1.1.1.1";

pub fn get_configured_dns_server() -> anyhow::Result<String> {
    let name = CFString::from_str("anylinuxfs");
    let dyn_store = unsafe { SCDynamicStore::new(None, &name, None, null_mut()) }
        .context("failed to retrieve SCDynamicStore")?;

    let global_dns_key = "State:/Network/Global/DNS";
    let dns_settings = SCDynamicStore::value(Some(&dyn_store), &CFString::from_str(global_dns_key))
        .context("failed to retrieve DNS settings")?;

    let dict: &CFDictionary = dns_settings
        .downcast_ref()
        .with_context(|| format!("{} is not a CFDictionary", global_dns_key))?;
    // inspect_cf_dictionary_values(&dict);

    let srv_addrs: &CFArray<CFString> = unsafe { cfdict_get_value(dict, "ServerAddresses") }
        .context("failed to retrieve DNS ServerAddresses")?;

    srv_addrs
        .get(0)
        .map(|s| s.to_string())
        .context("no DNS server addresses found")
        .map_err(|e| e.into())
}

pub fn get_dns_server_with_fallback<'a>() -> Cow<'a, str> {
    get_configured_dns_server()
        .map(Cow::from)
        .unwrap_or_else(|_| DEFAULT_DNS_SERVER.into())
}
