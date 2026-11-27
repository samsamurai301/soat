//! How far behind a background queue is: what operators graph and alert on.

/// One queue's backlog: work that is due and not done yet, and work given up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Backlog {
    /// Items due now and not done.
    pub waiting: i64,
    /// How long the oldest waiting item has been due, in seconds; 0 when
    /// nothing waits.
    pub oldest_secs: f64,
    /// Items given up after their last retry in the past 24 hours.
    pub failed_last_day: i64,
}

impl Backlog {
    /// Whether the oldest waiting item has waited longer than `secs`.
    pub fn stuck(&self, secs: f64) -> bool {
        self.waiting > 0 && self.oldest_secs > secs
    }
}
