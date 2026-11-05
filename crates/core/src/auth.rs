//! Authentication protocol and state transitions. No Telegram or platform types.

use std::{
    fmt,
    future::Future,
    time::{Duration, Instant},
};

use zeroize::Zeroizing;

/// An owned, redacted, zeroized-on-drop command payload. Never serialize it.
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

pub enum AuthAction {
    Restore,
    Configure { api_id: i32, api_hash: SecretString },
    RequestCode(SecretString),
    SubmitCode(SecretString),
    SubmitPassword(SecretString),
    Restart,
}

impl fmt::Debug for AuthAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Restore => "Restore",
            Self::Configure { .. } => "Configure([REDACTED])",
            Self::RequestCode(_) => "RequestCode([REDACTED])",
            Self::SubmitCode(_) => "SubmitCode([REDACTED])",
            Self::SubmitPassword(_) => "SubmitPassword([REDACTED])",
            Self::Restart => "Restart",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AuthStep {
    #[default]
    Credentials,
    Phone,
    Code,
    Password,
    Authorized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthError {
    InvalidCredentials,
    InvalidPhone,
    InvalidCode,
    InvalidPassword,
    CodeExpired,
    SignUpRequired,
    FloodWait(u32),
    Network,
    SecureStorage,
    MissingKey,
    CorruptSession,
    AlreadyRunning,
    UnsupportedPlatform,
    UnsupportedReply,
    Rejected,
    UnexpectedState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthFailure {
    pub kind: AuthError,
    pub step: AuthStep,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AuthState {
    pub step: AuthStep,
    pub busy: bool,
    pub error: Option<AuthError>,
}

pub trait Authentication: Send {
    fn execute(
        &mut self,
        action: AuthAction,
    ) -> impl Future<Output = Result<AuthStep, AuthFailure>> + Send;
}

pub(crate) struct AuthController<A> {
    backend: A,
    state: AuthState,
    retry_at: Option<Instant>,
}

impl<A: Authentication> AuthController<A> {
    pub fn new(backend: A) -> Self {
        Self {
            backend,
            state: AuthState::default(),
            retry_at: None,
        }
    }

    pub fn state(&self) -> AuthState {
        self.state.clone()
    }

    pub fn backend_mut(&mut self) -> &mut A {
        &mut self.backend
    }

    pub fn remaining_wait(&self) -> Option<u32> {
        self.retry_at
            .and_then(|time| time.checked_duration_since(Instant::now()))
            .map(|remaining| {
                remaining
                    .as_secs()
                    .saturating_add(1)
                    .min(u64::from(u32::MAX)) as u32
            })
    }

    pub fn record_flood_wait(&mut self, seconds: u32) {
        self.retry_at = Instant::now().checked_add(Duration::from_secs(u64::from(seconds)));
    }

    pub fn prepare(&mut self, action: AuthAction) -> Option<AuthAction> {
        self.state.error = None;
        if let Some(seconds) = self.remaining_wait() {
            self.state.error = Some(AuthError::FloodWait(seconds));
            return None;
        }
        let prepared = match action {
            AuthAction::Restore => Ok(AuthAction::Restore),
            AuthAction::Restart if self.state.step != AuthStep::Authorized => {
                Ok(AuthAction::Restart)
            }
            AuthAction::Configure { api_id, api_hash }
                if self.state.step == AuthStep::Credentials =>
            {
                let hash = api_hash.expose().trim();
                if api_id <= 0
                    || hash.len() != 32
                    || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    Err(AuthError::InvalidCredentials)
                } else {
                    Ok(AuthAction::Configure {
                        api_id,
                        api_hash: SecretString::new(hash.to_owned()),
                    })
                }
            }
            AuthAction::RequestCode(phone) if self.state.step == AuthStep::Phone => {
                let normalized: String = phone
                    .expose()
                    .chars()
                    .filter(|ch| !matches!(ch, ' ' | '-' | '(' | ')'))
                    .collect();
                if !normalized.starts_with('+')
                    || !(7..=15).contains(&normalized[1..].len())
                    || !normalized[1..].bytes().all(|b| b.is_ascii_digit())
                {
                    Err(AuthError::InvalidPhone)
                } else {
                    Ok(AuthAction::RequestCode(SecretString::new(normalized)))
                }
            }
            AuthAction::SubmitCode(code) if self.state.step == AuthStep::Code => {
                let code = code.expose().trim();
                if !(3..=10).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_digit()) {
                    Err(AuthError::InvalidCode)
                } else {
                    Ok(AuthAction::SubmitCode(SecretString::new(code.to_owned())))
                }
            }
            AuthAction::SubmitPassword(password) if self.state.step == AuthStep::Password => {
                if password.expose().is_empty() {
                    Err(AuthError::InvalidPassword)
                } else {
                    Ok(AuthAction::SubmitPassword(password))
                }
            }
            _ => Err(AuthError::UnexpectedState),
        };
        match prepared {
            Ok(action) => {
                self.state.busy = true;
                Some(action)
            }
            Err(error) => {
                self.state.error = Some(error);
                None
            }
        }
    }

    pub async fn execute(&mut self, action: AuthAction) -> Result<AuthStep, AuthFailure> {
        self.backend.execute(action).await
    }

    pub fn complete(&mut self, result: Result<AuthStep, AuthFailure>) {
        self.state.busy = false;
        match result {
            Ok(step) => self.state.step = step,
            Err(failure) => {
                self.state.step = failure.step;
                self.state.error = Some(failure.kind);
                if let AuthError::FloodWait(seconds) = failure.kind {
                    self.record_flood_wait(seconds);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    struct FakeAuth {
        results: VecDeque<Result<AuthStep, AuthFailure>>,
        calls: usize,
    }

    impl Authentication for FakeAuth {
        async fn execute(&mut self, _: AuthAction) -> Result<AuthStep, AuthFailure> {
            self.calls += 1;
            self.results
                .pop_front()
                .unwrap_or(Ok(AuthStep::Credentials))
        }
    }

    fn secret(text: &str) -> SecretString {
        SecretString::new(text.into())
    }

    async fn apply(controller: &mut AuthController<FakeAuth>, action: AuthAction) {
        if let Some(action) = controller.prepare(action) {
            let result = controller.execute(action).await;
            controller.complete(result);
        }
    }

    #[tokio::test]
    async fn credentials_phone_code_and_2fa_form_a_typed_flow() {
        let mut auth = AuthController::new(FakeAuth {
            results: [
                Ok(AuthStep::Phone),
                Ok(AuthStep::Code),
                Ok(AuthStep::Password),
                Ok(AuthStep::Authorized),
            ]
            .into(),
            calls: 0,
        });
        apply(
            &mut auth,
            AuthAction::Configure {
                api_id: 1,
                api_hash: secret(&"0".repeat(32)),
            },
        )
        .await;
        assert_eq!(auth.state().step, AuthStep::Phone);
        apply(
            &mut auth,
            AuthAction::RequestCode(secret("+1 (555) 123-4567")),
        )
        .await;
        assert_eq!(auth.state().step, AuthStep::Code);
        apply(&mut auth, AuthAction::SubmitCode(secret("12345"))).await;
        assert_eq!(auth.state().step, AuthStep::Password);
        apply(
            &mut auth,
            AuthAction::SubmitPassword(secret("test password")),
        )
        .await;
        assert_eq!(auth.state().step, AuthStep::Authorized);
        assert!(!auth.state().busy);
    }

    #[tokio::test]
    async fn invalid_fields_and_invalid_transitions_never_call_telegram() {
        let mut auth = AuthController::new(FakeAuth {
            results: VecDeque::new(),
            calls: 0,
        });
        apply(
            &mut auth,
            AuthAction::Configure {
                api_id: 0,
                api_hash: secret("bad"),
            },
        )
        .await;
        assert_eq!(auth.state().error, Some(AuthError::InvalidCredentials));
        apply(&mut auth, AuthAction::SubmitPassword(secret("password"))).await;
        assert_eq!(auth.backend.calls, 0);
        assert_eq!(auth.state().error, Some(AuthError::UnexpectedState));
    }

    #[tokio::test]
    async fn flood_wait_blocks_repeated_requests_locally() {
        let mut auth = AuthController::new(FakeAuth {
            results: [Err(AuthFailure {
                kind: AuthError::FloodWait(60),
                step: AuthStep::Phone,
            })]
            .into(),
            calls: 0,
        });
        apply(&mut auth, AuthAction::Restore).await;
        apply(&mut auth, AuthAction::RequestCode(secret("+15551234567"))).await;
        assert_eq!(auth.backend.calls, 1);
        assert!(matches!(auth.state().error, Some(AuthError::FloodWait(_))));
        assert!(!auth.state().busy);
    }

    #[test]
    fn authentication_commands_never_debug_credentials_or_challenges() {
        let value = "never print this";
        for action in [
            AuthAction::Configure {
                api_id: 123456,
                api_hash: secret(value),
            },
            AuthAction::RequestCode(secret(value)),
            AuthAction::SubmitCode(secret(value)),
            AuthAction::SubmitPassword(secret(value)),
        ] {
            let debug = format!("{action:?}");
            assert!(!debug.contains(value));
            assert!(!debug.contains("123456"));
        }
    }

    #[tokio::test]
    async fn restoration_does_not_request_a_code_and_revocation_returns_to_phone_entry() {
        for expected in [AuthStep::Authorized, AuthStep::Phone] {
            let mut auth = AuthController::new(FakeAuth {
                results: [Ok(expected)].into(),
                calls: 0,
            });
            apply(&mut auth, AuthAction::Restore).await;
            assert_eq!(auth.state().step, expected);
            assert_eq!(auth.backend.calls, 1);
            assert!(auth.state().error.is_none());
        }
    }

    #[tokio::test]
    async fn incorrect_code_and_password_remain_retryable_on_their_own_steps() {
        let mut auth = AuthController::new(FakeAuth {
            results: [
                Err(AuthFailure {
                    kind: AuthError::InvalidCode,
                    step: AuthStep::Code,
                }),
                Ok(AuthStep::Password),
                Err(AuthFailure {
                    kind: AuthError::InvalidPassword,
                    step: AuthStep::Password,
                }),
                Ok(AuthStep::Authorized),
            ]
            .into(),
            calls: 0,
        });
        auth.state.step = AuthStep::Code;
        apply(&mut auth, AuthAction::SubmitCode(secret("12345"))).await;
        assert_eq!(auth.state().error, Some(AuthError::InvalidCode));
        apply(&mut auth, AuthAction::SubmitCode(secret("54321"))).await;
        assert_eq!(auth.state().step, AuthStep::Password);
        apply(&mut auth, AuthAction::SubmitPassword(secret("wrong"))).await;
        assert_eq!(auth.state().error, Some(AuthError::InvalidPassword));
        apply(&mut auth, AuthAction::SubmitPassword(secret("right"))).await;
        assert_eq!(auth.state().step, AuthStep::Authorized);
        assert!(auth.state().error.is_none());
    }
}
