use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::core::snapshot::CoreSnapshot;

#[derive(Clone)]
pub struct SnapshotStore {
    current: Arc<ArcSwap<CoreSnapshot>>,
}

impl SnapshotStore {
    pub fn new(initial: CoreSnapshot) -> Self {
        Self {
            current: Arc::new(ArcSwap::from_pointee(initial)),
        }
    }

    pub fn load(&self) -> Arc<CoreSnapshot> {
        self.current.load_full()
    }

    pub fn publish(&self, snapshot: CoreSnapshot) {
        self.current.store(Arc::new(snapshot));
    }
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new(CoreSnapshot::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_keep_the_old_immutable_snapshot_after_publish() {
        let store = SnapshotStore::default();
        let old = store.load();
        let mut next = CoreSnapshot::default();
        next.revision = 1;
        store.publish(next);

        assert_eq!(old.revision, 0);
        assert_eq!(store.load().revision, 1);
    }
}
