//! Native host acceptance policy. No GPUI dependency; tests run through the
//! dependency-light native bridge checker, not an actual GPUI window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputReadiness {
    Ready,
    Paused,
    Disconnected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectionReason {
    Saturated,
    Paused,
    HostDisconnected,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct InputPolicy {
    state: InputReadiness,
}
impl Default for InputPolicy {
    fn default() -> Self {
        Self { state: InputReadiness::Ready }
    }
}
impl InputPolicy {
    pub(crate) fn state(&self) -> InputReadiness {
        self.state
    }
    pub(crate) fn rejection(&self) -> Option<RejectionReason> {
        match self.state {
            InputReadiness::Ready => None,
            InputReadiness::Paused => Some(RejectionReason::Paused),
            InputReadiness::Disconnected => Some(RejectionReason::HostDisconnected),
        }
    }
    pub(crate) fn saturate(&mut self) {
        if self.state != InputReadiness::Disconnected {
            self.state = InputReadiness::Paused;
        }
    }
    pub(crate) fn disconnect(&mut self) {
        self.state = InputReadiness::Disconnected;
    }
    pub(crate) fn resume(&mut self, pending: usize) -> bool {
        if self.state == InputReadiness::Disconnected || pending != 0 {
            return false;
        }
        self.state = InputReadiness::Ready;
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saturation_cannot_resume_before_fifo_drains() {
        let mut policy = InputPolicy::default();
        assert_eq!(policy.rejection(), None);
        policy.saturate();
        assert_eq!(policy.state(), InputReadiness::Paused);
        assert!(!policy.resume(1));
        assert_eq!(policy.rejection(), Some(RejectionReason::Paused));
        assert!(policy.resume(0));
        assert_eq!(policy.state(), InputReadiness::Ready);
    }
    #[test]
    fn disconnect_is_terminal_even_after_saturation_and_drain() {
        let mut policy = InputPolicy::default();
        policy.disconnect();
        policy.saturate();
        assert_eq!(policy.state(), InputReadiness::Disconnected);
        assert_eq!(policy.rejection(), Some(RejectionReason::HostDisconnected));
        assert!(!policy.resume(0));
    }
}
