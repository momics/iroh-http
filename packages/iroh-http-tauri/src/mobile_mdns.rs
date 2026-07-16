//! Mobile mDNS bridge for tauri-plugin-iroh-http.
//!
//! On iOS and Android, raw UDP multicast (required by the Rust mdns-sd crate)
//! is restricted by the OS. This module bridges to the platform's native mDNS
//! APIs (NWBrowser/NetService on iOS, NsdManager on Android) via Tauri's mobile
//! plugin system, providing the same browse/advertise API surface as the desktop
//! implementation.

use serde::{Deserialize, Serialize};
use tauri::{
    plugin::{PluginApi, PluginHandle},
    AppHandle, Runtime,
};

pub use crate::mobile_discovery_transport::{
    DnsSdBrowsePollResponse, MobileServiceRecord, MobileSessionStatus,
};
use crate::mobile_discovery_transport::{NativeAdvertisementApi, NativeBrowseApi, NativeFuture};
use iroh_http_discovery::engine::AdvertisementUpdate;

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

// A derived implementation would add the unnecessary `R: Clone` bound even
// though `PluginHandle<R>` itself is cloneable for every Tauri runtime.
impl<R: Runtime> Clone for MobileMdns<R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

// ── Outgoing payloads ────────────────────────────────────────────────────────

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

#[derive(Deserialize)]
struct DnsServersResponse {
    servers: Vec<String>,
}

#[derive(Deserialize)]
struct InterfaceAddressesResponse {
    addresses: Vec<String>,
}

// ── Methods ──────────────────────────────────────────────────────────────────

impl<R: Runtime> MobileMdns<R> {
    /// Query the platform's active-network DNS nameservers (IP strings).
    ///
    /// iroh's default resolver can't read the system DNS config on Android, so
    /// the native layer reads it (via `ConnectivityManager`/`LinkProperties`)
    /// and returns the servers to configure iroh's resolver explicitly.
    pub async fn get_dns_servers(&self) -> Result<Vec<String>, String> {
        let resp: DnsServersResponse = self
            .0
            .run_mobile_plugin_async("get_dns_servers", ())
            .await
            .map_err(|e| e.to_string())?;
        Ok(resp.servers)
    }

    /// Query operational interface IPs from the native mobile layer.
    ///
    /// Android implements this with API-21-safe `ConnectivityManager`,
    /// `LinkProperties`, and `NetworkInterface` calls. It cannot use Rust's
    /// `if-addrs` because Android did not expose `getifaddrs` until API 24.
    pub async fn get_interface_addresses(&self) -> Result<Vec<String>, String> {
        let resp: InterfaceAddressesResponse = self
            .0
            .run_mobile_plugin_async("get_interface_addresses", ())
            .await
            .map_err(|e| e.to_string())?;
        Ok(resp.addresses)
    }
}

// ---------------------------------------------------------------------------
// Generic DNS-SD — advertise/browse arbitrary services, including iroh peers.
//
// Mirrors the desktop `iroh_http_discovery::{advertise, browse}` surface over
// the same native NsdManager / NWBrowser bridge. Records carry the full DNS-SD
// payload; Rust projects peer-shaped records into iroh endpoint discovery.
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
struct DnsSdAdvertiseUpdatePayload<'a> {
    advertise_id: u64,
    port: u16,
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

impl<R: Runtime> MobileMdns<R> {
    /// Advertise a generic DNS-SD service. Returns an `advertise_id` handle.
    #[allow(clippy::too_many_arguments)]
    pub async fn advertise_start(
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
            .run_mobile_plugin_async(
                "advertise_start",
                DnsSdAdvertiseStartPayload {
                    service_name,
                    instance_name,
                    port,
                    protocol,
                    addrs,
                    txt,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(resp.advertise_id)
    }

    /// Update the mutable port/TXT snapshot without changing service identity.
    pub async fn advertise_update(
        &self,
        advertise_id: u64,
        port: u16,
        addrs: &[String],
        txt: &std::collections::HashMap<String, String>,
    ) -> Result<(), String> {
        self.0
            .run_mobile_plugin_async::<()>(
                "advertise_update",
                DnsSdAdvertiseUpdatePayload {
                    advertise_id,
                    port,
                    addrs,
                    txt,
                },
            )
            .await
            .map_err(|error| error.to_string())
    }

    /// Stop a generic DNS-SD advertisement.
    pub async fn advertise_stop(&self, advertise_id: u64) -> Result<(), String> {
        self.0
            .run_mobile_plugin_async::<()>("advertise_stop", AdvertiseStopPayload { advertise_id })
            .await
            .map_err(|e| e.to_string())
    }

    /// Start a generic DNS-SD browse session. Returns a `browse_id` handle.
    pub async fn browse_start(&self, service_name: &str, protocol: &str) -> Result<u64, String> {
        let resp: BrowseStartResponse = self
            .0
            .run_mobile_plugin_async(
                "browse_start",
                DnsSdBrowseStartPayload {
                    service_name,
                    protocol,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(resp.browse_id)
    }

    /// Drain buffered records and observe the native session's terminal state.
    pub async fn browse_poll(&self, browse_id: u64) -> Result<DnsSdBrowsePollResponse, String> {
        let resp: DnsSdBrowsePollResponse = self
            .0
            .run_mobile_plugin_async("browse_poll", BrowsePollPayload { browse_id })
            .await
            .map_err(|e| e.to_string())?;
        Ok(resp)
    }

    /// Stop a generic DNS-SD browse session.
    pub async fn browse_stop(&self, browse_id: u64) -> Result<(), String> {
        self.0
            .run_mobile_plugin_async::<()>("browse_stop", BrowseStopPayload { browse_id })
            .await
            .map_err(|e| e.to_string())
    }
}

impl<R: Runtime> NativeBrowseApi for MobileMdns<R> {
    fn poll(&self, browse_id: u64) -> NativeFuture<Result<DnsSdBrowsePollResponse, String>> {
        let mdns = self.clone();
        Box::pin(async move { mdns.browse_poll(browse_id).await })
    }

    fn stop(&self, browse_id: u64) -> NativeFuture<Result<(), String>> {
        let mdns = self.clone();
        Box::pin(async move { mdns.browse_stop(browse_id).await })
    }
}

impl<R: Runtime> NativeAdvertisementApi for MobileMdns<R> {
    fn update(
        &self,
        advertise_id: u64,
        update: AdvertisementUpdate,
    ) -> NativeFuture<Result<(), String>> {
        let mdns = self.clone();
        Box::pin(async move {
            if !update.addrs.is_empty() {
                return Err(
                    "native mobile DNS-SD advertisements do not support explicit addresses"
                        .to_string(),
                );
            }
            let txt = update.txt.into_iter().collect();
            mdns.advertise_update(advertise_id, update.port, &[], &txt)
                .await
        })
    }

    fn stop(&self, advertise_id: u64) -> NativeFuture<Result<(), String>> {
        let mdns = self.clone();
        Box::pin(async move { mdns.advertise_stop(advertise_id).await })
    }
}
