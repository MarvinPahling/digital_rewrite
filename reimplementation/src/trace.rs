//! Bounded, opt-in, fixed-size simulator trace records.
use crate::domain::{ComponentId, PinRef};
use crate::runtime::{STARTUP_RANDOM_POLICY, StartupPhase};
use std::{mem::size_of, vec::Vec};

pub const MAX_TRACE_CAPACITY: usize = 65_536;
pub const MAX_TRACE_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceHeader {
    pub startup_seed: u64,
    pub random_policy: &'static str,
    pub session: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceEvent {
    FrontierBegin {
        startup: bool,
        phase: StartupPhase,
    },
    GateEvaluated {
        component: ComponentId,
        immediate: bool,
        value: super::runtime::Level,
    },
    NetPublished {
        net: usize,
        driver: PinRef,
        old: super::runtime::Level,
        new: super::runtime::Level,
    },
    FrontierComplete {
        startup: bool,
        evaluated: usize,
    },
    Lifecycle {
        phase: StartupPhase,
    },
    InputChanged {
        component: ComponentId,
        value: bool,
    },
    Stable,
    Terminal,
    Stop,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceRecord {
    pub sequence: u64,
    pub event: TraceEvent,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceConfigError {
    CapacityOutOfRange(usize),
    ByteLimit { capacity: usize, bytes: usize },
    AllocationFailed,
}
impl std::fmt::Display for TraceConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapacityOutOfRange(n) => write!(
                f,
                "trace capacity must be 1..={MAX_TRACE_CAPACITY} (got {n})"
            ),
            Self::ByteLimit { capacity, bytes } => write!(
                f,
                "trace capacity {capacity} requires {bytes} bytes, exceeding {MAX_TRACE_BYTES}"
            ),
            Self::AllocationFailed => f.write_str("unable to allocate trace ring"),
        }
    }
}
impl std::error::Error for TraceConfigError {}

/// Fixed-capacity ring. Records carry monotonically increasing sequence numbers; evictions
/// advance `dropped` and `first_sequence`, making loss explicit without allocating per event.
pub struct TraceLog {
    pub header: TraceHeader,
    slots: Vec<Option<TraceRecord>>,
    next: usize,
    len: usize,
    next_sequence: u64,
    sequence_exhausted: bool,
    unrecorded_stop: bool,
    dropped: u64,
}
impl TraceLog {
    pub fn new(capacity: usize, startup_seed: u64, session: u64) -> Result<Self, TraceConfigError> {
        if !(1..=MAX_TRACE_CAPACITY).contains(&capacity) {
            return Err(TraceConfigError::CapacityOutOfRange(capacity));
        }
        let bytes = capacity
            .checked_mul(size_of::<Option<TraceRecord>>())
            .ok_or(TraceConfigError::ByteLimit {
                capacity,
                bytes: usize::MAX,
            })?;
        if bytes > MAX_TRACE_BYTES {
            return Err(TraceConfigError::ByteLimit { capacity, bytes });
        }
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(capacity)
            .map_err(|_| TraceConfigError::AllocationFailed)?;
        slots.resize_with(capacity, || None);
        Ok(Self {
            header: TraceHeader {
                startup_seed,
                random_policy: STARTUP_RANDOM_POLICY,
                session,
            },
            slots,
            next: 0,
            len: 0,
            next_sequence: 1,
            sequence_exhausted: false,
            unrecorded_stop: false,
            dropped: 0,
        })
    }
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    /// True when simulation stopped but sequence exhaustion prevented recording `Stop`.
    pub fn has_unrecorded_stop(&self) -> bool {
        self.unrecorded_stop
    }
    pub(crate) fn mark_unrecorded_stop(&mut self) {
        self.unrecorded_stop = true;
    }
    pub fn first_sequence(&self) -> Option<u64> {
        self.records().next().map(|r| r.sequence)
    }
    pub fn records(&self) -> impl Iterator<Item = TraceRecord> + '_ {
        let start = (self.next + self.slots.len() - self.len) % self.slots.len();
        (0..self.len).filter_map(move |offset| self.slots[(start + offset) % self.slots.len()])
    }
    pub(crate) fn can_record(&self, count: usize) -> bool {
        if count == 0 {
            return true;
        }
        if self.sequence_exhausted {
            return false;
        }
        u64::try_from(count - 1)
            .ok()
            .and_then(|last| self.next_sequence.checked_add(last))
            .is_some()
    }
    pub(crate) fn record(&mut self, event: TraceEvent) {
        debug_assert!(self.can_record(1));
        let sequence = self.next_sequence;
        if sequence == u64::MAX {
            self.sequence_exhausted = true
        } else {
            self.next_sequence += 1;
        }
        if self.len == self.slots.len() {
            self.dropped = self.dropped.saturating_add(1)
        } else {
            self.len += 1;
        }
        self.slots[self.next] = Some(TraceRecord { sequence, event });
        self.next = (self.next + 1) % self.slots.len();
    }
    #[cfg(test)]
    pub(crate) fn force_sequence(&mut self, sequence: u64) {
        self.next_sequence = sequence;
        self.sequence_exhausted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capacity_is_checked_and_ring_eviction_is_explicit() {
        assert!(matches!(
            TraceLog::new(0, 0, 0),
            Err(TraceConfigError::CapacityOutOfRange(0))
        ));
        assert!(matches!(
            TraceLog::new(MAX_TRACE_CAPACITY + 1, 0, 0),
            Err(TraceConfigError::CapacityOutOfRange(_))
        ));
        let mut trace = TraceLog::new(1, 44, 7).unwrap();
        trace.record(TraceEvent::Stable);
        trace.record(TraceEvent::Stop);
        assert_eq!(trace.header.startup_seed, 44);
        assert_eq!(trace.header.session, 7);
        assert_eq!(trace.dropped(), 1);
        assert_eq!(trace.first_sequence(), Some(2));
        assert_eq!(
            trace.records().map(|r| r.event).collect::<Vec<_>>(),
            [TraceEvent::Stop]
        );
    }
    #[test]
    fn sequence_headroom_accepts_maximum_once_then_rejects() {
        let mut trace = TraceLog::new(2, 0, 0).unwrap();
        trace.force_sequence(u64::MAX);
        assert!(trace.can_record(1));
        assert!(!trace.can_record(2));
        trace.record(TraceEvent::Stop);
        assert!(!trace.can_record(1));
        assert_eq!(trace.records().next().unwrap().sequence, u64::MAX);
    }
}
