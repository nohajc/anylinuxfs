use std::{
    borrow::Cow,
    cmp,
    net::{IpAddr, Ipv4Addr},
    ptr::null_mut,
};

use anyhow::Context;
use getifaddrs::{InterfaceFilter, InterfaceFlags};
use ipnet::Ipv4Net;
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

    let (ipv4_addrs, ipv6_addrs): (Vec<_>, Vec<_>) = srv_addrs
        .iter()
        .flat_map(|s| s.to_string().parse::<IpAddr>().ok().into_iter())
        .partition(|ip| ip.is_ipv4());

    ipv4_addrs
        .into_iter()
        .chain(ipv6_addrs)
        .map(|ip| ip.to_string())
        .next()
        .context("no DNS server addresses found")
}

pub fn get_dns_server_with_fallback<'a>() -> Cow<'a, str> {
    get_configured_dns_server()
        .map(Cow::from)
        .unwrap_or_else(|_| DEFAULT_DNS_SERVER.into())
}

pub fn get_interface_networks() -> anyhow::Result<Vec<Ipv4Net>> {
    let mut networks = Vec::new();
    for iface in InterfaceFilter::new().v4().get()? {
        if iface.flags.contains(InterfaceFlags::LOOPBACK) {
            continue;
        }
        if let Some(ip) = iface.address.ip_addr() {
            let IpAddr::V4(ip) = ip else {
                return Err(anyhow::anyhow!("unexpected non-IPv4 address: {}", ip));
            };

            let netmask = iface.address.netmask().unwrap();
            let IpAddr::V4(netmask) = netmask else {
                return Err(anyhow::anyhow!("unexpected non-IPv4 netmask: {}", netmask));
            };

            let net = Ipv4Net::with_netmask(ip, netmask)?.trunc();
            networks.push(net);
        }
    }

    Ok(networks)
}

pub fn pick_available_network(
    prefix_len: u8,
    used_networks: &[Ipv4Net],
) -> anyhow::Result<Ipv4Net> {
    if prefix_len <= 12 {
        return Err(anyhow::anyhow!(
            "invalid prefix length: {}, must be greater than 12",
            prefix_len
        ));
    }
    let candidate_base = Ipv4Net::new(Ipv4Addr::new(172, 27, 1, 0), prefix_len)?;
    let mut search_prefix_len = prefix_len - 1;
    let mut candidate = candidate_base;

    loop {
        let mut conflicting = Vec::new();
        for net in used_networks {
            if candidate.contains(net) || net.contains(&candidate) {
                conflicting.push(*net);
            }
        }
        if conflicting.is_empty() {
            break;
        }

        conflicting.push(candidate);
        let aggregated = Ipv4Net::aggregate(&conflicting)[0];

        search_prefix_len = cmp::min(search_prefix_len, aggregated.prefix_len() - 1);
        let mut supernet = Ipv4Net::new(aggregated.network(), search_prefix_len)?;
        // println!("current supernet: {}", supernet);
        loop {
            let siblings = supernet.subnets(aggregated.prefix_len()).unwrap();
            if let Some(next_candidate) = siblings
                .skip_while(|it| it <= &candidate)
                .next()
                .or(siblings.take_while(|it| it < &candidate).next())
            {
                candidate = Ipv4Net::new(next_candidate.network(), prefix_len)?;
                // println!("next candidate: {}", candidate);
                if candidate == candidate_base {
                    if supernet.prefix_len() > 12 {
                        // broaden the search space
                        supernet = supernet.supernet().unwrap();
                        search_prefix_len = supernet.prefix_len();
                        // println!("broadened search space to: {}", supernet);
                    } else {
                        return Err(anyhow::anyhow!("exhausted candidate IP ranges for VMs"));
                    }
                } else {
                    break;
                }
            } else {
                return Err(anyhow::anyhow!("failed to autoconfigure IP range for VMs"));
            }
        }
    }

    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aggregation() {
        let nets = vec![
            "172.27.1.0/26".parse::<Ipv4Net>().unwrap(),
            "172.27.1.64/26".parse().unwrap(),
            "172.27.1.128/26".parse().unwrap(),
            "172.27.1.0/24".parse().unwrap(),
        ];

        assert_eq!(
            Ipv4Net::aggregate(&nets),
            vec!["172.27.1.0/24".parse::<Ipv4Net>().unwrap(),]
        );
    }

    #[test]
    fn test_pick_available_network_no_conflicts() {
        let result = pick_available_network(24, &[]).unwrap();
        assert_eq!(result, "172.27.1.0/24".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn test_pick_available_network_avoids_exact_conflict() {
        let used = vec!["172.27.1.0/24".parse::<Ipv4Net>().unwrap()];
        let result = pick_available_network(24, &used).unwrap();
        assert_eq!(result, "172.27.0.0/24".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn test_pick_available_network_avoids_multiple_conflicts() {
        let used = vec![
            "172.27.0.0/24".parse::<Ipv4Net>().unwrap(),
            "172.27.1.0/24".parse().unwrap(),
        ];
        let result = pick_available_network(24, &used).unwrap();
        assert_eq!(result, "172.27.2.0/24".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn test_pick_available_network_avoids_multiple_conflicts_extended() {
        let used = vec![
            "172.27.0.0/24".parse::<Ipv4Net>().unwrap(),
            "172.27.1.0/24".parse().unwrap(),
            "172.27.2.0/24".parse().unwrap(),
            "172.27.3.0/24".parse().unwrap(),
        ];
        let result = pick_available_network(24, &used).unwrap();
        assert_eq!(result, "172.27.4.0/24".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn test_pick_available_network_avoids_supernet_conflict() {
        // A broader network that covers the default candidate
        let used = vec![
            "172.27.0.0/16".parse::<Ipv4Net>().unwrap(),
            "172.26.0.0/24".parse().unwrap(),
        ];
        let result = pick_available_network(24, &used).unwrap();
        assert_eq!(result, "172.26.1.0/24".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn test_pick_available_network_avoids_subnet_conflict() {
        // A smaller subnet within the default candidate range
        let used = vec!["172.27.1.128/26".parse::<Ipv4Net>().unwrap()];
        let result = pick_available_network(24, &used).unwrap();
        assert_eq!(result, "172.27.0.0/24".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn test_pick_available_network_different_prefix_len() {
        let result = pick_available_network(26, &[]).unwrap();
        assert_eq!(result, "172.27.1.0/26".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn test_pick_available_network_short_prefix_len() {
        let used = vec!["172.27.1.128/26".parse::<Ipv4Net>().unwrap()];
        let result = pick_available_network(16, &used).unwrap();
        assert_eq!(result, "172.26.0.0/16".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn test_pick_available_network_long_prefix_len() {
        let used = vec![
            "172.27.1.0/30".parse::<Ipv4Net>().unwrap(),
            "172.27.1.4/30".parse().unwrap(),
            "172.27.1.8/30".parse().unwrap(),
            "172.27.1.12/30".parse().unwrap(),
            "172.27.1.16/30".parse().unwrap(),
        ];
        let result = pick_available_network(30, &used).unwrap();
        assert_eq!(result, "172.27.1.20/30".parse::<Ipv4Net>().unwrap());
    }
}
