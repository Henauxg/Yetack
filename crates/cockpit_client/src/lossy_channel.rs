//! A bounded queue whose sender need not wait for the receiver: when the queue is full, the items
//! waiting in it are dropped and the receiver is told.

use tokio::sync::mpsc::error::{SendError, TrySendError};
use tokio::sync::{mpsc, watch};

/// A queue that holds up to `capacity` items.
pub(crate) fn channel<T>(capacity: usize) -> (LossySender<T>, LossyReceiver<T>) {
    let (items_sender, items) = mpsc::channel(capacity);
    let (losses_sender, losses) = watch::channel(0);
    let sender = LossySender {
        items: items_sender,
        losses: losses_sender,
    };
    let receiver = LossyReceiver {
        items,
        losses,
        reported: 0,
        held: None,
    };
    (sender, receiver)
}

/// Each item goes with the number of losses before it was queued, so that the receiver can tell
/// the items queued before a loss.
pub(crate) struct LossySender<T> {
    items: mpsc::Sender<(u64, T)>,
    /// How many times an item found the queue full.
    losses: watch::Sender<u64>,
}

pub(crate) struct LossyReceiver<T> {
    items: mpsc::Receiver<(u64, T)>,
    losses: watch::Receiver<u64>,
    /// The losses reported so far.
    reported: u64,
    /// The first item queued after the last loss reported.
    held: Option<T>,
}

/// What `send_or_drop` did with an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sent {
    Queued,
    /// The queue was full: the receiver drops the items waiting in it and reports the loss.
    Dropped,
    /// The receiver is gone.
    Closed,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Received<T> {
    Item(T),
    /// Items were dropped here.
    Lost,
}

impl<T> LossySender<T> {
    /// Queues `item`, or drops it when the queue is full. Never waits.
    pub(crate) fn send_or_drop(&self, item: T) -> Sent {
        let losses = *self.losses.borrow();
        match self.items.try_send((losses, item)) {
            Ok(()) => Sent::Queued,
            Err(TrySendError::Full(_)) => {
                self.losses.send_modify(|losses| *losses += 1);
                Sent::Dropped
            }
            Err(TrySendError::Closed(_)) => Sent::Closed,
        }
    }

    /// Queues `item` when more than `spare` places stay free, and skips it otherwise. Unlike
    /// `send_or_drop`, a skip drops no item already waiting. Never waits. Returns `false` when the
    /// receiver is gone.
    pub(crate) fn send_if_room(&self, item: T, spare: usize) -> bool {
        if self.items.is_closed() {
            return false;
        }
        if self.items.capacity() <= spare {
            return true;
        }
        let losses = *self.losses.borrow();
        !matches!(
            self.items.try_send((losses, item)),
            Err(TrySendError::Closed(_))
        )
    }

    /// Queues `item`, waiting for room. Fails when the receiver is gone.
    pub(crate) async fn send(&self, item: T) -> Result<(), SendError<T>> {
        let losses = *self.losses.borrow();
        self.items
            .send((losses, item))
            .await
            .map_err(|SendError((_, item))| SendError(item))
    }

    /// Waits until the receiver is gone.
    pub(crate) async fn closed(&self) {
        self.items.closed().await;
    }
}

impl<T> LossyReceiver<T> {
    /// The next item, or `Lost` in place of the items dropped since the last call. `None` once the
    /// sender is gone and every item was received.
    ///
    /// It is cancel safe.
    pub(crate) async fn recv(&mut self) -> Option<Received<T>> {
        if let Some(item) = self.held.take() {
            return Some(Received::Item(item));
        }
        if self.drop_items_before_loss() {
            return Some(Received::Lost);
        }
        let Some((losses, item)) = self.items.recv().await else {
            // The sender can drop an item after the check above, then close.
            return self.drop_items_before_loss().then_some(Received::Lost);
        };
        if losses == self.reported {
            return Some(Received::Item(item));
        }
        // The loss came after the check above, when every item before it was received already.
        self.reported = losses;
        self.held = Some(item);
        Some(Received::Lost)
    }

    /// Returns whether items were dropped since the last loss reported. If so, drops the items
    /// queued before the loss, so that the queue has room again.
    fn drop_items_before_loss(&mut self) -> bool {
        let losses = *self.losses.borrow();
        if losses == self.reported {
            return false;
        }
        self.reported = losses;
        while let Ok((item_losses, item)) = self.items.try_recv() {
            if item_losses >= losses {
                self.reported = item_losses;
                self.held = Some(item);
                break;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_full_queue_drops_the_waiting_items_and_reports_it_once() {
        let (sender, mut receiver) = channel(2);

        let sent = [1, 2, 3, 4].map(|item| sender.send_or_drop(item));
        let lost = receiver.recv().await;
        let queued_after = sender.send_or_drop(5);
        drop(sender);

        assert_eq!(
            sent,
            [Sent::Queued, Sent::Queued, Sent::Dropped, Sent::Dropped]
        );
        assert_eq!(lost, Some(Received::Lost));
        assert_eq!(queued_after, Sent::Queued);
        assert_eq!(receiver.recv().await, Some(Received::Item(5)));
        assert_eq!(receiver.recv().await, None);
    }

    #[tokio::test]
    async fn a_waiting_send_after_a_loss_comes_after_the_report() {
        let (sender, mut receiver) = channel(1);
        sender.send_or_drop(1);
        sender.send_or_drop(2);

        let (sent, lost) = tokio::join!(sender.send(3), receiver.recv());

        sent.unwrap();
        assert_eq!(lost, Some(Received::Lost));
        assert_eq!(receiver.recv().await, Some(Received::Item(3)));
    }

    #[tokio::test]
    async fn an_item_without_room_is_skipped_and_the_waiting_ones_stay() {
        let (sender, mut receiver) = channel(3);

        let sent = [1, 2, 3].map(|item| sender.send_if_room(item, 1));
        drop(sender);

        assert_eq!(sent, [true; 3]);
        assert_eq!(receiver.recv().await, Some(Received::Item(1)));
        assert_eq!(receiver.recv().await, Some(Received::Item(2)));
        assert_eq!(receiver.recv().await, None);
    }

    #[tokio::test]
    async fn a_loss_is_reported_even_after_the_sender_closes() {
        let (sender, mut receiver) = channel(1);
        sender.send_or_drop(1);
        sender.send_or_drop(2);
        drop(sender);

        assert_eq!(receiver.recv().await, Some(Received::Lost));
        assert_eq!(receiver.recv().await, None);
    }
}
