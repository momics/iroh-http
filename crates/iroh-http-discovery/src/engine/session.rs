use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex as StdMutex,
};

use tokio::sync::{Mutex, Notify};

use super::{AdvertisementHandle, AdvertisementUpdate, BrowseHandle, RawEvent, TransportError};

struct InFlight<T> {
    result: StdMutex<Option<T>>,
    ready: Notify,
}

impl<T: Clone> InFlight<T> {
    fn new() -> Self {
        Self {
            result: StdMutex::new(None),
            ready: Notify::new(),
        }
    }

    fn complete(&self, result: T) {
        *self
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
        self.ready.notify_one();
    }

    async fn wait(&self) -> T {
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

type BrowseOutcome = Result<Option<RawEvent>, TransportError>;
type UpdateOutcome = Result<(), TransportError>;

fn clear_operation<T>(slot: &StdMutex<Option<Arc<InFlight<T>>>>, completed: &Arc<InFlight<T>>) {
    let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, completed))
    {
        slot.take();
    }
}

/// Serialized, terminal-safe access to an already-started browse handle.
pub struct BrowseSession {
    handle: Arc<dyn BrowseHandle>,
    next_gate: Mutex<()>,
    next_operation: StdMutex<Option<Arc<InFlight<BrowseOutcome>>>>,
    terminal: AtomicBool,
    close_callbacks: StdMutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl BrowseSession {
    pub fn new(handle: impl BrowseHandle) -> Self {
        Self {
            handle: Arc::new(handle),
            next_gate: Mutex::new(()),
            next_operation: StdMutex::new(None),
            terminal: AtomicBool::new(false),
            close_callbacks: StdMutex::new(Vec::new()),
        }
    }

    pub async fn next(&self) -> Result<Option<RawEvent>, TransportError> {
        let _guard = self.next_gate.lock().await;
        if self.terminal.load(Ordering::Acquire) {
            return Ok(None);
        }
        let operation = {
            let mut slot = self
                .next_operation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(operation) = slot.as_ref() {
                Arc::clone(operation)
            } else {
                let operation = Arc::new(InFlight::new());
                *slot = Some(Arc::clone(&operation));
                let task_operation = Arc::clone(&operation);
                let handle = Arc::clone(&self.handle);
                tokio::spawn(async move {
                    task_operation.complete(handle.next().await);
                });
                operation
            }
        };
        let result = operation.wait().await;
        clear_operation(&self.next_operation, &operation);
        if self.terminal.load(Ordering::Acquire) {
            return Ok(None);
        }
        match result {
            Ok(Some(event)) => Ok(Some(event)),
            Ok(None) => {
                self.close();
                Ok(None)
            }
            Err(error) => {
                self.close();
                Err(error)
            }
        }
    }

    pub fn close(&self) {
        if !self.terminal.swap(true, Ordering::AcqRel) {
            self.handle.request_close();
            let callbacks = std::mem::take(
                &mut *self
                    .close_callbacks
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            );
            for callback in callbacks {
                callback();
            }
        }
    }

    pub fn on_close(&self, callback: impl FnOnce() + Send + 'static) {
        let mut callbacks = self
            .close_callbacks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.terminal.load(Ordering::Acquire) {
            drop(callbacks);
            callback();
        } else {
            callbacks.push(Box::new(callback));
        }
    }

    pub async fn wait_closed(&self) -> Result<(), TransportError> {
        self.close();
        let _guard = self.next_gate.lock().await;
        let operation = self
            .next_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(operation) = operation {
            let _ = operation.wait().await;
            clear_operation(&self.next_operation, &operation);
        }
        self.handle.closed().await
    }
}

impl Drop for BrowseSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// Serialized access to an already-ready advertisement handle.
pub struct AdvertisementSession {
    handle: Arc<dyn AdvertisementHandle>,
    update_gate: Mutex<()>,
    update_operation: StdMutex<Option<Arc<InFlight<UpdateOutcome>>>>,
    closed: AtomicBool,
}

impl AdvertisementSession {
    pub fn new(handle: impl AdvertisementHandle) -> Self {
        Self {
            handle: Arc::new(handle),
            update_gate: Mutex::new(()),
            update_operation: StdMutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    pub async fn update(&self, update: AdvertisementUpdate) -> Result<(), TransportError> {
        let _guard = self.update_gate.lock().await;
        let previous = {
            self.update_operation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        };
        if let Some(previous) = previous {
            let result = previous.wait().await;
            clear_operation(&self.update_operation, &previous);
            if let Err(error) = result {
                self.close();
                return Err(error);
            }
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(TransportError::new("advertisement is closed"));
        }
        let operation = Arc::new(InFlight::new());
        *self
            .update_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&operation));
        let task_operation = Arc::clone(&operation);
        let handle = Arc::clone(&self.handle);
        tokio::spawn(async move {
            task_operation.complete(handle.update(update).await);
        });
        let result = operation.wait().await;
        clear_operation(&self.update_operation, &operation);
        if result.is_err() {
            self.close();
        }
        result
    }

    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.handle.request_close();
        }
    }

    pub async fn wait_closed(&self) -> Result<(), TransportError> {
        self.close();
        let _guard = self.update_gate.lock().await;
        let operation = self
            .update_operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(operation) = operation {
            let _ = operation.wait().await;
            clear_operation(&self.update_operation, &operation);
        }
        self.handle.closed().await
    }
}

impl Drop for AdvertisementSession {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        future::pending,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex as StdMutex,
        },
    };

    use tokio::sync::{Notify, Semaphore};

    use super::*;
    use crate::engine::BoxFuture;

    #[derive(Clone)]
    struct FakeBrowse(Arc<FakeBrowseState>);

    struct FakeBrowseState {
        responses: StdMutex<VecDeque<Result<Option<RawEvent>, TransportError>>>,
        entered: Notify,
        release: Semaphore,
        block: AtomicBool,
        active: AtomicUsize,
        max_active: AtomicUsize,
        close_count: AtomicUsize,
        closed: AtomicBool,
        closed_notify: Notify,
    }

    impl FakeBrowse {
        fn new(responses: Vec<Result<Option<RawEvent>, TransportError>>, block: bool) -> Self {
            Self(Arc::new(FakeBrowseState {
                responses: StdMutex::new(responses.into()),
                entered: Notify::new(),
                release: Semaphore::new(0),
                block: AtomicBool::new(block),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                close_count: AtomicUsize::new(0),
                closed: AtomicBool::new(false),
                closed_notify: Notify::new(),
            }))
        }
    }

    struct ActiveNext<'a>(&'a FakeBrowseState);

    impl Drop for ActiveNext<'_> {
        fn drop(&mut self) {
            self.0.active.fetch_sub(1, Ordering::AcqRel);
        }
    }

    impl BrowseHandle for FakeBrowse {
        fn next(&self) -> BoxFuture<'_, Result<Option<RawEvent>, TransportError>> {
            Box::pin(async move {
                let active = self
                    .0
                    .active
                    .fetch_add(1, Ordering::AcqRel)
                    .saturating_add(1);
                self.0.max_active.fetch_max(active, Ordering::AcqRel);
                let _active = ActiveNext(&self.0);
                self.0.entered.notify_waiters();
                if self.0.closed.load(Ordering::Acquire) {
                    return Ok(None);
                }
                if self.0.block.load(Ordering::Acquire) {
                    let permit = self.0.release.acquire().await;
                    if let Ok(permit) = permit {
                        permit.forget();
                    }
                }
                if self.0.closed.load(Ordering::Acquire) {
                    return Ok(None);
                }
                let mut responses = self
                    .0
                    .responses
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                responses.pop_front().unwrap_or(Ok(None))
            })
        }

        fn request_close(&self) {
            self.0.close_count.fetch_add(1, Ordering::AcqRel);
            self.0.closed.store(true, Ordering::Release);
            self.0.release.add_permits(1);
            self.0.closed_notify.notify_waiters();
        }

        fn closed(&self) -> BoxFuture<'_, Result<(), TransportError>> {
            Box::pin(async move {
                while !self.0.closed.load(Ordering::Acquire) {
                    self.0.closed_notify.notified().await;
                }
                Ok(())
            })
        }
    }

    fn event(name: &str) -> RawEvent {
        RawEvent::Remove {
            service_type: "_test._udp.local.".into(),
            instance_name: name.into(),
        }
    }

    #[tokio::test]
    async fn close_wakes_pending_next_and_is_idempotent() {
        let handle = FakeBrowse::new(vec![Ok(Some(event("late")))], true);
        let session = Arc::new(BrowseSession::new(handle.clone()));
        let entered = handle.0.entered.notified();
        let task = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.next().await }
        });
        entered.await;

        session.close();
        session.close();
        let outcome = task.await;
        assert!(matches!(outcome, Ok(Ok(None))));
        assert!(session.wait_closed().await.is_ok());
        assert_eq!(handle.0.close_count.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn error_is_reported_once_then_terminal_is_sticky() {
        let handle = FakeBrowse::new(
            vec![
                Err(TransportError::new("browse failed")),
                Ok(Some(event("must-not-escape"))),
            ],
            false,
        );
        let session = BrowseSession::new(handle.clone());

        assert!(matches!(session.next().await, Err(error) if error.to_string() == "browse failed"));
        assert!(matches!(session.next().await, Ok(None)));
        assert_eq!(handle.0.close_count.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn clean_transport_terminal_requests_cleanup_and_runs_callbacks_once() {
        let handle = FakeBrowse::new(vec![Ok(None)], false);
        let session = BrowseSession::new(handle.clone());
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_for_close = Arc::clone(&callback_count);
        session.on_close(move || {
            callback_count_for_close.fetch_add(1, Ordering::AcqRel);
        });

        assert!(matches!(session.next().await, Ok(None)));
        session.close();
        assert!(session.wait_closed().await.is_ok());
        assert_eq!(handle.0.close_count.load(Ordering::Acquire), 1);
        assert_eq!(callback_count.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn concurrent_next_calls_are_serialized() {
        let handle = FakeBrowse::new(vec![Ok(Some(event("one"))), Ok(Some(event("two")))], true);
        let session = Arc::new(BrowseSession::new(handle.clone()));
        let first_entered = handle.0.entered.notified();
        let first = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.next().await }
        });
        first_entered.await;
        let second = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.next().await }
        });
        tokio::task::yield_now().await;
        assert_eq!(handle.0.max_active.load(Ordering::Acquire), 1);

        handle.0.release.add_permits(2);
        assert!(matches!(first.await, Ok(Ok(Some(_)))));
        assert!(matches!(second.await, Ok(Ok(Some(_)))));
        assert_eq!(handle.0.max_active.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn cancelling_next_waiter_does_not_cancel_transport_operation() {
        let handle = FakeBrowse::new(vec![Ok(Some(event("observed")))], true);
        let session = Arc::new(BrowseSession::new(handle.clone()));
        let entered = handle.0.entered.notified();
        let task = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.next().await }
        });
        entered.await;

        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert_eq!(handle.0.active.load(Ordering::Acquire), 1);
        assert!(session.wait_closed().await.is_ok());
        assert_eq!(handle.0.active.load(Ordering::Acquire), 0);
        assert_eq!(handle.0.close_count.load(Ordering::Acquire), 1);
    }

    #[derive(Clone)]
    struct FakeAdvertisement(Arc<FakeAdvertisementState>);

    struct FakeAdvertisementState {
        fail: AtomicBool,
        block: AtomicBool,
        entered: Notify,
        release: Semaphore,
        completed: AtomicUsize,
        close_count: AtomicUsize,
        closed: AtomicBool,
        closed_notify: Notify,
    }

    impl FakeAdvertisement {
        fn new(fail: bool, block: bool) -> Self {
            Self(Arc::new(FakeAdvertisementState {
                fail: AtomicBool::new(fail),
                block: AtomicBool::new(block),
                entered: Notify::new(),
                release: Semaphore::new(0),
                completed: AtomicUsize::new(0),
                close_count: AtomicUsize::new(0),
                closed: AtomicBool::new(false),
                closed_notify: Notify::new(),
            }))
        }
    }

    impl AdvertisementHandle for FakeAdvertisement {
        fn update(
            &self,
            _update: AdvertisementUpdate,
        ) -> BoxFuture<'_, Result<(), TransportError>> {
            Box::pin(async move {
                self.0.entered.notify_waiters();
                if self.0.block.load(Ordering::Acquire) {
                    let permit = self.0.release.acquire().await;
                    if let Ok(permit) = permit {
                        permit.forget();
                    }
                }
                self.0.completed.fetch_add(1, Ordering::AcqRel);
                if self.0.fail.load(Ordering::Acquire) {
                    Err(TransportError::new("update failed"))
                } else {
                    Ok(())
                }
            })
        }

        fn request_close(&self) {
            self.0.close_count.fetch_add(1, Ordering::AcqRel);
            self.0.closed.store(true, Ordering::Release);
            self.0.closed_notify.notify_waiters();
        }

        fn closed(&self) -> BoxFuture<'_, Result<(), TransportError>> {
            Box::pin(async move {
                while !self.0.closed.load(Ordering::Acquire) {
                    self.0.closed_notify.notified().await;
                }
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn advertisement_update_failure_closes_and_cleanup_is_awaitable() {
        let handle = FakeAdvertisement::new(true, false);
        let session = AdvertisementSession::new(handle.clone());
        let update = AdvertisementUpdate {
            addrs: Vec::new(),
            txt: Vec::new(),
        };

        assert!(
            matches!(session.update(update).await, Err(error) if error.to_string() == "update failed")
        );
        session.close();
        assert!(session.wait_closed().await.is_ok());
        assert_eq!(handle.0.close_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn drop_requests_advertisement_cleanup_without_an_async_runtime() {
        let handle = FakeAdvertisement::new(false, false);
        drop(AdvertisementSession::new(handle.clone()));
        assert_eq!(handle.0.close_count.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn cancelling_update_waiter_does_not_cancel_transport_operation() {
        let handle = FakeAdvertisement::new(false, true);
        let session = Arc::new(AdvertisementSession::new(handle.clone()));
        let entered = handle.0.entered.notified();
        let task = tokio::spawn({
            let session = Arc::clone(&session);
            async move {
                session
                    .update(AdvertisementUpdate {
                        addrs: Vec::new(),
                        txt: Vec::new(),
                    })
                    .await
            }
        });
        entered.await;

        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        session.close();
        assert_eq!(handle.0.completed.load(Ordering::Acquire), 0);
        handle.0.release.add_permits(1);
        assert!(session.wait_closed().await.is_ok());
        assert_eq!(handle.0.completed.load(Ordering::Acquire), 1);
        assert_eq!(handle.0.close_count.load(Ordering::Acquire), 1);
    }

    #[allow(dead_code)]
    async fn assert_box_future_is_send() {
        fn require_send(_: impl Send) {}
        require_send(pending::<Result<(), TransportError>>());
    }
}
