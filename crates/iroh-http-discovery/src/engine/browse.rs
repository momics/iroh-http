use super::{NextEvent, RawEvent, TransportError};

/// Observable lifecycle of a browse session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowseStatus {
    #[default]
    Open,
    Closed,
    Failed,
}

/// Input from a platform browse transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseInput {
    Event(RawEvent),
    Closed,
    Failed(TransportError),
}

/// Pure terminal-state reducer for browse results.
///
/// It guarantees exactly one externally visible terminal result and ignores
/// stale native callbacks after termination.
#[derive(Debug, Default)]
pub struct BrowseLifecycle {
    status: BrowseStatus,
}

impl BrowseLifecycle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self) -> BrowseStatus {
        self.status
    }

    /// Reduce one transport input to at most one public poll result.
    pub fn apply(&mut self, input: BrowseInput) -> Option<NextEvent> {
        if self.status != BrowseStatus::Open {
            return None;
        }

        match input {
            BrowseInput::Event(event) => Some(Ok(Some(event))),
            BrowseInput::Closed => {
                self.status = BrowseStatus::Closed;
                Some(Ok(None))
            }
            BrowseInput::Failed(error) => {
                self.status = BrowseStatus::Failed;
                Some(Err(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ServiceRecord, TransportErrorKind};
    use super::*;

    fn upsert(instance_name: &str) -> RawEvent {
        RawEvent::Upsert(ServiceRecord {
            service_type: "_demo._udp.local.".into(),
            instance_name: instance_name.into(),
            host: None,
            port: 4242,
            addrs: vec![],
            txt: vec![],
        })
    }

    #[test]
    fn forwards_events_in_transport_order_including_remove_before_upsert() {
        let mut lifecycle = BrowseLifecycle::new();
        let remove = RawEvent::Remove {
            service_type: "_demo._udp.local.".into(),
            instance_name: "late".into(),
        };

        assert_eq!(
            lifecycle.apply(BrowseInput::Event(remove.clone())),
            Some(Ok(Some(remove)))
        );
        assert_eq!(
            lifecycle.apply(BrowseInput::Event(upsert("late").clone())),
            Some(Ok(Some(upsert("late"))))
        );
        assert_eq!(lifecycle.status(), BrowseStatus::Open);
    }

    #[test]
    fn clean_close_is_emitted_once_and_late_callbacks_are_ignored() {
        let mut lifecycle = BrowseLifecycle::new();

        assert_eq!(lifecycle.apply(BrowseInput::Closed), Some(Ok(None)));
        assert_eq!(lifecycle.status(), BrowseStatus::Closed);
        assert_eq!(lifecycle.apply(BrowseInput::Event(upsert("stale"))), None);
        assert_eq!(lifecycle.apply(BrowseInput::Closed), None);
    }

    #[test]
    fn failure_is_emitted_once_and_then_becomes_closed_to_callers() {
        let mut lifecycle = BrowseLifecycle::new();
        let error = TransportError::new(TransportErrorKind::Browse, "permission revoked");

        assert_eq!(
            lifecycle.apply(BrowseInput::Failed(error.clone())),
            Some(Err(error))
        );
        assert_eq!(lifecycle.status(), BrowseStatus::Failed);
        assert_eq!(
            lifecycle.apply(BrowseInput::Failed(TransportError::new(
                TransportErrorKind::Browse,
                "duplicate"
            ))),
            None
        );
    }
}
