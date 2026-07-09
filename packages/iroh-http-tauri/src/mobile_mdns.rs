//! Mobile mDNS bridge for tauri-plugin-iroh-http.
//!
//! On iOS and Android, raw UDP multicast (required by the Rust mdns-sd crate)
//! is restricted by the OS. This module bridges to the platform's native mDNS
//! APIs (NWBrowser/NWListener on iOS, NsdManager on Android) via Tauri's mobile
//! plugin system, providing the same browse/advertise API surface as the desktop
//! implementation.

use serde::{Deserialize, Serialize};
use tauri::{
    plugin::{PluginApi, PluginHandle},
    AppHandle, Runtime,
};

// ---------------------------------------------------------------------------
// iOS native binding
// ---------------------------------------------------------------------------

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_iroh_http);

/// Register the native iOS/Android plugin and return a `MobileMdns` handle.
pub fn init<R: Runtime, C: serde::de::DeserializeOwned>(
    _app: &AppHandle<R>,
    api: PluginApi<R, C>,
) -> Result<MobileMdns<R>, String> {
    #[cfg(target_os = "android")]
    let handle = api
        .register_android_plugin("com.iroh.http", "IrohHttpPlugin")
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "ios")]
    let handle = api
        .register_ios_plugin(init_plugin_iroh_http)
        .map_err(|e| e.to_string())?;
    Ok(MobileMdns(handle))
}

// ---------------------------------------------------------------------------
// MobileMdns — thin wrapper around PluginHandle
// ---------------------------------------------------------------------------

pub struct MobileMdns<R: Runtime>(PluginHandle<R>);

// ── Outgoing payloads ────────────────────────────────────────────────────────

#[derive(Serialize)]
struct BrowseStartPayload<'a> {
    #[serde(rename = "serviceName")]
    service_name: &'a str,
}

#[derive(Serialize)]
struct BrowsePollPayload {
    #[serde(rename = "browseId")]
    browse_id: u64,
}

#[derive(Serialize)]
struct BrowseStopPayload {
    #[serde(rename = "browseId")]
    browse_id: u64,
}

#[derive(Serialize)]
struct AdvertiseStartPayload<'a> {
    #[serde(rename = "serviceName")]
    service_name: &'a str,
    /// base32-encoded Ed25519 public key — required by browse parsers.
    pk: &'a str,
    /// Relay URL, if any. Optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    relay: Option<&'a str>,
}

#[derive(Serialize)]
struct AdvertiseStopPayload {
    #[serde(rename = "advertiseId")]
    advertise_id: u64,
}

// ── Incoming responses ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct BrowseStartResponse {
    #[serde(rename = "browseId")]
    browse_id: u64,
}

#[derive(Deserialize)]
struct AdvertiseStartResponse {
    #[serde(rename = "advertiseId")]
    advertise_id: u64,
}

/// A single discovery event from the native layer.
#[derive(Deserialize)]
pub struct MobileDiscoveryEvent {
    /// `"discovered"` or `"expired"`
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "nodeId")]
    pub node_id: String,
    pub addrs: Vec<String>,
}

#[derive(Deserialize)]
struct BrowsePollResponse {
    pub events: Vec<MobileDiscoveryEvent>,
}

// ── Methods ──────────────────────────────────────────────────────────────────

impl<R: Runtime> MobileMdns<R> {
    /// Start a browse session on the native layer. Returns a `browse_id` handle.
    pub fn browse_start(&self, service_name: &str) -> Result<u64, String> {
        let resp: BrowseStartResponse = self
            .0
            .run_mobile_plugin("mdns_browse_start", BrowseStartPayload { service_name })
            .map_err(|e| e.to_string())?;
        Ok(resp.browse_id)
    }

    /// Drain all buffered events for a browse session. Non-blocking — returns `[]` if none.
    pub fn browse_poll(&self, browse_id: u64) -> Result<Vec<MobileDiscoveryEvent>, String> {
        let resp: BrowsePollResponse = self
            .0
            .run_mobile_plugin("mdns_browse_poll", BrowsePollPayload { browse_id })
            .map_err(|e| e.to_string())?;
        Ok(resp.events)
    }

    /// Stop a browse session.
    pub fn browse_stop(&self, browse_id: u64) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<()>("mdns_browse_stop", BrowseStopPayload { browse_id })
            .map_err(|e| e.to_string())
    }

    /// Start advertising on the native layer. Returns an `advertise_id` handle.
    pub fn advertise_start(
        &self,
        service_name: &str,
        pk: &str,
        relay: Option<&str>,
    ) -> Result<u64, String> {
        let resp: AdvertiseStartResponse = self
            .0
            .run_mobile_plugin(
                "mdns_advertise_start",
                AdvertiseStartPayload {
                    service_name,
                    pk,
                    relay,
                },
            )
            .map_err(|e| e.to_string())?;
        Ok(resp.advertise_id)
    }

    /// Stop advertising.
    pub fn advertise_stop(&self, advertise_id: u64) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<()>("mdns_advertise_stop", AdvertiseStopPayload { advertise_id })
            .map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Generic DNS-SD — advertise/browse arbitrary services, not just iroh peers.
//
// Mirrors the desktop `iroh_http_discovery::{advertise, browse}` surface over
// the same native NsdManager / NWBrowser bridge. Unlike the peer path, records
// carry the full DNS-SD payload (instance name, host, port, TXT, addresses)
// rather than a reduced `(nodeId, addrs)` tuple.
// ---------------------------------------------------------------------------

// ── Outgoing payloads ────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DnsSdAdvertiseStartPayload<'a> {
    service_name: &'a str,
    instance_name: &'a str,
    port: u16,
    protocol: &'a str,
    addrs: &'a [String],
    txt: &'a std::collections::HashMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DnsSdBrowseStartPayload<'a> {
    service_name: &'a str,
    protocol: &'a str,
}

// ── Incoming responses ───────────────────────────────────────────────────────

/// A single generic DNS-SD record polled from the native layer.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileServiceRecord {
    /// `true` when the service appeared, `false` when it went away.
    pub is_active: bool,
    pub service_type: String,
    pub instance_name: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub addrs: Vec<String>,
    #[serde(default)]
    pub txt: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct DnsSdBrowsePollResponse {
    records: Vec<MobileServiceRecord>,
}

impl<R: Runtime> MobileMdns<R> {
    /// Advertise a generic DNS-SD service. Returns an `advertise_id` handle.
    #[allow(clippy::too_many_arguments)]
    pub fn dns_sd_advertise_start(
        &self,
        service_name: &str,
        instance_name: &str,
        port: u16,
        protocol: &str,
        addrs: &[String],
        txt: &std::collections::HashMap<String, String>,
    ) -> Result<u64, String> {
        let resp: AdvertiseStartResponse = self
            .0
            .run_mobile_plugin(
                "dns_sd_advertise_start",
                DnsSdAdvertiseStartPayload {
                    service_name,
                    instance_name,
                    port,
                    protocol,
                    addrs,
                    txt,
                },
            )
            .map_err(|e| e.to_string())?;
        Ok(resp.advertise_id)
    }

    /// Stop a generic DNS-SD advertisement.
    pub fn dns_sd_advertise_stop(&self, advertise_id: u64) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<()>(
                "dns_sd_advertise_stop",
                AdvertiseStopPayload { advertise_id },
            )
            .map_err(|e| e.to_string())
    }

    /// Start a generic DNS-SD browse session. Returns a `browse_id` handle.
    pub fn dns_sd_browse_start(&self, service_name: &str, protocol: &str) -> Result<u64, String> {
        let resp: BrowseStartResponse = self
            .0
            .run_mobile_plugin(
                "dns_sd_browse_start",
                DnsSdBrowseStartPayload {
                    service_name,
                    protocol,
                },
            )
            .map_err(|e| e.to_string())?;
        Ok(resp.browse_id)
    }

    /// Drain all buffered records for a browse session. Non-blocking.
    pub fn dns_sd_browse_poll(&self, browse_id: u64) -> Result<Vec<MobileServiceRecord>, String> {
        let resp: DnsSdBrowsePollResponse = self
            .0
            .run_mobile_plugin("dns_sd_browse_poll", BrowsePollPayload { browse_id })
            .map_err(|e| e.to_string())?;
        Ok(resp.records)
    }

    /// Stop a generic DNS-SD browse session.
    pub fn dns_sd_browse_stop(&self, browse_id: u64) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<()>("dns_sd_browse_stop", BrowseStopPayload { browse_id })
            .map_err(|e| e.to_string())
    }
}
