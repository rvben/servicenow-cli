//! Races network requests against the operator's cancel key (Esc / Ctrl-C) so
//! a blocking `.await` never leaves the terminal unresponsive.

use std::future::Future;
use std::time::Duration;

#[cfg(not(test))]
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::api::ApiError;

use super::actions::ActionPrepError;

/// Outcome of racing a network call against the operator's cancel key.
pub(super) enum RequestOutcome<T> {
    Completed(T),
    Cancelled,
}

/// Awaits `fut` unless `cancel` resolves first. Cancelling drops `fut`
/// immediately, aborting whatever request it held; for a write that may
/// already have reached the server, the caller must treat the outcome as
/// unknown rather than as success or failure.
pub(super) async fn run_cancellable<T>(
    fut: impl Future<Output = T>,
    cancel: impl Future<Output = ()>,
) -> RequestOutcome<T> {
    tokio::select! {
        result = fut => RequestOutcome::Completed(result),
        () = cancel => RequestOutcome::Cancelled,
    }
}

/// Awaits an API call, mapping a cancel to `ActionPrepError::Cancelled` so
/// preparation steps can keep using `?`.
pub(super) async fn cancellable_api<T>(
    fut: impl Future<Output = Result<T, ApiError>>,
) -> Result<T, ActionPrepError> {
    match run_cancellable(fut, wait_for_cancel_key()).await {
        RequestOutcome::Completed(result) => Ok(result?),
        RequestOutcome::Cancelled => Err(ActionPrepError::Cancelled),
    }
}

#[cfg(not(test))]
fn is_cancel_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Esc
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// Waits for the operator to press Esc or Ctrl-C, polling the terminal from a
/// blocking task so the async runtime stays responsive while a request is in
/// flight. Raced against a network call via `run_cancellable`.
///
/// The blocking task only peeks for input and the key is read here instead.
/// A blocking task cannot be stopped once started, so if it read keys itself,
/// one still polling after the request completed would swallow the next key
/// meant for the main loop.
///
/// Every other key pressed while the request runs is read and discarded
/// rather than replayed afterwards: the request usually replaces the rows and
/// selection those keys were aimed at, so replaying them could move to or act
/// on a different record than the operator saw.
#[cfg(not(test))]
pub(super) async fn wait_for_cancel_key() {
    loop {
        let ready =
            tokio::task::spawn_blocking(|| event::poll(Duration::from_millis(50)).unwrap_or(false))
                .await
                .unwrap_or(false);
        if ready
            && let Ok(Event::Key(key)) = event::read()
            && key.kind == KeyEventKind::Press
            && is_cancel_key(key)
        {
            return;
        }
    }
}

// Real terminal polling can't run against the fake stdin in `cargo test`, so
// tests arm a delay through `arm_test_cancel` and this resolves after it
// elapses; with no delay armed it never resolves, matching "no cancel key
// pressed" for every test that does not opt in.
#[cfg(test)]
thread_local! {
    static TEST_CANCEL_DELAY: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) struct TestCancelGuard;

#[cfg(test)]
impl Drop for TestCancelGuard {
    fn drop(&mut self) {
        TEST_CANCEL_DELAY.with(|cell| cell.set(None));
    }
}

#[cfg(test)]
#[must_use]
pub(super) fn arm_test_cancel(delay: Duration) -> TestCancelGuard {
    TEST_CANCEL_DELAY.with(|cell| cell.set(Some(delay)));
    TestCancelGuard
}

#[cfg(test)]
pub(super) async fn wait_for_cancel_key() {
    let delay = TEST_CANCEL_DELAY.with(std::cell::Cell::get);
    match delay {
        Some(delay) => tokio::time::sleep(delay).await,
        None => std::future::pending().await,
    }
}
