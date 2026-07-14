use super::{ServiceConfig, TransportError, TransportResult};

/// Observable lifecycle of an advertisement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvertiseStatus {
    Starting,
    Ready,
    Updating,
    Stopping,
    Closed,
    Failed,
}

/// Request or acknowledgement reduced by [`AdvertiseLifecycle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvertiseInput {
    Started,
    StartFailed(TransportError),
    UpdateRequested(ServiceConfig),
    Updated,
    UpdateFailed(TransportError),
    StopRequested,
    Stopped,
    StopFailed(TransportError),
}

/// Command for the platform transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvertiseCommand {
    Start(ServiceConfig),
    Update(ServiceConfig),
    Stop,
}

/// Effect produced by the pure advertisement reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvertiseEffect {
    Command(AdvertiseCommand),
    /// The initial advertisement has been acknowledged by the transport.
    Ready,
    /// Permanent session outcome. `Ok(())` is a clean stop.
    Terminal(TransportResult<()>),
}

#[derive(Debug)]
enum State {
    Starting {
        initial: ServiceConfig,
        pending_update: Option<ServiceConfig>,
        stop_requested: bool,
    },
    Ready(ServiceConfig),
    Updating {
        target: ServiceConfig,
        pending_update: Option<ServiceConfig>,
        stop_requested: bool,
    },
    Stopping,
    Closed,
    Failed,
}

/// Pure lifecycle reducer for one advertisement.
///
/// Update requests are coalesced while an update is in flight. Stop wins over
/// pending updates, and stale native acknowledgements after a terminal outcome
/// are ignored.
#[derive(Debug)]
pub struct AdvertiseLifecycle {
    state: State,
}

impl AdvertiseLifecycle {
    /// Create the reducer and the first command to execute.
    pub fn new(initial: ServiceConfig) -> (Self, AdvertiseEffect) {
        let command = AdvertiseEffect::Command(AdvertiseCommand::Start(initial.clone()));
        (
            Self {
                state: State::Starting {
                    initial,
                    pending_update: None,
                    stop_requested: false,
                },
            },
            command,
        )
    }

    pub fn status(&self) -> AdvertiseStatus {
        match self.state {
            State::Starting { .. } => AdvertiseStatus::Starting,
            State::Ready(_) => AdvertiseStatus::Ready,
            State::Updating { .. } => AdvertiseStatus::Updating,
            State::Stopping => AdvertiseStatus::Stopping,
            State::Closed => AdvertiseStatus::Closed,
            State::Failed => AdvertiseStatus::Failed,
        }
    }

    /// Reduce one input. A transition can produce both a readiness signal and
    /// a queued command, so effects are returned as a short vector.
    pub fn apply(&mut self, input: AdvertiseInput) -> Vec<AdvertiseEffect> {
        match (&mut self.state, input) {
            (State::Starting { pending_update, .. }, AdvertiseInput::UpdateRequested(config)) => {
                *pending_update = Some(config);
                vec![]
            }
            (State::Starting { stop_requested, .. }, AdvertiseInput::StopRequested) => {
                *stop_requested = true;
                vec![]
            }
            (State::Starting { .. }, AdvertiseInput::Started) => self.finish_start(),
            (State::Starting { .. }, AdvertiseInput::StartFailed(error)) => self.fail(error),

            (State::Ready(current), AdvertiseInput::UpdateRequested(config))
                if *current != config =>
            {
                self.state = State::Updating {
                    target: config.clone(),
                    pending_update: None,
                    stop_requested: false,
                };
                vec![AdvertiseEffect::Command(AdvertiseCommand::Update(config))]
            }
            (State::Ready(_), AdvertiseInput::UpdateRequested(_)) => vec![],
            (State::Ready(_), AdvertiseInput::StopRequested) => self.begin_stop(),

            (State::Updating { pending_update, .. }, AdvertiseInput::UpdateRequested(config)) => {
                *pending_update = Some(config);
                vec![]
            }
            (State::Updating { stop_requested, .. }, AdvertiseInput::StopRequested) => {
                *stop_requested = true;
                vec![]
            }
            (State::Updating { .. }, AdvertiseInput::Updated) => self.finish_update(),
            (State::Updating { .. }, AdvertiseInput::UpdateFailed(error)) => self.fail(error),

            (State::Stopping, AdvertiseInput::Stopped) => {
                self.state = State::Closed;
                vec![AdvertiseEffect::Terminal(Ok(()))]
            }
            (State::Stopping, AdvertiseInput::StopFailed(error)) => self.fail(error),

            // Native callbacks are asynchronous. An acknowledgement for an
            // operation that is no longer current is stale, not a new state.
            _ => vec![],
        }
    }

    fn finish_start(&mut self) -> Vec<AdvertiseEffect> {
        let State::Starting {
            initial,
            pending_update,
            stop_requested,
        } = std::mem::replace(&mut self.state, State::Failed)
        else {
            return vec![];
        };

        let mut effects = vec![AdvertiseEffect::Ready];
        if stop_requested {
            self.state = State::Stopping;
            effects.push(AdvertiseEffect::Command(AdvertiseCommand::Stop));
        } else if let Some(target) = pending_update.filter(|target| target != &initial) {
            self.state = State::Updating {
                target: target.clone(),
                pending_update: None,
                stop_requested: false,
            };
            effects.push(AdvertiseEffect::Command(AdvertiseCommand::Update(target)));
        } else {
            self.state = State::Ready(initial);
        }
        effects
    }

    fn finish_update(&mut self) -> Vec<AdvertiseEffect> {
        let State::Updating {
            target,
            pending_update,
            stop_requested,
        } = std::mem::replace(&mut self.state, State::Failed)
        else {
            return vec![];
        };

        if stop_requested {
            self.state = State::Stopping;
            return vec![AdvertiseEffect::Command(AdvertiseCommand::Stop)];
        }

        if let Some(next) = pending_update.filter(|next| next != &target) {
            self.state = State::Updating {
                target: next.clone(),
                pending_update: None,
                stop_requested: false,
            };
            vec![AdvertiseEffect::Command(AdvertiseCommand::Update(next))]
        } else {
            self.state = State::Ready(target);
            vec![]
        }
    }

    fn begin_stop(&mut self) -> Vec<AdvertiseEffect> {
        self.state = State::Stopping;
        vec![AdvertiseEffect::Command(AdvertiseCommand::Stop)]
    }

    fn fail(&mut self, error: TransportError) -> Vec<AdvertiseEffect> {
        self.state = State::Failed;
        vec![AdvertiseEffect::Terminal(Err(error))]
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Protocol, TransportErrorKind};
    use super::*;

    fn config(instance_name: &str, port: u16) -> ServiceConfig {
        ServiceConfig {
            service_name: "demo".into(),
            instance_name: instance_name.into(),
            port,
            addrs: vec![],
            txt: vec![],
            protocol: Protocol::Udp,
        }
    }

    #[test]
    fn starts_updates_and_stops_with_explicit_acknowledgements() {
        let first = config("one", 1);
        let second = config("one", 2);
        let (mut lifecycle, start) = AdvertiseLifecycle::new(first);

        assert_eq!(
            start,
            AdvertiseEffect::Command(AdvertiseCommand::Start(config("one", 1)))
        );
        assert_eq!(
            lifecycle.apply(AdvertiseInput::Started),
            vec![AdvertiseEffect::Ready]
        );
        assert_eq!(
            lifecycle.apply(AdvertiseInput::UpdateRequested(second.clone())),
            vec![AdvertiseEffect::Command(AdvertiseCommand::Update(second))]
        );
        assert_eq!(lifecycle.apply(AdvertiseInput::Updated), vec![]);
        assert_eq!(lifecycle.status(), AdvertiseStatus::Ready);
        assert_eq!(
            lifecycle.apply(AdvertiseInput::StopRequested),
            vec![AdvertiseEffect::Command(AdvertiseCommand::Stop)]
        );
        assert_eq!(
            lifecycle.apply(AdvertiseInput::Stopped),
            vec![AdvertiseEffect::Terminal(Ok(()))]
        );
        assert_eq!(lifecycle.status(), AdvertiseStatus::Closed);
    }

    #[test]
    fn update_requests_are_coalesced_and_stop_wins() {
        let (mut lifecycle, _) = AdvertiseLifecycle::new(config("one", 1));
        lifecycle.apply(AdvertiseInput::Started);
        lifecycle.apply(AdvertiseInput::UpdateRequested(config("one", 2)));
        lifecycle.apply(AdvertiseInput::UpdateRequested(config("one", 3)));
        lifecycle.apply(AdvertiseInput::StopRequested);

        assert_eq!(
            lifecycle.apply(AdvertiseInput::Updated),
            vec![AdvertiseEffect::Command(AdvertiseCommand::Stop)]
        );
        assert_eq!(lifecycle.status(), AdvertiseStatus::Stopping);
    }

    #[test]
    fn close_requested_during_start_waits_for_start_then_stops() {
        let (mut lifecycle, _) = AdvertiseLifecycle::new(config("one", 1));

        assert_eq!(lifecycle.apply(AdvertiseInput::StopRequested), vec![]);
        assert_eq!(
            lifecycle.apply(AdvertiseInput::Started),
            vec![
                AdvertiseEffect::Ready,
                AdvertiseEffect::Command(AdvertiseCommand::Stop)
            ]
        );
        assert_eq!(lifecycle.status(), AdvertiseStatus::Stopping);
    }

    #[test]
    fn failure_is_terminal_and_late_callbacks_are_ignored() {
        let (mut lifecycle, _) = AdvertiseLifecycle::new(config("one", 1));
        let error = TransportError::new(TransportErrorKind::Start, "not permitted");

        assert_eq!(
            lifecycle.apply(AdvertiseInput::StartFailed(error.clone())),
            vec![AdvertiseEffect::Terminal(Err(error))]
        );
        assert_eq!(lifecycle.status(), AdvertiseStatus::Failed);
        assert_eq!(lifecycle.apply(AdvertiseInput::Started), vec![]);
        assert_eq!(lifecycle.apply(AdvertiseInput::Stopped), vec![]);
    }

    #[test]
    fn update_queued_during_start_runs_after_ready() {
        let (mut lifecycle, _) = AdvertiseLifecycle::new(config("one", 1));
        lifecycle.apply(AdvertiseInput::UpdateRequested(config("one", 9)));

        assert_eq!(
            lifecycle.apply(AdvertiseInput::Started),
            vec![
                AdvertiseEffect::Ready,
                AdvertiseEffect::Command(AdvertiseCommand::Update(config("one", 9)))
            ]
        );
        assert_eq!(lifecycle.status(), AdvertiseStatus::Updating);
    }
}
