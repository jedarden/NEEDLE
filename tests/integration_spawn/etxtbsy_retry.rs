//! Simulation tests for the ETXTBSY retry wrappers.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use needle::bead_store::{
    spawn_with_etxtbsy_retry, spawn_with_etxtbsy_retry_exponential, spawn_with_etxtbsy_retry_sync,
    spawn_with_etxtbsy_retry_sync_exponential,
};

fn etxtbsy_error() -> io::Error {
    io::Error::from_raw_os_error(26)
}

#[tokio::test]
async fn first_attempt_success_does_not_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result = spawn_with_etxtbsy_retry(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok::<_, io::Error>(b"success".to_vec())
            }
        },
        4,
        0,
    )
    .await;

    assert!(matches!(result, Ok(ref output) if output.as_slice() == b"success"));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn etxtbsy_error_retries_and_succeeds() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result = spawn_with_etxtbsy_retry(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            async move {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"recovered".to_vec())
                }
            }
        },
        4,
        0,
    )
    .await;

    assert!(matches!(result, Ok(ref output) if output.as_slice() == b"recovered"));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn etxtbsy_error_is_returned_after_max_attempts() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result: io::Result<Vec<u8>> = spawn_with_etxtbsy_retry(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<Vec<u8>, _>(etxtbsy_error())
            }
        },
        3,
        0,
    )
    .await;

    let errno = match result {
        Ok(_) => None,
        Err(error) => error.raw_os_error(),
    };
    assert_eq!(errno, Some(26));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn non_etxtbsy_error_propagates_without_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result: io::Result<Vec<u8>> = spawn_with_etxtbsy_retry(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<Vec<u8>, _>(io::Error::new(io::ErrorKind::NotFound, "missing"))
            }
        },
        4,
        0,
    )
    .await;

    let error_kind = match result {
        Ok(_) => None,
        Err(error) => Some(error.kind()),
    };
    assert_eq!(error_kind, Some(io::ErrorKind::NotFound));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

// ========== Exponential Backoff Tests ==========

#[tokio::test]
async fn exponential_first_attempt_success_does_not_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result = spawn_with_etxtbsy_retry_exponential(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok::<_, io::Error>(b"success".to_vec())
            }
        },
        5,
        10,
    )
    .await;

    assert!(matches!(result, Ok(ref output) if output.as_slice() == b"success"));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn exponential_etxtbsy_error_retries_and_succeeds() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result = spawn_with_etxtbsy_retry_exponential(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            async move {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(etxtbsy_error())
                } else {
                    Ok::<_, io::Error>(b"recovered".to_vec())
                }
            }
        },
        5,
        10,
    )
    .await;

    assert!(matches!(result, Ok(ref output) if output.as_slice() == b"recovered"));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn exponential_etxtbsy_error_is_returned_after_max_attempts() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result: io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_exponential(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<Vec<u8>, _>(etxtbsy_error())
            }
        },
        3,
        10,
    )
    .await;

    let errno = match result {
        Ok(_) => None,
        Err(error) => error.raw_os_error(),
    };
    assert_eq!(errno, Some(26));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn exponential_non_etxtbsy_error_propagates_without_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result: io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_exponential(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<Vec<u8>, _>(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            }
        },
        5,
        10,
    )
    .await;

    let error_kind = match result {
        Ok(_) => None,
        Err(error) => Some(error.kind()),
    };
    assert_eq!(error_kind, Some(io::ErrorKind::PermissionDenied));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

// ========== Sync Linear Backoff Tests ==========

#[test]
fn sync_first_attempt_success_does_not_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result = spawn_with_etxtbsy_retry_sync(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok::<_, io::Error>(b"success".to_vec())
        },
        4,
        0,
    );

    assert!(matches!(result, Ok(ref output) if output.as_slice() == b"success"));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn sync_etxtbsy_error_retries_and_succeeds() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result = spawn_with_etxtbsy_retry_sync(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(etxtbsy_error())
            } else {
                Ok::<_, io::Error>(b"recovered".to_vec())
            }
        },
        4,
        0,
    );

    assert!(matches!(result, Ok(ref output) if output.as_slice() == b"recovered"));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn sync_etxtbsy_error_is_returned_after_max_attempts() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result: io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<Vec<u8>, _>(etxtbsy_error())
        },
        3,
        0,
    );

    let errno = match result {
        Ok(_) => None,
        Err(error) => error.raw_os_error(),
    };
    assert_eq!(errno, Some(26));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn sync_non_etxtbsy_error_propagates_without_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result: io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<Vec<u8>, _>(io::Error::new(io::ErrorKind::BrokenPipe, "broken"))
        },
        4,
        0,
    );

    let error_kind = match result {
        Ok(_) => None,
        Err(error) => Some(error.kind()),
    };
    assert_eq!(error_kind, Some(io::ErrorKind::BrokenPipe));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

// ========== Sync Exponential Backoff Tests ==========

#[test]
fn sync_exponential_first_attempt_success_does_not_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result = spawn_with_etxtbsy_retry_sync_exponential(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok::<_, io::Error>(b"success".to_vec())
        },
        5,
        10,
    );

    assert!(matches!(result, Ok(ref output) if output.as_slice() == b"success"));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn sync_exponential_etxtbsy_error_retries_and_succeeds() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result = spawn_with_etxtbsy_retry_sync_exponential(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(etxtbsy_error())
            } else {
                Ok::<_, io::Error>(b"recovered".to_vec())
            }
        },
        5,
        10,
    );

    assert!(matches!(result, Ok(ref output) if output.as_slice() == b"recovered"));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn sync_exponential_etxtbsy_error_is_returned_after_max_attempts() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result: io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync_exponential(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<Vec<u8>, _>(etxtbsy_error())
        },
        3,
        10,
    );

    let errno = match result {
        Ok(_) => None,
        Err(error) => error.raw_os_error(),
    };
    assert_eq!(errno, Some(26));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn sync_exponential_non_etxtbsy_error_propagates_without_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_spawn = Arc::clone(&attempts);

    let result: io::Result<Vec<u8>> = spawn_with_etxtbsy_retry_sync_exponential(
        || {
            let attempts = Arc::clone(&attempts_for_spawn);
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<Vec<u8>, _>(io::Error::new(io::ErrorKind::AddrInUse, "in use"))
        },
        5,
        10,
    );

    let error_kind = match result {
        Ok(_) => None,
        Err(error) => Some(error.kind()),
    };
    assert_eq!(error_kind, Some(io::ErrorKind::AddrInUse));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}
