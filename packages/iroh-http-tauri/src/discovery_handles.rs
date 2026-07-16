//! Monotonic public handles for desktop discovery resources.
//!
//! `slab::Slab` indices are immediately reusable, so a delayed close for an
//! old session can otherwise delete a newer resource (including a different
//! advertisement kind sharing the same slab). These handles are never reused
//! during the process lifetime.

use std::collections::HashMap;

pub(crate) struct DiscoveryHandleMap<T> {
    next: u64,
    entries: HashMap<u64, T>,
}

impl<T> Default for DiscoveryHandleMap<T> {
    fn default() -> Self {
        Self {
            next: 1,
            entries: HashMap::new(),
        }
    }
}

impl<T> DiscoveryHandleMap<T> {
    pub(crate) fn insert(&mut self, value: T) -> Result<u64, String> {
        let handle = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| "discovery handle space exhausted".to_string())?;
        self.entries.insert(handle, value);
        Ok(handle)
    }

    pub(crate) fn get(&self, handle: u64) -> Option<&T> {
        self.entries.get(&handle)
    }

    pub(crate) fn remove(&mut self, handle: u64) -> Option<T> {
        self.entries.remove(&handle)
    }

    /// Remove `handle` only when its current resource still matches the
    /// generation observed by an asynchronous caller.
    pub(crate) fn remove_if(
        &mut self,
        handle: u64,
        predicate: impl FnOnce(&T) -> bool,
    ) -> Option<T> {
        if self.entries.get(&handle).is_some_and(predicate) {
            self.entries.remove(&handle)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DiscoveryHandleMap;

    #[test]
    fn removed_handles_are_never_reused() {
        let mut handles = DiscoveryHandleMap::default();
        let first = handles.insert("old").unwrap();
        assert_eq!(handles.remove(first), Some("old"));
        let second = handles.insert("new").unwrap();

        assert_ne!(first, second);
        assert!(handles.get(first).is_none());
        assert_eq!(handles.get(second), Some(&"new"));
    }

    #[test]
    fn conditional_remove_cannot_delete_a_different_generation() {
        let mut handles = DiscoveryHandleMap::default();
        let handle = handles.insert("current").unwrap();

        assert_eq!(handles.remove_if(handle, |value| *value == "stale"), None);
        assert_eq!(handles.get(handle), Some(&"current"));
        assert_eq!(
            handles.remove_if(handle, |value| *value == "current"),
            Some("current")
        );
    }
}
