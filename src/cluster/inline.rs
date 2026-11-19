use std::sync::Arc;

pub(crate) type PayloadPin = Arc<[u8]>;

#[derive(Debug)]
pub(super) struct Payload(Arc<[u8]>);

impl From<Vec<u8>> for Payload {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes.into())
    }
}

impl From<Arc<[u8]>> for Payload {
    fn from(bytes: Arc<[u8]>) -> Self {
        Self(bytes)
    }
}

impl Payload {
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn with_bytes<T>(&self, f: impl FnOnce(&[u8]) -> T) -> T {
        f(&self.0)
    }

    pub(super) fn shared(&self) -> &Arc<[u8]> {
        &self.0
    }

    pub(super) fn pin(&self) -> PayloadPin {
        self.0.clone()
    }
}
