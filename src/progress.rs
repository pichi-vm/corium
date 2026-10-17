// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Started,
    Modules,
    Discovered,
    Assembled,
    Mounted,
    Pivoted,
}

impl Phase {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "started" => Ok(Self::Started),
            "modules" => Ok(Self::Modules),
            "discovered" => Ok(Self::Discovered),
            "assembled" => Ok(Self::Assembled),
            "mounted" => Ok(Self::Mounted),
            "pivoted" => Ok(Self::Pivoted),
            _ => Err(format!("invalid corium.halt value: {value}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Modules => "modules",
            Self::Discovered => "discovered",
            Self::Assembled => "assembled",
            Self::Mounted => "mounted",
            Self::Pivoted => "pivoted",
        }
    }
}

pub(crate) struct Progress {
    started: Instant,
    phase_started: Instant,
    pub(crate) shutdown: Option<Phase>,
}

impl Progress {
    pub(crate) fn new() -> Self {
        let started = Instant::now();
        Self {
            started,
            phase_started: started,
            shutdown: None,
        }
    }

    /// Record a completed boundary; true means the caller should return to
    /// PID 1's poweroff path. No following boot phase may run in that case.
    #[must_use]
    pub(crate) fn complete(&mut self, phase: Phase) -> bool {
        let now = Instant::now();
        log::info!(target: "corium",
            phase = phase.name(),
            elapsed_us = now.duration_since(self.started).as_micros(),
            duration_us = now.duration_since(self.phase_started).as_micros();
            "phase completed");
        if self.shutdown == Some(phase) {
            log::info!(target: "corium", phase = phase.name(); "shutdown requested");
            return true;
        }
        // Exclude this phase's completion-record write from the next phase's
        // duration. Total elapsed time still includes all logging overhead.
        self.phase_started = Instant::now();
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_names_round_trip() {
        for phase in [
            Phase::Started,
            Phase::Modules,
            Phase::Discovered,
            Phase::Assembled,
            Phase::Mounted,
            Phase::Pivoted,
        ] {
            assert_eq!(Phase::parse(phase.name()).unwrap(), phase);
        }
    }
}
