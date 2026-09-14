//! Frame accounting and bounded retries for nonblocking PCM writes.
use crate::backend::DirectWriteError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(crate) fn write_complete(
    frames: usize,
    cancel: &AtomicBool,
    stall_limit: Duration,
    mut write: impl FnMut(usize) -> Result<usize, i32>,
    mut recover: impl FnMut(i32) -> Result<bool, String>,
    mut wait: impl FnMut() -> Result<(), String>,
) -> Result<usize, DirectWriteError> {
    let mut written = 0;
    let mut progress = Instant::now();
    while written < frames {
        let canceled = cancel.load(Ordering::Acquire);
        if canceled || progress.elapsed() >= stall_limit {
            return Err(DirectWriteError {
                frames_written: written,
                canceled,
                message: if canceled {
                    "PCM write canceled"
                } else {
                    "PCM made no progress before deadline"
                }
                .into(),
            });
        }
        let result = write(written);
        let retry_wait = match result {
            Ok(count) if count > frames - written => {
                Err("PCM returned more frames than supplied".into())
            }
            Ok(0) => Ok(true),
            Ok(count) => {
                written += count;
                progress = Instant::now();
                Ok(false)
            }
            Err(libc::EINTR) => Ok(false),
            Err(libc::EAGAIN) => Ok(true),
            Err(error) => recover(error).map(|ready| !ready),
        };
        if let Err(message) = retry_wait.and_then(|needed| if needed { wait() } else { Ok(()) }) {
            return Err(DirectWriteError {
                frames_written: written,
                canceled: false,
                message,
            });
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn retries_keep_the_unwritten_suffix_and_account_in_frames() {
        let cancel = AtomicBool::new(false);
        let mut script = VecDeque::from([
            Ok(2),
            Err(libc::EINTR),
            Err(libc::EAGAIN),
            Ok(0),
            Err(libc::EPIPE),
            Ok(3),
        ]);
        let mut offsets = Vec::new();
        let mut recoveries = Vec::new();
        let result = write_complete(
            5,
            &cancel,
            Duration::from_secs(1),
            |offset| {
                offsets.push(offset);
                script.pop_front().unwrap()
            },
            |error| {
                recoveries.push(error);
                Ok(true)
            },
            || Ok(()),
        );
        assert_eq!(result.unwrap(), 5);
        assert_eq!(offsets, [0, 2, 2, 2, 2, 2]);
        assert_eq!(recoveries, [libc::EPIPE]);
    }

    #[test]
    fn stop_during_wait_and_device_failure_preserve_progress() {
        let cancel = AtomicBool::new(false);
        let mut script = VecDeque::from([Ok(2), Err(libc::EAGAIN)]);
        let error = write_complete(
            5,
            &cancel,
            Duration::from_secs(1),
            |_| script.pop_front().unwrap(),
            |_| unreachable!(),
            || {
                cancel.store(true, Ordering::Release);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.canceled);
        assert_eq!(error.frames_written, 2);
        cancel.store(false, Ordering::Release);
        let mut script = VecDeque::from([Ok(2), Err(libc::ENODEV)]);
        let error = write_complete(
            5,
            &cancel,
            Duration::from_secs(1),
            |_| script.pop_front().unwrap(),
            |_| Err("disconnected".into()),
            || unreachable!(),
        )
        .unwrap_err();
        assert!(!error.canceled);
        assert_eq!(error.frames_written, 2);
    }

    #[test]
    fn zero_progress_has_a_deadline_and_overreported_frames_fail() {
        let cancel = AtomicBool::new(false);
        assert!(write_complete(
            1,
            &cancel,
            Duration::ZERO,
            |_| panic!("expired write must not touch device"),
            |_| unreachable!(),
            || unreachable!()
        )
        .is_err());
        assert!(write_complete(
            1,
            &cancel,
            Duration::from_secs(1),
            |_| Ok(2),
            |_| unreachable!(),
            || unreachable!()
        )
        .is_err());
    }
}
