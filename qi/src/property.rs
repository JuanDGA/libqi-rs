use std::sync::{Arc, Mutex};

use crate::signal::Signal;

enum PropertyKind<T> {
    Local {
        value: Arc<Mutex<T>>,
        signal: Signal<T>,
    },
    #[allow(dead_code)]
    Remote,
}

pub struct Property<T> {
    kind: PropertyKind<T>,
}

impl<T> Clone for Property<T> {
    fn clone(&self) -> Self {
        Self {
            kind: match &self.kind {
                PropertyKind::Local { value, signal } => PropertyKind::Local {
                    value: Arc::clone(value),
                    signal: signal.clone(),
                },
                PropertyKind::Remote => PropertyKind::Remote,
            },
        }
    }
}

impl<T: Clone> Property<T> {
    pub fn new(value: T) -> Self {
        Self {
            kind: PropertyKind::Local {
                value: Arc::new(Mutex::new(value)),
                signal: Signal::new(),
            },
        }
    }

    pub fn get(&self) -> T {
        match &self.kind {
            PropertyKind::Local { value, .. } => value.lock().unwrap().clone(),
            PropertyKind::Remote => unreachable!("remote property handles are not constructed"),
        }
    }

    /// Stores `value` and emits it on the signal entry registered for this property.
    pub fn set(&self, value: T) {
        let PropertyKind::Local {
            value: slot,
            signal,
        } = &self.kind
        else {
            return;
        };
        *slot.lock().unwrap() = value.clone();
        signal.emit(value);
    }

    pub fn subscribe(&self, listener: impl Fn(T) + Send + Sync + 'static) {
        let PropertyKind::Local { signal, .. } = &self.kind else {
            return;
        };
        signal.subscribe(listener);
    }
}
