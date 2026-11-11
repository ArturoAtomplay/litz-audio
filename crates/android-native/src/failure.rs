//! What went wrong, as data. The app chooses and translates the words.
use litz_core::protocol::{Code, close};
use std::error::Error;

#[derive(Clone, Debug, PartialEq, uniffi::Error)]
pub enum Failure {
    Cancelled,
    ConnectTimeout,
    ControlTimeout,
    AudioDiscontinuity,
    ConnectionLost,
    Revoked,
    Moved,
    /// The computer refused every enabled direction; each direction says why.
    Refused,
    ComputerRefused {
        reason: Code,
    },
    UnexpectedReply,
    ProtocolMismatch,
    IdentityChanged,
    InUse {
        owner: Option<String>,
    },
    OutputsChanged,
    NotConnected,
    AudioUnavailable,
    AudioDeviceDisconnected,
    RouteDisconnected,
    PairingUnavailable,
    PairingDeclined,
    PairingFull,
    PromptUnavailable,
    Internal {
        detail: String,
    },
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Internal { detail } => f.write_str(detail),
            other => write!(f, "{other:?}"),
        }
    }
}

impl Error for Failure {}

pub fn cause<'a, T: Error + 'static>(error: &'a (dyn Error + 'static)) -> Option<&'a T> {
    std::iter::successors(Some(error), |&e| e.source()).find_map(|e| e.downcast_ref())
}

impl Failure {
    pub fn of(error: &(dyn Error + 'static)) -> Self {
        if let Some(failure) = cause::<Failure>(error) {
            return failure.clone();
        }
        if let Some(closed) = cause::<quinn::ConnectionError>(error) {
            return match closed {
                quinn::ConnectionError::ApplicationClosed(reason)
                    if reason.error_code == close::FORGOTTEN =>
                {
                    Self::Revoked
                }
                quinn::ConnectionError::ApplicationClosed(reason)
                    if reason.error_code == close::MOVED =>
                {
                    Self::Moved
                }
                quinn::ConnectionError::ApplicationClosed(_)
                | quinn::ConnectionError::ConnectionClosed(_)
                | quinn::ConnectionError::Reset
                | quinn::ConnectionError::TimedOut
                | quinn::ConnectionError::LocallyClosed => Self::ConnectionLost,
                other => Self::Internal {
                    detail: other.to_string(),
                },
            };
        }
        if cause::<tokio::time::error::Elapsed>(error).is_some() {
            return Self::ControlTimeout;
        }
        Self::Internal {
            detail: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_errors_become_codes_and_the_rest_keeps_its_detail() {
        let closed = |code: u32| {
            quinn::ReadError::ConnectionLost(quinn::ConnectionError::ApplicationClosed(
                quinn::ApplicationClose {
                    error_code: code.into(),
                    reason: b"".as_slice().into(),
                },
            ))
        };
        assert_eq!(Failure::of(&closed(0x21)), Failure::Revoked);
        assert_eq!(Failure::of(&closed(0x20)), Failure::Moved);
        assert_eq!(Failure::of(&closed(1)), Failure::ConnectionLost);
        let other: Box<dyn Error> = "epoch exhausted".into();
        assert_eq!(
            Failure::of(other.as_ref()),
            Failure::Internal {
                detail: "epoch exhausted".into()
            }
        );
    }
}
