use tokio::sync::watch;

/// Wait while paused, returns true if cancelled during wait
pub(crate) async fn wait_while_paused(
    pause: &mut watch::Receiver<bool>,
    cancel: &mut watch::Receiver<bool>,
) -> bool {
    while *pause.borrow() {
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
