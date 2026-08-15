//! Simulation tests for the ETXTBSY retry wrappers.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use needle::bead_store::spawn_with_etxtbsy_retry;

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
