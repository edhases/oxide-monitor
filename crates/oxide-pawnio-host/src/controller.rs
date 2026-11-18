//! Pure start/stop policy for the optional elevated helper.
//!
//! Kept free of Win32 so the gating rules (startup, explicit Start, UAC denial,
//! Game Mode, stop, config disable) are unit-testable without triggering UAC.

use oxide_core::PawnioState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Launch,
    Stop,
}

fn stop_action(was_active: bool) -> Vec<Action> {
    if was_active {
        vec![Action::Stop]
    } else {
        Vec::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// One-time state at monitor startup. Never launches the helper.
    Initial {
        enabled: bool,
        installed: bool,
    },
    /// `providers.pawnio` or installation state changed. A false→true transition is
    /// an explicit user action after startup and may launch.
    ConfigChanged {
        enabled: bool,
        installed: bool,
    },
    /// Settings' "Enable CPU temperature (admin)" / retry button.
    StartRequested,
    /// Settings' Stop button.
    StopRequested,
    GameMode(bool),
    LaunchDenied,
    LaunchFailed(String),
    Connected,
    ReadFailed(String),
    Disconnected,
    Shutdown,
}

pub struct Controller {
    enabled: bool,
    installed: bool,
    state: PawnioState,
    detail: String,
    last_error: Option<String>,
    denied: bool,
    game_mode: bool,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            enabled: false,
            installed: false,
            state: PawnioState::Disabled,
            detail: "optional elevated PawnIO helper is disabled".into(),
            last_error: None,
            denied: false,
            game_mode: false,
        }
    }
}

impl Controller {
    pub fn state(&self) -> PawnioState {
        self.state
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn installed(&self) -> bool {
        self.installed
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, PawnioState::Starting | PawnioState::Running)
    }

    pub fn on(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Initial { enabled, installed } => {
                self.enabled = enabled;
                self.installed = installed;
                self.denied = false;
                self.state = self.resting_state();
                self.detail = self.resting_detail();
                self.last_error = None;
                Vec::new()
            }
            Event::ConfigChanged { enabled, installed } => {
                let was_enabled = self.enabled;
                let was_installed = self.installed;
                let was_active = self.is_active();
                self.enabled = enabled;
                self.installed = installed;

                if !enabled {
                    self.denied = false;
                    self.state = PawnioState::Disabled;
                    self.detail = "optional elevated PawnIO helper is disabled".into();
                    self.last_error = None;
                    return stop_action(was_active);
                }

                if !installed {
                    self.denied = false;
                    self.state = PawnioState::NotInstalled;
                    self.detail =
                        "PawnIO is not installed; install it manually from pawnio.eu".into();
                    self.last_error = None;
                    return stop_action(was_active);
                }

                // Enabling the provider is an explicit user action; enabling it again
                // after disabling clears a previous UAC denial. Merely detecting a
                // newly installed driver does not trigger a prompt.
                if !was_enabled {
                    self.denied = false;
                    if self.game_mode {
                        self.state = PawnioState::Stopped;
                        self.detail = "paused while Game Mode is active".into();
                        return Vec::new();
                    }
                    return self.begin_start();
                }

                if !was_installed
                    || matches!(
                        self.state,
                        PawnioState::Disabled | PawnioState::NotInstalled | PawnioState::Error
                    )
                {
                    self.state = self.resting_state();
                    self.detail = self.resting_detail();
                    self.last_error = None;
                }
                Vec::new()
            }
            Event::StartRequested => {
                self.denied = false;
                self.last_error = None;
                if !self.enabled {
                    self.state = PawnioState::Stopped;
                    self.detail = "enable providers.pawnio before starting the helper".into();
                    return Vec::new();
                }
                if !self.installed {
                    self.state = PawnioState::NotInstalled;
                    self.detail =
                        "PawnIO is not installed; install it manually from pawnio.eu".into();
                    return Vec::new();
                }
                if self.game_mode {
                    self.state = PawnioState::Stopped;
                    self.detail = "cannot start while Game Mode is active".into();
                    return Vec::new();
                }
                if self.is_active() {
                    return Vec::new();
                }
                self.begin_start()
            }
            Event::StopRequested => {
                let was_active = self.is_active();
                self.state = PawnioState::Stopped;
                self.detail = "stopped by user".into();
                self.last_error = None;
                self.denied = false;
                stop_action(was_active)
            }
            Event::GameMode(active) => {
                self.game_mode = active;
                if active && self.is_active() {
                    self.state = PawnioState::Stopped;
                    self.detail = "stopped by Game Mode".into();
                    self.last_error = None;
                    return vec![Action::Stop];
                }
                Vec::new()
            }
            Event::LaunchDenied => {
                self.denied = true;
                self.state = PawnioState::Denied;
                self.detail = "UAC was declined; press Enable CPU temperature to retry".into();
                self.last_error = None;
                Vec::new()
            }
            Event::LaunchFailed(message) => {
                self.state = PawnioState::Error;
                self.detail = "the helper failed to start".into();
                self.last_error = Some(message);
                vec![Action::Stop]
            }
            Event::Connected => {
                self.state = PawnioState::Running;
                self.detail = "reading CPU Tctl/Tdie over the per-user helper pipe".into();
                self.last_error = None;
                Vec::new()
            }
            Event::ReadFailed(message) => {
                self.state = PawnioState::Error;
                self.detail = "the helper stopped returning a temperature".into();
                self.last_error = Some(message);
                vec![Action::Stop]
            }
            Event::Disconnected => {
                if self.is_active() {
                    self.state = PawnioState::Error;
                    self.detail = "the helper disconnected".into();
                    self.last_error = Some("helper pipe closed unexpectedly".into());
                    vec![Action::Stop]
                } else {
                    Vec::new()
                }
            }
            Event::Shutdown => {
                let was_active = self.is_active();
                self.state = PawnioState::Stopped;
                self.detail = "shutting down".into();
                stop_action(was_active)
            }
        }
    }

    fn begin_start(&mut self) -> Vec<Action> {
        if self.denied {
            // StartRequested clears this before calling; ConfigChanged clears it too.
            self.denied = false;
        }
        self.state = PawnioState::Starting;
        self.detail = "waiting for UAC consent and the helper pipe".into();
        self.last_error = None;
        vec![Action::Launch]
    }

    fn resting_state(&self) -> PawnioState {
        if !self.enabled {
            PawnioState::Disabled
        } else if !self.installed {
            PawnioState::NotInstalled
        } else {
            PawnioState::Stopped
        }
    }

    fn resting_detail(&self) -> String {
        if !self.enabled {
            "optional elevated PawnIO helper is disabled".into()
        } else if !self.installed {
            "PawnIO is not installed; install it manually from pawnio.eu".into()
        } else {
            "installed; press Enable CPU temperature to start the elevated helper".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initial(enabled: bool, installed: bool) -> Controller {
        let mut c = Controller::default();
        c.on(Event::Initial { enabled, installed });
        c
    }

    #[test]
    fn never_launches_at_startup_even_when_enabled() {
        let mut c = initial(true, true);
        assert_eq!(c.state(), PawnioState::Stopped);
        assert!(
            c.on(Event::ConfigChanged {
                enabled: true,
                installed: true
            })
            .is_empty()
        );
        assert_eq!(c.state(), PawnioState::Stopped);
    }

    #[test]
    fn enabling_after_startup_starts_once() {
        let mut c = initial(false, true);
        assert_eq!(c.state(), PawnioState::Disabled);
        assert_eq!(
            c.on(Event::ConfigChanged {
                enabled: true,
                installed: true
            }),
            vec![Action::Launch]
        );
        assert_eq!(c.state(), PawnioState::Starting);
        // Repeated ticks/duplicate config events must not launch twice.
        assert!(
            c.on(Event::ConfigChanged {
                enabled: true,
                installed: true
            })
            .is_empty()
        );
        assert_eq!(c.on(Event::Connected), Vec::<Action>::new());
        assert_eq!(c.state(), PawnioState::Running);
    }

    #[test]
    fn uac_denial_is_not_retried_silently_but_start_retries() {
        let mut c = initial(false, true);
        c.on(Event::ConfigChanged {
            enabled: true,
            installed: true,
        });
        assert_eq!(c.on(Event::LaunchDenied), Vec::<Action>::new());
        assert_eq!(c.state(), PawnioState::Denied);
        assert!(
            c.on(Event::ConfigChanged {
                enabled: true,
                installed: true
            })
            .is_empty()
        );
        assert_eq!(c.state(), PawnioState::Denied);

        assert_eq!(c.on(Event::StartRequested), vec![Action::Launch]);
        assert_eq!(c.state(), PawnioState::Starting);
    }

    #[test]
    fn stop_and_game_mode_stop_the_helper_without_auto_restart() {
        let mut c = initial(true, true);
        c.on(Event::StartRequested);
        c.on(Event::Connected);
        assert_eq!(c.on(Event::StopRequested), vec![Action::Stop]);
        assert_eq!(c.state(), PawnioState::Stopped);
        assert!(
            c.on(Event::ConfigChanged {
                enabled: true,
                installed: true
            })
            .is_empty()
        );

        c.on(Event::StartRequested);
        c.on(Event::Connected);
        assert_eq!(c.on(Event::GameMode(true)), vec![Action::Stop]);
        assert_eq!(c.state(), PawnioState::Stopped);
        assert_eq!(c.on(Event::GameMode(false)), Vec::<Action>::new());
        assert_eq!(c.state(), PawnioState::Stopped);
        assert_eq!(c.on(Event::StartRequested), vec![Action::Launch]);
    }

    #[test]
    fn disabling_the_provider_stops_and_missing_pawnio_never_launches() {
        let mut c = initial(true, true);
        c.on(Event::StartRequested);
        c.on(Event::Connected);
        assert_eq!(
            c.on(Event::ConfigChanged {
                enabled: false,
                installed: true
            }),
            vec![Action::Stop]
        );
        assert_eq!(c.state(), PawnioState::Disabled);

        let mut c = initial(true, false);
        assert_eq!(c.state(), PawnioState::NotInstalled);
        assert_eq!(c.on(Event::StartRequested), Vec::<Action>::new());
        assert_eq!(c.state(), PawnioState::NotInstalled);
    }

    #[test]
    fn read_failure_reports_error_and_stops() {
        let mut c = initial(true, true);
        c.on(Event::StartRequested);
        c.on(Event::Connected);
        assert_eq!(
            c.on(Event::ReadFailed("timeout".into())),
            vec![Action::Stop]
        );
        assert_eq!(c.state(), PawnioState::Error);
        assert_eq!(c.last_error(), Some("timeout"));
    }
}
