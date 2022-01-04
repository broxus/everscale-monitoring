use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct AverageValueCounter(AtomicU64);

impl AverageValueCounter {
    pub fn with_value(value: u32) -> Self {
        Self(AtomicU64::new(((value as u64) << 32) | 0x1))
    }

    pub fn push(&self, value: u32) {
        self.0
            .fetch_add(((value as u64) << 32) | 0x1, Ordering::Release);
    }

    pub fn reset(&self) -> Option<f64> {
        let value = self.0.swap(0, Ordering::AcqRel);
        let count = value as u32;
        let value = value >> 32;

        if count > 0 {
            Some((value as f64) / (count as f64))
        } else {
            None
        }
    }
}
