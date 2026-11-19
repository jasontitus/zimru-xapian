use std::sync::{Arc, Mutex, OnceLock};

/// Pin on immutable bytes. A boxed slice avoids the mandatory copy into an
/// inline Arc slice. Shrinking the decoder's Vec may still move its allocation.
#[derive(Clone, Debug)]
pub(crate) enum PayloadPin {
    Decoded(Arc<Box<[u8]>>),
    Shared(Arc<[u8]>),
}

impl AsRef<[u8]> for PayloadPin {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Decoded(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }
}

#[derive(Debug)]
pub(super) struct Payload {
    len: usize,
    // Only the legacy payload() accessor needs an Arc with inline slice storage.
    shared: OnceLock<Arc<[u8]>>,
    decoded: Mutex<Option<Arc<Box<[u8]>>>>,
}

impl From<Vec<u8>> for Payload {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            len: bytes.len(),
            shared: OnceLock::new(),
            decoded: Mutex::new(Some(Arc::new(bytes.into_boxed_slice()))),
        }
    }
}

impl From<Arc<[u8]>> for Payload {
    fn from(bytes: Arc<[u8]>) -> Self {
        Self {
            len: bytes.len(),
            shared: OnceLock::from(bytes),
            decoded: Mutex::new(None),
        }
    }
}

impl Payload {
    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn with_bytes<T>(&self, f: impl FnOnce(&[u8]) -> T) -> T {
        f(self.pin().as_ref())
    }

    pub(super) fn pin(&self) -> PayloadPin {
        if let Some(bytes) = self.shared.get() {
            return PayloadPin::Shared(bytes.clone());
        }
        let decoded = self
            .decoded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match decoded.as_ref() {
            Some(bytes) => PayloadPin::Decoded(bytes.clone()),
            None => PayloadPin::Shared(
                self.shared
                    .get()
                    .expect("shared payload published under lock")
                    .clone(),
            ),
        }
    }

    pub(super) fn shared(&self) -> &Arc<[u8]> {
        if let Some(bytes) = self.shared.get() {
            return bytes;
        }
        let mut decoded = self
            .decoded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = self.shared.get_or_init(|| {
            let source = decoded
                .as_ref()
                .expect("decoded payload before publication");
            Arc::from(source.as_ref().as_ref())
        });
        // Existing blob pins own their bytes until dropped. The cache retains
        // only the shared representation after explicit publication.
        *decoded = None;
        bytes
    }
}
