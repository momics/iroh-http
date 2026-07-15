use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, Notify};

use iroh_http_discovery::engine::{
    AdvertisementHandle, AdvertisementUpdate, BoxFuture, BrowseHandle, RawEvent, ServiceRecord,
    TransportError,
};

pub(crate) type NativeFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MobileSessionStatus {
    Active,
    Closed,
    Failed,
}

/// One generic DNS-SD record crossing the native mobile bridge.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileServiceRecord {
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
    pub txt: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct DnsSdBrowsePollResponse {
    pub status: MobileSessionStatus,
    #[serde(default)]
    pub records: Vec<MobileServiceRecord>,
    #[serde(default)]
    pub error: Option<String>,
}

pub(crate) fn raw_event_from_mobile(
    record: MobileServiceRecord,
    service_type: &str,
) -> Result<RawEvent, TransportError> {
    if !equivalent_service_type(&record.service_type, service_type) {
        return Err(TransportError::new(format!(
            "native DNS-SD returned service type {:?} for {service_type:?}",
            record.service_type
        )));
    }
    if !record.is_active {
        return Ok(RawEvent::Remove {
            service_type: service_type.to_string(),
            instance_name: record.instance_name,
        });
    }
    let addrs = record
        .addrs
        .into_iter()
        .map(|address| {
            address.parse().map_err(|_| {
                TransportError::new(format!(
                    "native DNS-SD returned an invalid socket address: {address}"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut txt: Vec<_> = record.txt.into_iter().collect();
    txt.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(RawEvent::Upsert(ServiceRecord {
        is_active: true,
        service_type: service_type.to_string(),
        instance_name: record.instance_name,
        host: record.host,
        port: record.port,
        addrs,
        txt,
    }))
}

fn equivalent_service_type(native: &str, configured: &str) -> bool {
    fn short(value: &str) -> &str {
        value
            .trim_end_matches('.')
            .strip_suffix(".local")
            .unwrap_or_else(|| value.trim_end_matches('.'))
    }

    short(native).eq_ignore_ascii_case(short(configured))
}

pub(crate) trait NativeBrowseApi: Clone + Send + Sync + 'static {
    fn poll(&self, browse_id: u64) -> NativeFuture<Result<DnsSdBrowsePollResponse, String>>;
    fn stop(&self, browse_id: u64) -> NativeFuture<Result<(), String>>;
}

pub(crate) trait NativeAdvertisementApi: Clone + Send + Sync + 'static {
    fn update(
        &self,
        advertise_id: u64,
        update: AdvertisementUpdate,
    ) -> NativeFuture<Result<(), String>>;
    fn stop(&self, advertise_id: u64) -> NativeFuture<Result<(), String>>;
}

enum BrowseCommand {
    Next(oneshot::Sender<Result<Option<RawEvent>, TransportError>>),
    Close,
}

struct BrowseCommands {
    closing: bool,
    sender: mpsc::UnboundedSender<BrowseCommand>,
}

struct CloseOutcome {
    result: Mutex<Option<Result<(), TransportError>>>,
    ready: Notify,
}

impl CloseOutcome {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            ready: Notify::new(),
        }
    }

    fn complete(&self, result: Result<(), TransportError>) {
        let mut current = self
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.is_none() {
            *current = Some(result);
            self.ready.notify_waiters();
        }
    }

    async fn wait(&self) -> Result<(), TransportError> {
        loop {
            let notified = self.ready.notified();
            if let Some(result) = self
                .result
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
            {
                return result;
            }
            notified.await;
        }
    }
}

pub(crate) struct NativeBrowseHandle {
    commands: Mutex<BrowseCommands>,
    closed: Arc<CloseOutcome>,
}

enum AdvertisementCommand {
    Update(
        AdvertisementUpdate,
        oneshot::Sender<Result<(), TransportError>>,
    ),
    Close,
}

struct AdvertisementCommands {
    closing: bool,
    sender: mpsc::UnboundedSender<AdvertisementCommand>,
}

pub(crate) struct NativeAdvertisementHandle {
    commands: Mutex<AdvertisementCommands>,
    closed: Arc<CloseOutcome>,
}

impl NativeAdvertisementHandle {
    pub(crate) fn new(api: impl NativeAdvertisementApi, advertise_id: u64) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let closed = Arc::new(CloseOutcome::new());
        tokio::spawn(run_advertisement_actor(
            api,
            advertise_id,
            receiver,
            Arc::clone(&closed),
        ));
        Self {
            commands: Mutex::new(AdvertisementCommands {
                closing: false,
                sender,
            }),
            closed,
        }
    }
}

impl AdvertisementHandle for NativeAdvertisementHandle {
    fn update(&self, update: AdvertisementUpdate) -> BoxFuture<'_, Result<(), TransportError>> {
        let (reply, response) = oneshot::channel();
        let commands = self
            .commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if commands.closing
            || commands
                .sender
                .send(AdvertisementCommand::Update(update, reply))
                .is_err()
        {
            return Box::pin(async { Err(TransportError::new("advertisement is closed")) });
        }
        Box::pin(async move {
            response
                .await
                .unwrap_or_else(|_| Err(TransportError::new("advertisement is closed")))
        })
    }

    fn request_close(&self) {
        let mut commands = self
            .commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !commands.closing {
            commands.closing = true;
            if commands.sender.send(AdvertisementCommand::Close).is_err() {
                self.closed.complete(Ok(()));
            }
        }
    }

    fn closed(&self) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(self.closed.wait())
    }
}

impl NativeBrowseHandle {
    pub(crate) fn new(api: impl NativeBrowseApi, browse_id: u64, service_type: String) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let closed = Arc::new(CloseOutcome::new());
        tokio::spawn(run_browse_actor(
            api,
            browse_id,
            service_type,
            receiver,
            Arc::clone(&closed),
        ));
        Self {
            commands: Mutex::new(BrowseCommands {
                closing: false,
                sender,
            }),
            closed,
        }
    }
}

impl BrowseHandle for NativeBrowseHandle {
    fn next(&self) -> BoxFuture<'_, Result<Option<RawEvent>, TransportError>> {
        let (reply, response) = oneshot::channel();
        let commands = self
            .commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if commands.closing || commands.sender.send(BrowseCommand::Next(reply)).is_err() {
            return Box::pin(async { Ok(None) });
        }
        Box::pin(async move { response.await.unwrap_or(Ok(None)) })
    }

    fn request_close(&self) {
        let mut commands = self
            .commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !commands.closing {
            commands.closing = true;
            if commands.sender.send(BrowseCommand::Close).is_err() {
                self.closed.complete(Ok(()));
            }
        }
    }

    fn closed(&self) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(self.closed.wait())
    }
}

async fn run_browse_actor(
    api: impl NativeBrowseApi,
    browse_id: u64,
    service_type: String,
    mut commands: mpsc::UnboundedReceiver<BrowseCommand>,
    closed: Arc<CloseOutcome>,
) {
    let mut buffered = VecDeque::new();
    while let Some(command) = commands.recv().await {
        match command {
            BrowseCommand::Next(reply) => {
                if let Some(event) = buffered.pop_front() {
                    let _ = reply.send(Ok(Some(event)));
                    continue;
                }
                loop {
                    let batch = match api.poll(browse_id).await {
                        Ok(batch) => batch,
                        Err(error) => {
                            let error = TransportError::new(error);
                            close_native_browse(&api, browse_id, &closed).await;
                            let _ = reply.send(Err(error));
                            return;
                        }
                    };
                    match batch.status {
                        MobileSessionStatus::Active => {
                            for record in batch.records {
                                match raw_event_from_mobile(record, &service_type) {
                                    Ok(event) => buffered.push_back(event),
                                    Err(error) => {
                                        close_native_browse(&api, browse_id, &closed).await;
                                        let _ = reply.send(Err(error));
                                        return;
                                    }
                                }
                            }
                            if let Some(event) = buffered.pop_front() {
                                let _ = reply.send(Ok(Some(event)));
                                break;
                            }
                            if let Ok(Some(command)) = tokio::time::timeout(
                                std::time::Duration::from_millis(250),
                                commands.recv(),
                            )
                            .await
                            {
                                match command {
                                    BrowseCommand::Close => {
                                        let _ = reply.send(Ok(None));
                                        close_native_browse(&api, browse_id, &closed).await;
                                        return;
                                    }
                                    BrowseCommand::Next(concurrent) => {
                                        let _ = concurrent.send(Err(TransportError::new(
                                            "native browse next calls must be serialized",
                                        )));
                                    }
                                }
                            }
                        }
                        MobileSessionStatus::Closed => {
                            let _ = reply.send(Ok(None));
                            closed.complete(Ok(()));
                            return;
                        }
                        MobileSessionStatus::Failed => {
                            let error = TransportError::new(
                                batch
                                    .error
                                    .unwrap_or_else(|| "native browse failed".to_string()),
                            );
                            close_native_browse(&api, browse_id, &closed).await;
                            let _ = reply.send(Err(error));
                            return;
                        }
                    }
                }
            }
            BrowseCommand::Close => {
                close_native_browse(&api, browse_id, &closed).await;
                return;
            }
        }
    }
    close_native_browse(&api, browse_id, &closed).await;
}

async fn run_advertisement_actor(
    api: impl NativeAdvertisementApi,
    advertise_id: u64,
    mut commands: mpsc::UnboundedReceiver<AdvertisementCommand>,
    closed: Arc<CloseOutcome>,
) {
    while let Some(command) = commands.recv().await {
        match command {
            AdvertisementCommand::Update(update, reply) => {
                match api.update(advertise_id, update).await {
                    Ok(()) => {
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        let error = TransportError::new(error);
                        close_native_advertisement(&api, advertise_id, &closed).await;
                        let _ = reply.send(Err(error));
                        return;
                    }
                }
            }
            AdvertisementCommand::Close => {
                close_native_advertisement(&api, advertise_id, &closed).await;
                return;
            }
        }
    }
    close_native_advertisement(&api, advertise_id, &closed).await;
}

async fn close_native_browse(api: &impl NativeBrowseApi, browse_id: u64, closed: &CloseOutcome) {
    closed.complete(api.stop(browse_id).await.map_err(TransportError::new));
}

async fn close_native_advertisement(
    api: &impl NativeAdvertisementApi,
    advertise_id: u64,
    closed: &CloseOutcome,
) {
    closed.complete(api.stop(advertise_id).await.map_err(TransportError::new));
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use iroh_http_discovery::engine::RawEvent;
    use tokio::sync::{Notify, Semaphore};

    use super::*;

    fn record(is_active: bool) -> MobileServiceRecord {
        MobileServiceRecord {
            is_active,
            service_type: "_demo._udp.local.".to_string(),
            instance_name: "printer".to_string(),
            host: Some("printer.local.".to_string()),
            port: 9100,
            addrs: vec![
                "192.168.1.20:9100".to_string(),
                "[fd00::20]:9100".to_string(),
            ],
            txt: HashMap::from([("note".to_string(), "office".to_string())]),
        }
    }

    #[test]
    fn active_native_record_becomes_a_canonical_upsert() {
        let RawEvent::Upsert(record) =
            raw_event_from_mobile(record(true), "_demo._udp.local.").unwrap()
        else {
            panic!("expected an upsert");
        };

        assert_eq!(record.service_type, "_demo._udp.local.");
        assert_eq!(record.instance_name, "printer");
        assert_eq!(record.port, 9100);
        assert_eq!(record.addrs.len(), 2);
        assert_eq!(record.txt, vec![("note".to_string(), "office".to_string())]);
    }

    #[test]
    fn inactive_native_record_preserves_only_removal_identity() {
        let RawEvent::Remove {
            service_type,
            instance_name,
        } = raw_event_from_mobile(record(false), "_demo._udp.local.").unwrap()
        else {
            panic!("expected a removal");
        };

        assert_eq!(service_type, "_demo._udp.local.");
        assert_eq!(instance_name, "printer");
    }

    #[test]
    fn malformed_active_native_address_is_rejected_at_the_adapter_seam() {
        let mut record = record(true);
        record.addrs.push("not-a-socket".to_string());

        assert!(raw_event_from_mobile(record, "_demo._udp.local.").is_err());
    }

    #[test]
    fn native_service_type_must_match_the_started_browse() {
        let mut expected = record(true);
        expected.service_type = "_DEMO._UDP".to_string();
        assert!(raw_event_from_mobile(expected, "_demo._udp.local.").is_ok());

        let mut stale = record(true);
        stale.service_type = "_other._udp".to_string();
        assert!(raw_event_from_mobile(stale, "_demo._udp.local.").is_err());
    }

    #[derive(Clone)]
    struct FakeNativeBrowse {
        batches: Arc<Mutex<VecDeque<Result<DnsSdBrowsePollResponse, String>>>>,
        polls: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
    }

    impl FakeNativeBrowse {
        fn new(batches: Vec<DnsSdBrowsePollResponse>) -> Self {
            Self {
                batches: Arc::new(Mutex::new(batches.into_iter().map(Ok).collect())),
                polls: Arc::new(AtomicUsize::new(0)),
                stops: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl NativeBrowseApi for FakeNativeBrowse {
        fn poll(&self, _browse_id: u64) -> NativeFuture<Result<DnsSdBrowsePollResponse, String>> {
            let batches = Arc::clone(&self.batches);
            let polls = Arc::clone(&self.polls);
            Box::pin(async move {
                polls.fetch_add(1, Ordering::AcqRel);
                batches
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pop_front()
                    .unwrap_or(Ok(DnsSdBrowsePollResponse {
                        status: MobileSessionStatus::Closed,
                        records: Vec::new(),
                        error: None,
                    }))
            })
        }

        fn stop(&self, _browse_id: u64) -> NativeFuture<Result<(), String>> {
            let stops = Arc::clone(&self.stops);
            Box::pin(async move {
                stops.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn native_browse_actor_hides_empty_polls_and_canonicalizes_service_identity() {
        let mut announced = record(true);
        announced.service_type = "_DEMO._UDP".to_string();
        let native = FakeNativeBrowse::new(vec![
            DnsSdBrowsePollResponse {
                status: MobileSessionStatus::Active,
                records: Vec::new(),
                error: None,
            },
            DnsSdBrowsePollResponse {
                status: MobileSessionStatus::Active,
                records: vec![announced],
                error: None,
            },
        ]);
        let session = iroh_http_discovery::engine::BrowseSession::new(NativeBrowseHandle::new(
            native.clone(),
            7,
            "_demo._udp.local.".to_string(),
        ));

        let RawEvent::Upsert(record) = session.next().await.unwrap().unwrap() else {
            panic!("expected an upsert");
        };
        assert_eq!(record.service_type, "_demo._udp.local.");
        assert_eq!(native.polls.load(Ordering::Acquire), 2);

        session.close();
        session.wait_closed().await.unwrap();
        assert_eq!(native.stops.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn native_browse_actor_discards_records_from_a_terminal_batch() {
        let native = FakeNativeBrowse::new(vec![DnsSdBrowsePollResponse {
            status: MobileSessionStatus::Closed,
            records: vec![record(true)],
            error: None,
        }]);
        let session = iroh_http_discovery::engine::BrowseSession::new(NativeBrowseHandle::new(
            native,
            8,
            "_demo._udp.local.".to_string(),
        ));

        assert!(session.next().await.unwrap().is_none());
        assert!(session.next().await.unwrap().is_none());
        session.wait_closed().await.unwrap();
    }

    #[tokio::test]
    async fn canonical_mobile_stream_preserves_periodic_announcements_and_changes() {
        let unchanged = record(true);
        let mut changed = record(true);
        changed.txt.insert("note".to_string(), "lab".to_string());
        let native = FakeNativeBrowse::new(vec![
            DnsSdBrowsePollResponse {
                status: MobileSessionStatus::Active,
                records: vec![unchanged, record(true), changed],
                error: None,
            },
            DnsSdBrowsePollResponse {
                status: MobileSessionStatus::Closed,
                records: Vec::new(),
                error: None,
            },
        ]);
        let session = iroh_http_discovery::engine::BrowseSession::new(NativeBrowseHandle::new(
            native,
            11,
            "_demo._udp.local.".to_string(),
        ));

        let RawEvent::Upsert(first) = session.next().await.unwrap().unwrap() else {
            panic!("expected initial upsert");
        };
        let RawEvent::Upsert(second) = session.next().await.unwrap().unwrap() else {
            panic!("expected repeated upsert");
        };
        let RawEvent::Upsert(third) = session.next().await.unwrap().unwrap() else {
            panic!("expected changed upsert");
        };
        assert_eq!(first.txt, vec![("note".to_string(), "office".to_string())]);
        assert_eq!(second.txt, first.txt);
        assert_eq!(third.txt, vec![("note".to_string(), "lab".to_string())]);
        assert!(session.next().await.unwrap().is_none());
    }

    #[derive(Clone)]
    struct BlockingNativeBrowse {
        entered: Arc<Notify>,
        release: Arc<Semaphore>,
        stops: Arc<AtomicUsize>,
    }

    impl NativeBrowseApi for BlockingNativeBrowse {
        fn poll(&self, _browse_id: u64) -> NativeFuture<Result<DnsSdBrowsePollResponse, String>> {
            let entered = Arc::clone(&self.entered);
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                entered.notify_waiters();
                release.acquire().await.unwrap().forget();
                Ok(DnsSdBrowsePollResponse {
                    status: MobileSessionStatus::Active,
                    records: vec![record(true)],
                    error: None,
                })
            })
        }

        fn stop(&self, _browse_id: u64) -> NativeFuture<Result<(), String>> {
            let stops = Arc::clone(&self.stops);
            Box::pin(async move {
                stops.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn close_waits_for_an_in_flight_native_poll_after_caller_cancellation() {
        let native = BlockingNativeBrowse {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Semaphore::new(0)),
            stops: Arc::new(AtomicUsize::new(0)),
        };
        let session = Arc::new(iroh_http_discovery::engine::BrowseSession::new(
            NativeBrowseHandle::new(native.clone(), 9, "_demo._udp.local.".to_string()),
        ));
        let entered = native.entered.notified();
        let next = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.next().await }
        });
        entered.await;

        next.abort();
        assert!(matches!(next.await, Err(error) if error.is_cancelled()));
        session.close();
        assert_eq!(native.stops.load(Ordering::Acquire), 0);
        native.release.add_permits(1);
        session.wait_closed().await.unwrap();
        assert_eq!(native.stops.load(Ordering::Acquire), 1);
    }

    #[derive(Clone)]
    struct FailingPollWithBlockingStop {
        stop_entered: Arc<Notify>,
        stop_release: Arc<Semaphore>,
    }

    impl NativeBrowseApi for FailingPollWithBlockingStop {
        fn poll(&self, _browse_id: u64) -> NativeFuture<Result<DnsSdBrowsePollResponse, String>> {
            Box::pin(async { Err("native poll failed".to_string()) })
        }

        fn stop(&self, _browse_id: u64) -> NativeFuture<Result<(), String>> {
            let entered = Arc::clone(&self.stop_entered);
            let release = Arc::clone(&self.stop_release);
            Box::pin(async move {
                entered.notify_waiters();
                release.acquire().await.unwrap().forget();
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn terminal_error_is_not_exposed_until_native_cleanup_finishes() {
        let native = FailingPollWithBlockingStop {
            stop_entered: Arc::new(Notify::new()),
            stop_release: Arc::new(Semaphore::new(0)),
        };
        let session = Arc::new(iroh_http_discovery::engine::BrowseSession::new(
            NativeBrowseHandle::new(native.clone(), 10, "_demo._udp.local.".to_string()),
        ));
        let stop_entered = native.stop_entered.notified();
        let mut next = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.next().await }
        });
        stop_entered.await;

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut next)
                .await
                .is_err()
        );
        native.stop_release.add_permits(1);
        assert!(
            matches!(next.await.unwrap(), Err(error) if error.to_string() == "native poll failed")
        );
    }

    #[derive(Clone)]
    struct FakeNativeAdvertisement {
        updates: Arc<Mutex<Vec<AdvertisementUpdate>>>,
        update_entered: Arc<Notify>,
        update_release: Arc<Semaphore>,
        block_update: Arc<AtomicBool>,
        fail_update: Arc<AtomicBool>,
        stop_entered: Arc<Notify>,
        stop_release: Arc<Semaphore>,
        block_stop: Arc<AtomicBool>,
        fail_stop: Arc<AtomicBool>,
        stops: Arc<AtomicUsize>,
    }

    impl FakeNativeAdvertisement {
        fn new() -> Self {
            Self {
                updates: Arc::new(Mutex::new(Vec::new())),
                update_entered: Arc::new(Notify::new()),
                update_release: Arc::new(Semaphore::new(0)),
                block_update: Arc::new(AtomicBool::new(false)),
                fail_update: Arc::new(AtomicBool::new(false)),
                stop_entered: Arc::new(Notify::new()),
                stop_release: Arc::new(Semaphore::new(0)),
                block_stop: Arc::new(AtomicBool::new(false)),
                fail_stop: Arc::new(AtomicBool::new(false)),
                stops: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl NativeAdvertisementApi for FakeNativeAdvertisement {
        fn update(
            &self,
            _advertise_id: u64,
            update: AdvertisementUpdate,
        ) -> NativeFuture<Result<(), String>> {
            let updates = Arc::clone(&self.updates);
            let entered = Arc::clone(&self.update_entered);
            let release = Arc::clone(&self.update_release);
            let block = Arc::clone(&self.block_update);
            let fail = Arc::clone(&self.fail_update);
            Box::pin(async move {
                updates
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(update);
                entered.notify_waiters();
                if block.load(Ordering::Acquire) {
                    release.acquire().await.unwrap().forget();
                }
                if fail.load(Ordering::Acquire) {
                    Err("native update failed".to_string())
                } else {
                    Ok(())
                }
            })
        }

        fn stop(&self, _advertise_id: u64) -> NativeFuture<Result<(), String>> {
            let stops = Arc::clone(&self.stops);
            let entered = Arc::clone(&self.stop_entered);
            let release = Arc::clone(&self.stop_release);
            let block = Arc::clone(&self.block_stop);
            let fail = Arc::clone(&self.fail_stop);
            Box::pin(async move {
                stops.fetch_add(1, Ordering::AcqRel);
                entered.notify_waiters();
                if block.load(Ordering::Acquire) {
                    release.acquire().await.unwrap().forget();
                }
                if fail.load(Ordering::Acquire) {
                    Err("native stop failed".to_string())
                } else {
                    Ok(())
                }
            })
        }
    }

    fn advertisement_update(port: u16, revision: &str) -> AdvertisementUpdate {
        AdvertisementUpdate {
            port,
            addrs: Vec::new(),
            txt: vec![("revision".to_string(), revision.to_string())],
        }
    }

    #[tokio::test]
    async fn advertisement_close_waits_for_a_cancelled_native_update() {
        let native = FakeNativeAdvertisement::new();
        native.block_update.store(true, Ordering::Release);
        let session = Arc::new(
            iroh_http_discovery::engine::AdvertisementSession::with_initial(
                NativeAdvertisementHandle::new(native.clone(), 20),
                advertisement_update(8080, "one"),
            ),
        );
        let update_entered = native.update_entered.notified();
        let update = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.update(advertisement_update(8081, "two")).await }
        });
        update_entered.await;

        update.abort();
        assert!(matches!(update.await, Err(error) if error.is_cancelled()));
        session.close();
        assert_eq!(native.stops.load(Ordering::Acquire), 0);
        native.update_release.add_permits(1);
        session.wait_closed().await.unwrap();
        assert_eq!(native.stops.load(Ordering::Acquire), 1);
        assert_eq!(
            native
                .updates
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_slice(),
            &[advertisement_update(8081, "two")]
        );
    }

    #[tokio::test]
    async fn advertisement_update_error_waits_for_native_cleanup() {
        let native = FakeNativeAdvertisement::new();
        native.fail_update.store(true, Ordering::Release);
        native.block_stop.store(true, Ordering::Release);
        let session = Arc::new(iroh_http_discovery::engine::AdvertisementSession::new(
            NativeAdvertisementHandle::new(native.clone(), 21),
        ));
        let stop_entered = native.stop_entered.notified();
        let mut update = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.update(advertisement_update(8080, "bad")).await }
        });
        stop_entered.await;

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut update)
                .await
                .is_err()
        );
        native.stop_release.add_permits(1);
        assert!(
            matches!(update.await.unwrap(), Err(error) if error.to_string() == "native update failed")
        );
        session.wait_closed().await.unwrap();
        assert_eq!(native.stops.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn advertisement_keeps_primary_update_and_cached_cleanup_errors_distinct() {
        let native = FakeNativeAdvertisement::new();
        native.fail_update.store(true, Ordering::Release);
        native.fail_stop.store(true, Ordering::Release);
        let session = iroh_http_discovery::engine::AdvertisementSession::new(
            NativeAdvertisementHandle::new(native, 22),
        );

        assert!(matches!(
            session.update(advertisement_update(8080, "bad")).await,
            Err(error) if error.to_string() == "native update failed"
        ));
        assert!(matches!(
            session.wait_closed().await,
            Err(error) if error.to_string() == "native stop failed"
        ));
    }
}
