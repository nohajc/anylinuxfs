#![allow(unused)]
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

/// Generic publish / subscribe hub backed by std::sync::mpsc channels.
///
/// Characteristics:
/// - Thread safe (internal mutex protects subscriber map)
/// - Cloneable handle (Arc internally)
/// - Each subscriber gets its own Receiver<T>
/// - Messages are cloned for each active subscriber (T: Clone)
/// - Dropped / disconnected subscribers are cleaned up lazily on publish
/// - Optional automatic unsubscription via the Subscription guard (Drop)
pub struct PubSub<T: Clone + Send + 'static> {
    inner: Arc<PubSubInner<T>>,
}

struct PubSubInner<T: Clone + Send + 'static> {
    subscribers: Mutex<HashMap<usize, mpsc::Sender<T>>>,
    next_id: AtomicUsize,
}

impl<T: Clone + Send + 'static> Clone for PubSub<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Clone + Send + 'static> PubSub<T> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(PubSubInner {
                subscribers: Mutex::new(HashMap::new()),
                next_id: AtomicUsize::new(0),
            }),
        }
    }

    /// Subscribe to messages. Returns a Subscription guard holding the Receiver.
    /// Dropping the guard (or calling unsubscribe on it) removes the subscriber.
    pub fn subscribe(&self) -> Subscription<T> {
        let (tx, rx) = mpsc::channel();
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner.subscribers.lock().unwrap().insert(id, tx);
        Subscription {
            hub: self.clone(),
            id: Some(id),
            rx,
        }
    }

    /// Publish a message to all current subscribers.
    /// Disconnected subscribers are removed.
    pub fn publish(&self, msg: T) {
        let mut subs = self.inner.subscribers.lock().unwrap();
        subs.retain(|_, tx| tx.send(msg.clone()).is_ok());
    }

    /// Explicitly unsubscribe a subscriber id (normally handled by Subscription::drop).
    pub fn unsubscribe(&self, id: usize) {
        self.inner.subscribers.lock().unwrap().remove(&id);
    }

    /// Current number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.inner.subscribers.lock().unwrap().len()
    }
}

/// RAII guard representing a subscription (also owns the Receiver).
pub struct Subscription<T: Clone + Send + 'static> {
    hub: PubSub<T>,
    id: Option<usize>,
    rx: mpsc::Receiver<T>,
}

impl<T: Clone + Send + 'static> Subscription<T> {
    /// Manually unsubscribe (optional). Safe to call multiple times.
    pub fn unsubscribe(&mut self) {
        if let Some(id) = self.id.take() {
            self.hub.unsubscribe(id);
        }
    }
    /// Blocking receive.
    pub fn recv(&self) -> Result<T, mpsc::RecvError> {
        self.rx.recv()
    }
    /// Non-blocking receive.
    pub fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        self.rx.try_recv()
    }
    /// Access underlying receiver (e.g. for select-like patterns).
    pub fn receiver(&self) -> &mpsc::Receiver<T> {
        &self.rx
    }
    /// Return the subscriber id (None after unsubscribe).
    pub fn id(&self) -> Option<usize> {
        self.id
    }
}

impl<T: Clone + Send + 'static> Iterator for Subscription<T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        match self.rx.recv() {
            Ok(v) => Some(v),
            Err(_) => None,
        }
    }
}

impl<T: Clone + Send + 'static> Drop for Subscription<T> {
    fn drop(&mut self) {
        self.unsubscribe();
    }
}

impl<T: Clone + Send + 'static> fmt::Debug for Subscription<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription")
            .field("active", &self.id.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Duration};

    #[test]
    fn basic_pub_sub() {
        let hub: PubSub<String> = PubSub::new();
        let mut sub1 = hub.subscribe();
        let sub2 = hub.subscribe();
        assert_eq!(hub.subscriber_count(), 2);
        hub.publish("hello".to_string());
        assert_eq!(sub1.recv().unwrap(), "hello");
        assert_eq!(sub2.recv().unwrap(), "hello");
        sub1.unsubscribe();
        assert_eq!(hub.subscriber_count(), 1);
        hub.publish("world".to_string());
        assert_eq!(sub2.recv().unwrap(), "world");
    }

    #[test]
    fn dropped_receiver_is_cleaned() {
        let hub: PubSub<u32> = PubSub::new();
        let sub = hub.subscribe();
        // drop subscription (and receiver)
        drop(sub);
        hub.publish(10); // triggers cleanup
        assert_eq!(hub.subscriber_count(), 0);
    }

    #[test]
    fn multi_thread() {
        let hub: PubSub<usize> = PubSub::new();
        let sub1 = hub.subscribe();
        let sub2 = hub.subscribe();
        let hub_clone = hub.clone();
        let h = thread::spawn(move || {
            for i in 0..5 {
                hub_clone.publish(i);
                thread::sleep(Duration::from_millis(5));
            }
        });
        let collected1: Vec<_> = (0..5).map(|_| sub1.recv().unwrap()).collect();
        let collected2: Vec<_> = (0..5).map(|_| sub2.recv().unwrap()).collect();
        h.join().unwrap();
        assert_eq!(collected1, vec![0, 1, 2, 3, 4]);
        assert_eq!(collected2, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn iterator_over_subscription() {
        let hub: PubSub<usize> = PubSub::new();
        let mut sub = hub.subscribe();
        let id = sub.id().unwrap();
        let hub_clone = hub.clone();
        thread::spawn(move || {
            for i in 0..5 {
                hub_clone.publish(i);
            }
            // Close the channel by removing the sender
            hub_clone.unsubscribe(id);
        });
        let received: Vec<_> = sub.collect();
        assert_eq!(received, vec![0, 1, 2, 3, 4]);
        assert_eq!(hub.subscriber_count(), 0);
    }
}
