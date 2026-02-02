//! Emergency kill switch.

use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{error, info};

/// Emergency kill switch for halting trading.
pub struct KillSwitch {
    active: AtomicBool,
}

impl KillSwitch {
    /// Create a new kill switch.
    pub fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
        }
    }

    /// Activate the kill switch.
    pub fn activate(&self) {
        self.active.store(true, Ordering::SeqCst);
        error!("KILL SWITCH ACTIVATED - All trading halted");
    }

    /// Reset the kill switch.
    pub fn reset(&self) {
        self.active.store(false, Ordering::SeqCst);
        info!("Kill switch reset");
    }

    /// Check if the kill switch is active.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

impl Default for KillSwitch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kill_switch() {
        let ks = KillSwitch::new();
        assert!(!ks.is_active());

        ks.activate();
        assert!(ks.is_active());

        ks.reset();
        assert!(!ks.is_active());
    }
}
