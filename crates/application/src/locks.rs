//! Serialization primitives.
//!
//! Two independent concerns:
//!
//! * **Per-instance serialization.** Lifecycle and activation operations on one
//!   instance must not interleave. Two concurrent starts would each decide to
//!   spawn; two concurrent activations could leave the active pointer and the
//!   running configuration disagreeing. The lock lives here, in the application
//!   layer, because an adapter-owned lock would be created per call and serialize
//!   nothing.
//!
//! * **Per-subscription suppression.** A scheduled update and a manual update of
//!   the same subscription must not both run. The second one is skipped, not
//!   queued: queueing would pile up work that the next tick would also want.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use proxy_domain::shared::id::{MihomoInstanceId, SubscriptionId};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// Hands out one lock per instance.
///
/// Read-only operations deliberately do not take this lock, so a long activation
/// does not freeze status queries.
#[derive(Default)]
pub struct InstanceLocks {
    inner: AsyncMutex<HashMap<MihomoInstanceId, Arc<AsyncMutex<()>>>>,
}

impl InstanceLocks {
    /// Creates an empty set of locks.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquires the lock for `instance`, waiting until it is free.
    ///
    /// The returned guard is owned, so it can be held across `.await` points
    /// without borrowing this struct.
    pub async fn acquire(&self, instance: &MihomoInstanceId) -> OwnedMutexGuard<()> {
        // The map lock is released before the instance lock is awaited. Holding
        // it across the await would make one instance's acquisition block every
        // other instance's lookup, turning per-instance locking into a global
        // one under contention.
        let instance_lock = {
            let mut map = self.inner.lock().await;
            Arc::clone(map.entry(instance.clone()).or_default())
        };
        instance_lock.lock_owned().await
    }

    /// Whether an instance currently has an operation in flight.
    ///
    /// Advisory: for diagnostics and tests, not for control flow, since the
    /// answer can change immediately after it is returned.
    pub async fn is_locked(&self, instance: &MihomoInstanceId) -> bool {
        let map = self.inner.lock().await;
        match map.get(instance) {
            Some(lock) => lock.try_lock().is_err(),
            None => false,
        }
    }
}

/// Ensures at most one update per subscription.
pub struct SubscriptionGuards {
    in_flight: Mutex<HashSet<SubscriptionId>>,
}

impl SubscriptionGuards {
    /// Creates an empty guard set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            in_flight: Mutex::new(HashSet::new()),
        }
    }

    /// Claims the right to update `id`.
    ///
    /// Returns `None` when an update is already in flight, which the caller
    /// should treat as "skip this tick" rather than an error.
    #[must_use]
    pub fn try_begin(&self, id: &SubscriptionId) -> Option<SubscriptionGuard<'_>> {
        // A poisoned lock means another thread panicked while holding it. The
        // set is a simple membership check with no cross-entry invariants, so
        // recovering the data is safe and preferable to poisoning every future
        // update.
        let mut set = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        if set.insert(id.clone()) {
            Some(SubscriptionGuard {
                owner: self,
                id: id.clone(),
            })
        } else {
            None
        }
    }

    /// Whether an update for `id` is currently in flight.
    #[must_use]
    pub fn is_in_flight(&self, id: &SubscriptionId) -> bool {
        let set = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        set.contains(id)
    }
}

impl Default for SubscriptionGuards {
    fn default() -> Self {
        Self::new()
    }
}

/// Releases its subscription claim on drop.
///
/// Drop-based release means an early return or a panic in the update path cannot
/// leave a subscription permanently marked as in flight.
pub struct SubscriptionGuard<'a> {
    owner: &'a SubscriptionGuards,
    id: SubscriptionId,
}

impl Drop for SubscriptionGuard<'_> {
    fn drop(&mut self) {
        let mut set = self
            .owner
            .in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        set.remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(name: &str) -> MihomoInstanceId {
        MihomoInstanceId::parse(name).expect("valid")
    }

    fn subscription(name: &str) -> SubscriptionId {
        SubscriptionId::parse(name).expect("valid")
    }

    #[tokio::test]
    async fn same_instance_is_serialized() {
        let locks = Arc::new(InstanceLocks::new());
        let first = locks.acquire(&instance("default")).await;
        assert!(
            locks.is_locked(&instance("default")).await,
            "the lock should be reported as held"
        );
        drop(first);
        assert!(!locks.is_locked(&instance("default")).await);
    }

    /// Guards against reintroducing the map-lock-across-await deadlock: if the
    /// implementation held the map lock while awaiting an instance lock, two
    /// different instances would block each other.
    #[tokio::test]
    async fn different_instances_do_not_block_each_other() {
        let locks = Arc::new(InstanceLocks::new());
        let guard = locks.acquire(&instance("a")).await;

        // Acquiring "b" must complete promptly even though "a" is held.
        let second = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            locks.acquire(&instance("b")),
        )
        .await;

        assert!(
            second.is_ok(),
            "an unrelated instance must not block acquisition"
        );
        drop(second);
        drop(guard);
    }

    #[tokio::test]
    async fn acquiring_the_same_instance_twice_awaits_release() {
        let locks = Arc::new(InstanceLocks::new());
        let guard = locks.acquire(&instance("default")).await;

        let pending = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            locks.acquire(&instance("default")),
        )
        .await;
        assert!(pending.is_err(), "the second acquisition must wait");

        drop(guard);
        let after = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            locks.acquire(&instance("default")),
        )
        .await;
        assert!(after.is_ok(), "release must unblock the waiter");
    }

    #[test]
    fn concurrent_subscription_updates_are_suppressed() {
        let guards = SubscriptionGuards::new();
        let id = subscription("sub-1");

        let first = guards.try_begin(&id);
        assert!(first.is_some(), "the first update is admitted");
        assert!(
            guards.try_begin(&id).is_none(),
            "a concurrent update must be skipped"
        );
    }

    #[test]
    fn claim_is_released_on_drop() {
        let guards = SubscriptionGuards::new();
        let id = subscription("sub-1");

        {
            let _guard = guards.try_begin(&id).expect("first admitted");
            assert!(guards.is_in_flight(&id));
        }

        assert!(
            !guards.is_in_flight(&id),
            "dropping the guard releases the claim"
        );
        assert!(
            guards.try_begin(&id).is_some(),
            "a later update is admitted"
        );
    }

    /// An early return in the update path must not leak the claim.
    #[test]
    fn claim_is_released_on_early_return() {
        let guards = SubscriptionGuards::new();
        let id = subscription("sub-1");

        fn update(guards: &SubscriptionGuards, id: &SubscriptionId) -> Result<(), ()> {
            let _guard = guards.try_begin(id).ok_or(())?;
            Err(())?; // simulate failing halfway through
            Ok(())
        }

        assert!(update(&guards, &id).is_err());
        assert!(
            !guards.is_in_flight(&id),
            "a failed update must not hold the claim"
        );
    }

    #[test]
    fn different_subscriptions_are_independent() {
        let guards = SubscriptionGuards::new();
        let a = subscription("sub-a");
        let b = subscription("sub-b");

        let _a = guards.try_begin(&a).expect("a admitted");
        assert!(guards.try_begin(&b).is_some(), "b must not be blocked by a");
    }
}
