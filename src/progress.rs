use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar as IndicatifBar, ProgressStyle};
use once_cell::sync::Lazy;

static MULTI_PROGRESS: Lazy<MultiProgress> = Lazy::new(MultiProgress::new);

pub struct ProgressBar {
    inner: IndicatifBar,
    total: AtomicU64,
}

impl ProgressBar {
    pub fn new(title: &str, total: u64, label: &str) -> Self {
        let inner = MULTI_PROGRESS.add(if total == 0 {
            IndicatifBar::new_spinner()
        } else {
            IndicatifBar::new(total)
        });
        let suffix = if label.is_empty() {
            String::new()
        } else {
            format!(" ({{pos}}/{{len}} {label})")
        };
        let template = format!(
            "{{prefix:>28}}: [{{bar:40.cyan/blue}}] {{percent:>3}}%{suffix} {{binary_bytes_per_sec}}"
        );
        inner.set_style(
            ProgressStyle::with_template(&template)
                .expect("valid progress template")
                .progress_chars("=> "),
        );
        inner.set_prefix(title.to_owned());
        inner.enable_steady_tick(Duration::from_millis(200));
        Self {
            inner,
            total: AtomicU64::new(total),
        }
    }

    /// A bar that draws nothing, for when another program owns the terminal.
    pub fn hidden() -> Self {
        Self {
            inner: IndicatifBar::hidden(),
            total: AtomicU64::new(0),
        }
    }

    pub fn set_total(&self, total: u64) {
        if total > 0
            && self
                .total
                .compare_exchange(0, total, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.inner.set_length(total);
        }
    }

    pub fn update(&self, current: u64) {
        let total = self.total.load(Ordering::Relaxed);
        self.inner.set_position(if total > 0 {
            current.min(total)
        } else {
            current
        });
    }

    pub fn finish(&self) {
        self.inner.finish_and_clear();
    }
}
