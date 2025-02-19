use crate::{
    graph::{
        AnySource, AnySubscriber, Observer, ReactiveNode, ReactiveNodeState,
        Source, SourceSet, Subscriber, SubscriberSet, WithObserver,
    },
    owner::{Owner, Storage, StorageAccess},
};
use or_poisoned::OrPoisoned;
use std::{
    fmt::Debug,
    sync::{Arc, RwLock},
};

pub struct MemoInner<T, S>
where
    S: Storage<T>,
{
    pub(crate) value: Option<S::Wrapped>,
    #[allow(clippy::type_complexity)]
    pub(crate) fun: Arc<dyn Fn(Option<T>) -> (T, bool) + Send + Sync>,
    pub(crate) owner: Owner,
    pub(crate) state: ReactiveNodeState,
    pub(crate) sources: SourceSet,
    pub(crate) subscribers: SubscriberSet,
    pub(crate) any_subscriber: AnySubscriber,
}

impl<T, S> Debug for MemoInner<T, S>
where
    S: Storage<T>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoInner").finish_non_exhaustive()
    }
}

impl<T: 'static, S> MemoInner<T, S>
where
    S: Storage<T>,
{
    #[allow(clippy::type_complexity)]
    pub fn new(
        fun: Arc<dyn Fn(Option<T>) -> (T, bool) + Send + Sync>,
        any_subscriber: AnySubscriber,
    ) -> Self {
        Self {
            value: None,
            fun,
            owner: Owner::new(),
            state: ReactiveNodeState::Dirty,
            sources: Default::default(),
            subscribers: SubscriberSet::new(),
            any_subscriber,
        }
    }
}

impl<T: 'static, S> ReactiveNode for RwLock<MemoInner<T, S>>
where
    S: Storage<T>,
{
    fn mark_dirty(&self) {
        self.write().or_poisoned().state = ReactiveNodeState::Dirty;
        self.mark_subscribers_check();
    }

    fn mark_check(&self) {
        {
            let mut lock = self.write().or_poisoned();
            if lock.state != ReactiveNodeState::Dirty {
                lock.state = ReactiveNodeState::Check;
            }
        }
        for sub in (&self.read().or_poisoned().subscribers).into_iter() {
            sub.mark_check();
        }
    }

    fn mark_subscribers_check(&self) {
        let lock = self.read().or_poisoned();
        for sub in (&lock.subscribers).into_iter() {
            sub.mark_check();
        }
    }

    fn update_if_necessary(&self) -> bool {
        let (state, sources) = {
            let inner = self.read().or_poisoned();
            (inner.state, inner.sources.clone())
        };

        let needs_update = match state {
            ReactiveNodeState::Clean => false,
            ReactiveNodeState::Dirty => true,
            ReactiveNodeState::Check => (&sources).into_iter().any(|source| {
                source.update_if_necessary()
                    || self.read().or_poisoned().state
                        == ReactiveNodeState::Dirty
            }),
        };

        if needs_update {
            let (fun, value, owner) = {
                let mut lock = self.write().or_poisoned();
                (lock.fun.clone(), lock.value.take(), lock.owner.clone())
            };

            let any_subscriber =
                { self.read().or_poisoned().any_subscriber.clone() };
            any_subscriber.clear_sources(&any_subscriber);
            let (new_value, changed) = owner.with_cleanup(|| {
                any_subscriber
                    .with_observer(|| fun(value.map(StorageAccess::into_taken)))
            });

            let mut lock = self.write().or_poisoned();
            lock.value = Some(S::wrap(new_value));
            lock.state = ReactiveNodeState::Clean;

            if changed {
                let subs = lock.subscribers.clone();
                drop(lock);
                for sub in subs {
                    // don't trigger reruns of effects/memos
                    // basically: if one of the observers has triggered this memo to
                    // run, it doesn't need to be re-triggered because of this change
                    if !Observer::is(&sub) {
                        sub.mark_dirty();
                    }
                }
            }

            changed
        } else {
            if let Ok(mut lock) = self.try_write() {
                lock.state = ReactiveNodeState::Clean;
            }
            false
        }
    }
}

impl<T: 'static, S> Source for RwLock<MemoInner<T, S>>
where
    S: Storage<T>,
{
    fn add_subscriber(&self, subscriber: AnySubscriber) {
        if let Ok(mut lock) = self.try_write() {
            lock.subscribers.subscribe(subscriber);
        }
    }

    fn remove_subscriber(&self, subscriber: &AnySubscriber) {
        self.write()
            .or_poisoned()
            .subscribers
            .unsubscribe(subscriber);
    }

    fn clear_subscribers(&self) {
        self.write().or_poisoned().subscribers.take();
    }
}

impl<T: 'static, S> Subscriber for RwLock<MemoInner<T, S>>
where
    S: Storage<T>,
{
    fn add_source(&self, source: AnySource) {
        self.write().or_poisoned().sources.insert(source);
    }

    fn clear_sources(&self, subscriber: &AnySubscriber) {
        self.write().or_poisoned().sources.clear_sources(subscriber);
    }
}
