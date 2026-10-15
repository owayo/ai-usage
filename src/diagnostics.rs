//! TUI の描画中は警告を蓄え、端末へ直接書き込まない。

use std::fmt::Display;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct Diagnostics(Option<Arc<Mutex<Vec<String>>>>);

impl Diagnostics {
    pub fn new(capture: bool) -> Self {
        Self(capture.then(|| Arc::new(Mutex::new(Vec::new()))))
    }

    pub fn warn(&self, message: impl Display) {
        if let Some(messages) = &self.0 {
            messages
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(message.to_string());
        } else {
            eprintln!("{message}");
        }
    }

    pub fn take(&self) -> Vec<String> {
        self.0.as_ref().map_or_else(Vec::new, |messages| {
            std::mem::take(
                &mut *messages
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_warnings_are_shared_with_workers_and_drained_per_refresh() {
        let diagnostics = Diagnostics::new(true);
        let worker = diagnostics.clone();
        std::thread::spawn(move || worker.warn("worker warning"))
            .join()
            .unwrap();
        assert_eq!(diagnostics.take(), ["worker warning"]);
        assert!(diagnostics.take().is_empty());
        diagnostics.warn("next refresh");
        assert_eq!(diagnostics.take(), ["next refresh"]);
        assert!(Diagnostics::new(true).take().is_empty());
    }
}
