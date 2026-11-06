use std::{
    collections::HashMap, future::Future, panic::AssertUnwindSafe, path::PathBuf, sync::Arc,
    time::Duration,
};

use futures_util::FutureExt as _;
use grammers_client::{
    InvocationError, SignInError,
    client::{LoginToken, PasswordToken},
};
use tgbooks_core::{AccountProfile, AuthAction, AuthError, AuthFailure, AuthStep, Authentication};

use crate::{
    ApiCredentials, TelegramConnection,
    session::SecureSession,
    vault::{Vault, VaultError},
};

/// One owner for tokens, connection and session. No secrets are Debug/Serialize.
pub struct TelegramAuth {
    directory: PathBuf,
    session: Option<Arc<SecureSession>>,
    connection: Option<TelegramConnection>,
    login: Option<LoginToken>,
    password: Option<PasswordToken>,
    step: AuthStep,
    profile: Option<AccountProfile>,
    pub(crate) channels: HashMap<i64, grammers_session::types::PeerRef>,
}

impl TelegramAuth {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            session: None,
            connection: None,
            login: None,
            password: None,
            step: AuthStep::Credentials,
            profile: None,
            channels: HashMap::new(),
        }
    }

    async fn disconnect(&mut self) {
        if let Some(mut connection) = self.connection.take() {
            connection.stop().await;
        }
        self.login = None;
        self.password = None;
        self.profile = None;
        self.channels.clear();
    }

    fn connect(&mut self) -> Result<(), AuthFailure> {
        let session = self
            .session
            .as_ref()
            .ok_or(failure(AuthError::UnexpectedState, self.step))?;
        let credentials = session
            .credentials()
            .map_err(|error| storage_failure(error, self.step))?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| failure(AuthError::Network, self.step))?;
        self.connection = Some(TelegramConnection::start(
            session.clone(),
            &credentials,
            &runtime,
        ));
        Ok(())
    }

    async fn perform(&mut self, action: AuthAction) -> Result<AuthStep, AuthFailure> {
        match action {
            AuthAction::Restore => {
                self.disconnect().await;
                self.session = None;
                let directory = self.directory.clone();
                self.session = tokio::task::spawn_blocking(move || {
                    SecureSession::restore(Vault::open_system(&directory)?)
                })
                .await
                .map_err(|_| failure(AuthError::SecureStorage, AuthStep::Credentials))?
                .map_err(|error| storage_failure(error, AuthStep::Credentials))?;
                if self.session.is_none() {
                    return Ok(AuthStep::Credentials);
                }
                self.connect()?;
                let client = self.client()?;
                // Validate the session and get the display identity in one RPC.
                let user = timed(client.get_me())
                    .await
                    .map_err(|kind| failure(kind, AuthStep::Phone))?;
                match user {
                    Ok(user) => {
                        self.profile = Some(profile(&user));
                        return Ok(AuthStep::Authorized);
                    }
                    Err(InvocationError::Rpc(error)) if error.code == 401 => {}
                    Err(error) => return Err(rpc_failure(error, AuthStep::Phone)),
                }
                {
                    // Revoked keys must not be reused for a new login handshake.
                    self.disconnect().await;
                    let session = self
                        .session
                        .as_ref()
                        .ok_or(failure(AuthError::UnexpectedState, AuthStep::Credentials))?;
                    session
                        .reset_authorization()
                        .await
                        .map_err(|error| storage_failure(error, AuthStep::Phone))?;
                    self.connect()?;
                    Ok(AuthStep::Phone)
                }
            }
            AuthAction::Configure { api_id, api_hash } => {
                self.disconnect().await;
                self.session = None;
                let credentials = ApiCredentials::new(api_id, api_hash.expose().to_owned())
                    .map_err(|_| failure(AuthError::InvalidCredentials, AuthStep::Credentials))?;
                let directory = self.directory.clone();
                self.session = Some(
                    tokio::task::spawn_blocking(move || {
                        SecureSession::configure(Vault::open_system(&directory)?, credentials)
                    })
                    .await
                    .map_err(|_| failure(AuthError::SecureStorage, AuthStep::Credentials))?
                    .map_err(|error| storage_failure(error, AuthStep::Credentials))?,
                );
                self.connect()?;
                Ok(AuthStep::Phone)
            }
            AuthAction::RequestCode(phone) => {
                if self.connection.is_none() {
                    self.connect()?;
                }
                let client = self.client()?;
                let credentials = self
                    .session
                    .as_ref()
                    .ok_or(failure(AuthError::UnexpectedState, AuthStep::Credentials))?
                    .credentials()
                    .map_err(|error| storage_failure(error, AuthStep::Phone))?;
                let token =
                    timed(client.request_login_code(phone.expose(), credentials.api_hash()))
                        .await
                        .map_err(|kind| failure(kind, AuthStep::Phone))?
                        .map_err(|error| rpc_failure(error, AuthStep::Phone))?;
                self.login = Some(token);
                self.password = None;
                Ok(AuthStep::Code)
            }
            AuthAction::SubmitCode(code) => {
                let client = self.client()?;
                let token = self
                    .login
                    .as_ref()
                    .ok_or(failure(AuthError::UnexpectedState, AuthStep::Phone))?;
                match timed(client.sign_in(token, code.expose()))
                    .await
                    .map_err(|kind| failure(kind, AuthStep::Code))?
                {
                    Ok(user) => {
                        self.profile = Some(profile(&user));
                        self.login = None;
                        self.password = None;
                        Ok(AuthStep::Authorized)
                    }
                    Err(SignInError::PasswordRequired(token)) => {
                        self.password = Some(token);
                        Ok(AuthStep::Password)
                    }
                    Err(SignInError::InvalidCode) => {
                        Err(failure(AuthError::InvalidCode, AuthStep::Code))
                    }
                    Err(SignInError::SignUpRequired) => {
                        Err(failure(AuthError::SignUpRequired, AuthStep::Phone))
                    }
                    Err(SignInError::Other(error)) => Err(rpc_failure(error, AuthStep::Code)),
                    Err(SignInError::InvalidPassword(_)) => {
                        Err(failure(AuthError::UnexpectedState, AuthStep::Code))
                    }
                }
            }
            AuthAction::SubmitPassword(password) => {
                let client = self.client()?;
                let token = self
                    .password
                    .take()
                    .ok_or(failure(AuthError::UnexpectedState, AuthStep::Code))?;
                // A failed transport consumes the SRP token. Return to code entry
                // to obtain a fresh one; never retain the submitted password/code.
                match timed(client.check_password(token, password.expose().as_bytes()))
                    .await
                    .map_err(|kind| failure(kind, AuthStep::Code))?
                {
                    Ok(user) => {
                        self.profile = Some(profile(&user));
                        self.login = None;
                        Ok(AuthStep::Authorized)
                    }
                    Err(
                        SignInError::InvalidPassword(token) | SignInError::PasswordRequired(token),
                    ) => {
                        self.password = Some(token);
                        Err(failure(AuthError::InvalidPassword, AuthStep::Password))
                    }
                    Err(SignInError::Other(error)) => Err(rpc_failure(error, AuthStep::Code)),
                    Err(SignInError::SignUpRequired) => {
                        Err(failure(AuthError::SignUpRequired, AuthStep::Phone))
                    }
                    Err(SignInError::InvalidCode) => {
                        Err(failure(AuthError::CodeExpired, AuthStep::Phone))
                    }
                }
            }
            AuthAction::Restart => {
                self.login = None;
                self.password = None;
                Ok(if self.session.is_some() {
                    AuthStep::Phone
                } else {
                    AuthStep::Credentials
                })
            }
        }
    }

    fn client(&self) -> Result<grammers_client::Client, AuthFailure> {
        self.connection
            .as_ref()
            .map(|connection| connection.client.clone())
            .ok_or(failure(AuthError::UnexpectedState, self.step))
    }

    pub(crate) fn catalog_client(
        &self,
    ) -> Result<grammers_client::Client, tgbooks_core::CatalogError> {
        if self.step != AuthStep::Authorized {
            return Err(tgbooks_core::CatalogError::Unauthorized);
        }
        self.client()
            .map_err(|_| tgbooks_core::CatalogError::Unauthorized)
    }
}

impl Authentication for TelegramAuth {
    fn profile(&self) -> Option<AccountProfile> {
        (self.step == AuthStep::Authorized)
            .then(|| self.profile.clone())
            .flatten()
    }

    fn account_id(&self) -> Option<i64> {
        if self.step != AuthStep::Authorized {
            return None;
        }
        self.session.as_ref()?.account_id().ok().flatten()
    }

    async fn execute(&mut self, action: AuthAction) -> Result<AuthStep, AuthFailure> {
        // Grammers 0.11 still panics on some unsupported login replies (e.g.
        // PaymentRequired). Contain that at the adapter boundary, not in GPUI.
        let result = match AssertUnwindSafe(self.perform(action)).catch_unwind().await {
            Ok(result) => result,
            Err(_) => {
                self.disconnect().await;
                Err(failure(
                    AuthError::UnsupportedReply,
                    if self.session.is_some() {
                        AuthStep::Phone
                    } else {
                        AuthStep::Credentials
                    },
                ))
            }
        };
        self.step = match &result {
            Ok(step) => *step,
            Err(error) => error.step,
        };
        result
    }
}

fn profile(user: &grammers_client::peer::User) -> AccountProfile {
    AccountProfile {
        username: user.username().map(str::to_owned),
        name: user.full_name(),
    }
}

async fn timed<T>(future: impl Future<Output = T>) -> Result<T, AuthError> {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .map_err(|_| AuthError::Network)
}

fn failure(kind: AuthError, step: AuthStep) -> AuthFailure {
    AuthFailure { kind, step }
}

fn storage_failure(error: VaultError, step: AuthStep) -> AuthFailure {
    failure(
        match error {
            VaultError::Keyring | VaultError::Io => AuthError::SecureStorage,
            VaultError::MissingKey => AuthError::MissingKey,
            VaultError::Corrupt => AuthError::CorruptSession,
            VaultError::Busy => AuthError::AlreadyRunning,
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            VaultError::Unsupported => AuthError::UnsupportedPlatform,
        },
        step,
    )
}

fn rpc_failure(error: InvocationError, step: AuthStep) -> AuthFailure {
    match error {
        InvocationError::Session(_) => failure(AuthError::SecureStorage, step),
        InvocationError::Rpc(error) => {
            if error.code == 420 {
                failure(AuthError::FloodWait(error.value.unwrap_or(60).max(1)), step)
            } else if error.is("API_ID_INVALID") || error.is("API_HASH_INVALID") {
                failure(AuthError::InvalidCredentials, AuthStep::Credentials)
            } else if error.is("PHONE_NUMBER_INVALID") {
                failure(AuthError::InvalidPhone, AuthStep::Phone)
            } else if error.is("PHONE_CODE_EXPIRED") || error.code == 401 {
                failure(AuthError::CodeExpired, AuthStep::Phone)
            } else {
                failure(AuthError::Rejected, step)
            }
        }
        _ => failure(AuthError::Network, step),
    }
}

#[cfg(test)]
mod tests {
    use grammers_client::sender::RpcError;

    use super::*;

    #[test]
    fn rpc_errors_are_mapped_without_exposing_raw_response_details() {
        let error = InvocationError::Rpc(RpcError {
            code: 420,
            name: "FLOOD_WAIT".into(),
            value: Some(123),
            caused_by: None,
        });
        assert_eq!(
            rpc_failure(error, AuthStep::Code),
            failure(AuthError::FloodWait(123), AuthStep::Code)
        );
        let error = InvocationError::Rpc(RpcError {
            code: 400,
            name: "PHONE_CODE_EXPIRED".into(),
            value: None,
            caused_by: None,
        });
        assert_eq!(
            rpc_failure(error, AuthStep::Code),
            failure(AuthError::CodeExpired, AuthStep::Phone)
        );
    }

    #[tokio::test]
    async fn startup_without_a_session_does_not_access_keychain_or_telegram()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mut auth = TelegramAuth::new(directory.path().to_owned());
        assert_eq!(
            auth.execute(AuthAction::Restore).await,
            Ok(AuthStep::Credentials)
        );
        assert!(!directory.path().join("telegram.session.enc").exists());
        Ok(())
    }
}
