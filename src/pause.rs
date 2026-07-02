use tokio::sync::watch;

/// `watch::Receiver::has_changed` returns `Err(RecvError::Closed)` once all
/// senders have dropped. It returns `Ok(_)` (either `true` or `false`) while at
/// least one sender is alive, so callers that only care about channel closure
/// should ignore the inner boolean and just check `is_ok()`.
pub(crate) fn channel_is_open(pause: &watch::Receiver<bool>) -> bool {
    pause.has_changed().is_ok()
}

/// Returns `true` when the stream is currently paused.
///
/// Once the pause sender has been dropped, the `watch` channel is closed.
/// `*pause.borrow()` would keep reporting the last value forever (possibly
/// `true`), which would make any caller that keeps checking the receiver
/// busy-loop because `pause.changed()` becomes immediately ready. Treat a
/// closed pause channel as "not paused" so the stream resumes and is governed
/// by cancel/deadline instead.
pub(crate) fn is_paused(pause: &watch::Receiver<bool>) -> bool {
    *pause.borrow() && channel_is_open(pause)
}

/// Wait while paused, returns true if cancelled during wait.
///
/// If the pause sender is dropped while paused, this treats the stream as
/// resumed (returns `false`) rather than spinning forever, matching the
/// `is_paused` semantics.
pub(crate) async fn wait_while_paused(
    pause: &mut watch::Receiver<bool>,
    cancel: &mut watch::Receiver<bool>,
) -> bool {
    while is_paused(pause) {
        tokio::select! {
            biased;
            result = cancel.changed() => {
                if result.is_err() || *cancel.borrow() { return true; }
            }
            result = pause.changed() => {
                if result.is_err() { return false; }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn is_paused_treats_closed_channel_as_not_paused() {
        let (tx, rx) = watch::channel(false);
        assert!(!is_paused(&rx));

        tx.send(true).unwrap();
        assert!(is_paused(&rx));

        drop(tx);
        assert!(
            !is_paused(&rx),
            "closed pause channel should be treated as not paused"
        );
    }

    #[tokio::test]
    async fn channel_is_open_ignores_observed_change_state() {
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).unwrap();
        rx.changed().await.unwrap();

        assert!(
            !rx.has_changed().unwrap(),
            "test setup should mark the latest value as observed"
        );
        assert!(
            channel_is_open(&rx),
            "Ok(false) means open with no unseen value, not closed"
        );
    }

    #[tokio::test]
    async fn wait_while_paused_returns_when_sender_dropped() {
        let (pause_tx, mut pause_rx) = watch::channel(true);
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);

        let task =
            tokio::spawn(async move { wait_while_paused(&mut pause_rx, &mut cancel_rx).await });

        // Give the task a chance to start; with a paused=true + open sender
        // it should still be blocked.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!task.is_finished());

        // Dropping the sender must make the wait return promptly and without
        // treating the event as a cancellation.
        drop(pause_tx);
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("wait_while_paused should not hang after sender is dropped")
            .expect("spawned task should not panic");
        assert!(
            !result,
            "dropped pause sender should be treated as resumed, not cancelled"
        );
    }
}
