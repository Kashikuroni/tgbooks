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
    pub account: Option<AccountProfile>,
}

/// Display-only identity. Deliberately excludes the phone and redacts Debug.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct AccountProfile {
    pub username: Option<String>,
    pub name: String,
}

impl AccountProfile {
    pub fn label(&self) -> String {
        if let Some(username) = self
            .username
            .as_deref()
            .filter(|name| !name.trim().is_empty())
        {
            format!("@{}", username.trim_start_matches('@'))
        } else if !self.name.trim().is_empty() {
            self.name.clone()
        } else {
            "Личный аккаунт".into()
        }
    }
}

impl fmt::Debug for AccountProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountProfile([REDACTED])")
    }
}

pub trait Authentication: Send {
    /// Verified/cached Telegram user ID, never a phone, credential or auth key.
    /// Adapters without an identity cannot persist or restore download jobs.
    fn account_id(&self) -> Option<i64> {
        None
    }

    fn profile(&self) -> Option<AccountProfile> {
        None
    }

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
        AuthState {
            account: (self.state.step == AuthStep::Authorized)
                .then(|| self.backend.profile())
                .flatten(),
            ..self.state.clone()
        }
    }

    pub fn account_id(&self) -> Option<i64> {
        (self.state.step == AuthStep::Authorized)
            .then(|| self.backend.account_id())
            .flatten()
            .filter(|id| *id > 0)
    }

    pub fn backend_mut(&mut self) -> &mut A {
        &mut self.backend
    }

    pub fn remaining_wait(&self) -> Option<u32> {
        self.retry_at
            .and_then(|time| time.checked_duration_since(Instant::now()))
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| {
                remaining
                    .as_secs()
                    .saturating_add(u64::from(remaining.subsec_nanos() > 0))
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

    #[test]
    fn zero_flood_wait_is_already_expired_even_with_coarse_clock_ticks() {
        let mut auth = AuthController::new(FakeAuth {
            results: VecDeque::new(),
            calls: 0,
        });
        for _ in 0..100 {
            auth.record_flood_wait(0);
            assert_eq!(auth.remaining_wait(), None);
        }
        auth.record_flood_wait(3);
        assert!(auth.remaining_wait().is_some_and(|seconds| seconds <= 3));
    }

    #[test]
    fn account_labels_prefer_username_without_exposing_identity_in_debug() {
        let mut profile = AccountProfile {
            username: Some("reader".into()),
            name: "Иван Иванов".into(),
        };
        assert_eq!(profile.label(), "@reader");
        assert_eq!(format!("{profile:?}"), "AccountProfile([REDACTED])");
        profile.username = None;
        assert_eq!(profile.label(), "Иван Иванов");
        profile.name.clear();
        assert_eq!(profile.label(), "Личный аккаунт");
    }

    #[tokio::test]
    async fn profile_crosses_the_typed_boundary_only_while_authorized() {
        struct ProfileAuth;
        impl Authentication for ProfileAuth {
            fn profile(&self) -> Option<AccountProfile> {
                Some(AccountProfile {
                    username: Some("reader".into()),
                    name: "Reader".into(),
                })
            }
            async fn execute(&mut self, _: AuthAction) -> Result<AuthStep, AuthFailure> {
                Ok(AuthStep::Authorized)
            }
        }
        let mut auth = AuthController::new(ProfileAuth);
        assert!(auth.state().account.is_none());
        let result = auth.execute(AuthAction::Restore).await;
        auth.complete(result);
        assert_eq!(
            auth.state().account.map(|account| account.label()),
            Some("@reader".into())
        );
        auth.complete(Err(AuthFailure {
            kind: AuthError::Network,
            step: AuthStep::Phone,
        }));
        assert!(auth.state().account.is_none());
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
