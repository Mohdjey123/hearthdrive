//! Signing in safely.
//!
//! - **Two-step sign-in**: after the password, a six-digit code from an
//!   authenticator app (TOTP) or one of ten one-time recovery codes.
//! - **Passkeys**: signing in with Windows Hello, a phone or a security key
//!   instead of a password. They count as both steps.
//! - **Sessions**: where someone is signed in, and ending those sessions.
//! - **Confirming it is you**: sensitive changes ask for the password again,
//!   and a code when two-step sign-in is on.
//! - **Security log**: sign-ins (failed ones too), account and device
//!   changes.
//!
//! Locking accounts is on `/users/{id}`, blocking devices on `/devices/{id}`.

use super::{audit, clamp_limit};
use crate::auth::{AuthUser, bearer};
use crate::error::ApiResult;
use crate::net::Throttle;
use crate::state::SharedState;
use axum::Json;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use nexus_common::NexusError;
use nexus_common::time::now_ms;
use nexus_common::types::{ActivityEntry, User};
use nexus_database::{Passkey, SessionInfo};
use nexus_security::webauthn::{self, b64, unb64};
use nexus_security::{Action, generate_token, hash_token, totp, verify_password};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SESSION_DAYS: i64 = 30;
/// How long the code step may take after the password was accepted.
const TICKET_SECS: i64 = 5 * 60;
const TICKET_SCOPE: &str = "sign-in-code";
const CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CHALLENGES: usize = 500;
/// Wrong codes allowed per account within ten minutes.
const MAX_CODE_FAILURES: u32 = 5;

/// Passkey challenges handed out and not yet answered, and wrong codes per
/// account.
pub struct SecurityControl {
    challenges: Mutex<HashMap<Vec<u8>, Challenge>>,
    code_failures: Throttle<String>,
}

impl Default for SecurityControl {
    fn default() -> Self {
        Self { challenges: Mutex::default(), code_failures: Throttle::with_limit(MAX_CODE_FAILURES) }
    }
}

struct Challenge {
    /// The account adding a passkey, or `None` for a sign-in.
    adding_for: Option<String>,
    expires: Instant,
}

impl SecurityControl {
    fn new_challenge(&self, adding_for: Option<String>) -> Result<Vec<u8>, NexusError> {
        let challenge = generate_token().into_bytes();
        let mut map = self.challenges.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        map.retain(|_, c| c.expires > now);
        if map.len() >= MAX_CHALLENGES {
            return Err(NexusError::RateLimited);
        }
        map.insert(challenge.clone(), Challenge { adding_for, expires: now + CHALLENGE_TTL });
        Ok(challenge)
    }

    /// Takes a challenge back: each one is answered once.
    fn take(&self, challenge: &[u8]) -> Option<Challenge> {
        let mut map = self.challenges.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(challenge).filter(|c| c.expires > Instant::now())
    }
}

// ---------------------------------------------------------------------------
// Shared checks

fn protect(secret: &[u8]) -> Result<Vec<u8>, NexusError> {
    nexus_devices::dpapi::protect(secret).map_err(NexusError::internal)
}

fn unprotect(stored: &[u8]) -> Result<Vec<u8>, NexusError> {
    nexus_devices::dpapi::unprotect(stored).map_err(|_| NexusError::internal("The authenticator-app secret cannot be read on this Windows account"))
}

pub(crate) async fn two_factor_on(s: &SharedState, user_id: &str) -> Result<bool, NexusError> {
    Ok(s.db.totp_state(user_id).await?.enabled_at.is_some())
}

/// How a code was accepted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CodeKind {
    App,
    Recovery,
}

/// Checks (and uses up) a code from the authenticator app or a recovery
/// code. `None` if it is wrong or was already used.
async fn accept_code(s: &SharedState, user_id: &str, code: &str) -> Result<Option<CodeKind>, NexusError> {
    let state = s.db.totp_state(user_id).await?;
    let (Some(stored), Some(_)) = (state.secret, state.enabled_at) else { return Ok(None) };
    if totp::looks_like_code(code) {
        let secret = unprotect(&stored)?;
        let Some(step) = totp::verify(&secret, code, now_ms() / 1000, state.last_step) else { return Ok(None) };
        return Ok(s.db.use_totp_step(user_id, step).await?.then_some(CodeKind::App));
    }
    let normalized = totp::normalize_recovery_code(code);
    if normalized.len() != 10 {
        return Ok(None);
    }
    Ok(s.db.use_recovery_code(user_id, &hash_token(&normalized)).await?.then_some(CodeKind::Recovery))
}

async fn slow_down() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

/// The account a request acts for (the owner, for the app itself).
fn account(user: &AuthUser) -> Result<&str, NexusError> {
    if user.device_id.is_some() {
        return Err(NexusError::PermissionDenied("Paired devices cannot do this".into()));
    }
    user.user_id.as_deref().ok_or_else(|| NexusError::invalid("Set up an account first"))
}

/// Asks for the password again, and a code when two-step sign-in is on,
/// before a sensitive change (turning two-step sign-in off, adding a
/// passkey, showing a backup's recovery key).
pub(crate) async fn confirm_identity(s: &SharedState, user: &AuthUser, ip: IpAddr, password: &str, code: Option<&str>) -> ApiResult<()> {
    let user_id = account(user)?.to_string();
    s.throttle.check(ip)?;
    s.security.code_failures.check(user_id.clone())?;
    let found = s.db.find_credentials(&user.username).await?.filter(|(u, _)| u.id == user_id);
    let password = password.to_string();
    let valid = tokio::task::spawn_blocking(move || found.is_some_and(|(_, hash)| verify_password(&password, &hash)))
        .await
        .map_err(NexusError::internal)?;
    if !valid {
        s.throttle.fail(ip);
        s.security.code_failures.fail(user_id);
        slow_down().await;
        return Err(NexusError::PermissionDenied("That password is not right".into()).into());
    }
    if two_factor_on(s, &user_id).await? {
        let code = code.map(str::trim).filter(|c| !c.is_empty()).ok_or_else(|| NexusError::invalid("Enter a code from your authenticator app"))?;
        if accept_code(s, &user_id, code).await?.is_none() {
            s.throttle.fail(ip);
            s.security.code_failures.fail(user_id);
            slow_down().await;
            return Err(NexusError::PermissionDenied("That code is not right".into()).into());
        }
    }
    Ok(())
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub token: String,
    pub expires_at: i64,
    pub user: User,
}

pub(crate) async fn issue_session(s: &SharedState, user: User, headers: &HeaderMap, method: &str) -> ApiResult<SessionResponse> {
    let token = generate_token();
    let expires_at = now_ms() + SESSION_DAYS * 24 * 60 * 60 * 1000;
    let client = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok());
    s.db.create_session(&user.id, &hash_token(&token), expires_at, client, method).await?;
    Ok(SessionResponse { token, expires_at, user })
}

/// Who a sign-in entry in the log is about, before they have a session.
fn signing_in(user: &User) -> AuthUser {
    AuthUser {
        user_id: Some(user.id.clone()),
        username: user.username.clone(),
        role: user.role,
        local: false,
        device_id: None,
        allowed_roots: None,
        shared: Vec::new(),
        via_token: false,
    }
}

/// Logs a refused sign-in. Unknown names are logged as typed (shortened).
pub(crate) async fn log_failed(s: &SharedState, who: Option<&User>, typed: &str, why: &str) {
    let result = match who {
        Some(user) => s.db.log_activity("auth.failed", Some(why), Some(&user.id), Some(&s.node.device_id)).await,
        None => {
            let name: String = typed.trim().chars().filter(|c| !c.is_control()).take(40).collect();
            s.db.log_activity("auth.failed", Some(&format!("{why} (\"{name}\")")), None, Some(&s.node.device_id)).await
        }
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, "could not write activity log");
    }
}

fn locked() -> NexusError {
    NexusError::PermissionDenied("This account is locked. Ask an owner to unlock it.".into())
}

// ---------------------------------------------------------------------------
// Signing in

/// What `/auth/login` answers: a session, or a request for a code.
#[derive(Serialize)]
#[serde(untagged)]
pub enum LoginReply {
    Session(SessionResponse),
    Code { two_factor: bool, ticket: String },
}

/// Called by `/auth/login` once the password is right.
pub(crate) async fn after_password(s: &SharedState, user: User, headers: &HeaderMap) -> ApiResult<LoginReply> {
    if s.db.user_locked(&user.id).await? {
        log_failed(s, Some(&user), "", "account locked").await;
        return Err(locked().into());
    }
    if two_factor_on(s, &user.id).await? {
        let ticket = format!("{}:{}", user.id, s.tickets.issue(TICKET_SCOPE, &user.id, TICKET_SECS));
        return Ok(LoginReply::Code { two_factor: true, ticket });
    }
    audit(s, &signing_in(&user), "auth.login", None).await;
    Ok(LoginReply::Session(issue_session(s, user, headers, "password").await?))
}

#[derive(Deserialize)]
pub struct CodeRequest {
    ticket: String,
    code: String,
}

/// `POST /auth/login/code`: the second step.
pub async fn login_code(
    State(s): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<CodeRequest>,
) -> ApiResult<Json<SessionResponse>> {
    s.throttle.check(peer.ip())?;
    let expired = || NexusError::PermissionDenied("This sign-in took too long. Enter your password again.".into());
    let (user_id, ticket) = req.ticket.split_once(':').ok_or_else(expired)?;
    if !s.tickets.verify(ticket, TICKET_SCOPE, user_id) {
        return Err(expired().into());
    }
    s.security.code_failures.check(user_id.to_string())?;
    let user = s.db.get_user(user_id).await?;
    let Some(kind) = accept_code(&s, user_id, &req.code).await? else {
        s.throttle.fail(peer.ip());
        s.security.code_failures.fail(user_id.to_string());
        log_failed(&s, Some(&user), "", "wrong code").await;
        slow_down().await;
        return Err(NexusError::PermissionDenied("That code is not right".into()).into());
    };
    if s.db.user_locked(user_id).await? {
        return Err(locked().into());
    }
    s.throttle.clear(peer.ip());
    s.security.code_failures.clear(user_id.to_string());
    let (method, note) = match kind {
        CodeKind::App => ("password+code", "with a code".to_string()),
        CodeKind::Recovery => ("password+recovery", format!("with a recovery code ({} left)", s.db.recovery_codes_left(user_id).await?)),
    };
    audit(&s, &signing_in(&user), "auth.login", Some(&note)).await;
    Ok(Json(issue_session(&s, user, &headers, method).await?))
}

/// The page a passkey request comes from, if passkeys can work there: the
/// app's own window, or a browser at `http://localhost:<port>` (passkeys
/// need a name, so `127.0.0.1` does not work).
fn page_origin(headers: &HeaderMap) -> Option<String> {
    let origin = headers.get(header::ORIGIN)?.to_str().ok()?;
    if crate::allowed_origins().any(|o| o == origin) {
        return Some(origin.to_string());
    }
    let host = headers.get(header::HOST)?.to_str().ok()?;
    (localhost(host) && origin == format!("http://{host}")).then(|| origin.to_string())
}

fn localhost(host: &str) -> bool {
    host == "localhost" || host.strip_prefix("localhost:").is_some_and(|p| p.parse::<u16>().is_ok())
}

/// Whether the page asking can use passkeys. Browsers send no `Origin` on
/// a page's own GET requests, so the address it was opened at decides.
fn passkeys_here(headers: &HeaderMap) -> bool {
    page_origin(headers).is_some()
        || (!headers.contains_key(header::ORIGIN) && headers.get(header::HOST).and_then(|h| h.to_str().ok()).is_some_and(localhost))
}

fn need_origin(headers: &HeaderMap) -> Result<String, NexusError> {
    page_origin(headers).ok_or_else(|| NexusError::invalid("Passkeys work in the NEXUS app, or in a browser at http://localhost (not 127.0.0.1)"))
}

/// `POST /auth/passkey/start`: a challenge for signing in with a passkey.
pub async fn passkey_start(State(s): State<SharedState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    s.throttle.check(peer.ip())?;
    need_origin(&headers)?;
    let challenge = s.security.new_challenge(None)?;
    Ok(Json(json!({ "challenge": b64(&challenge), "rp_id": webauthn::RP_ID })))
}

#[derive(Deserialize)]
pub struct PasskeySignIn {
    challenge: String,
    credential_id: String,
    client_data: String,
    authenticator_data: String,
    signature: String,
    #[serde(default)]
    user_handle: Option<String>,
}

/// `POST /auth/passkey`: signs in with a passkey.
pub async fn passkey_sign_in(
    State(s): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PasskeySignIn>,
) -> ApiResult<Json<SessionResponse>> {
    s.throttle.check(peer.ip())?;
    let origin = need_origin(&headers)?;
    let refuse = |why: &str| NexusError::PermissionDenied(why.to_string());
    let challenge = unb64(&req.challenge).map_err(|e| refuse(e.0))?;
    let pending = s.security.take(&challenge).filter(|c| c.adding_for.is_none());
    if pending.is_none() {
        return Err(refuse("This sign-in took too long. Try again.").into());
    }
    let Some(passkey) = s.db.passkey_by_credential(&req.credential_id).await? else {
        s.throttle.fail(peer.ip());
        log_failed(&s, None, "passkey", "unknown passkey").await;
        return Err(refuse("This passkey is not known here (it may have been removed)").into());
    };
    let user = s.db.get_user(&passkey.user_id).await?;
    let decode = |text: &str| unb64(text).map_err(|e| refuse(e.0));
    if let Some(handle) = req.user_handle.as_deref().filter(|h| !h.is_empty())
        && decode(handle)? != passkey.user_id.as_bytes()
    {
        return Err(refuse("This passkey belongs to someone else").into());
    }
    let checked = webauthn::verify_assertion(
        &passkey.public_key,
        u32::try_from(passkey.sign_count).unwrap_or(u32::MAX),
        &decode(&req.client_data)?,
        &decode(&req.authenticator_data)?,
        &decode(&req.signature)?,
        &challenge,
        &origin,
    );
    let count = match checked {
        Ok(count) => count,
        Err(why) => {
            s.throttle.fail(peer.ip());
            log_failed(&s, Some(&user), "", &format!("passkey refused: {why}")).await;
            return Err(refuse(why.0).into());
        }
    };
    if !s.db.use_passkey(&passkey.id, passkey.sign_count, count).await? {
        return Err(refuse("This passkey was used twice at once. Try again.").into());
    }
    if s.db.user_locked(&user.id).await? {
        log_failed(&s, Some(&user), "", "account locked").await;
        return Err(locked().into());
    }
    s.throttle.clear(peer.ip());
    audit(&s, &signing_in(&user), "auth.login", Some(&format!("with the passkey \"{}\"", passkey.name))).await;
    Ok(Json(issue_session(&s, user, &headers, "passkey").await?))
}

// ---------------------------------------------------------------------------
// Settings → Security

#[derive(Serialize)]
pub struct SessionView {
    #[serde(flatten)]
    session: SessionInfo,
    /// The session this request was made with.
    current: bool,
}

#[derive(Serialize)]
pub struct SecurityView {
    two_factor: bool,
    two_factor_since: Option<i64>,
    recovery_codes_left: i64,
    passkeys: Vec<Passkey>,
    sessions: Vec<SessionView>,
    /// Whether passkeys can be used from the page asking.
    passkeys_here: bool,
}

/// `GET /auth/security`: the caller's sign-in settings and sessions.
pub async fn get(State(s): State<SharedState>, user: AuthUser, headers: HeaderMap) -> ApiResult<Json<SecurityView>> {
    let user_id = account(&user)?;
    let current = bearer(&headers).map(hash_token);
    let totp = s.db.totp_state(user_id).await?;
    let sessions = s
        .db
        .sessions_of(user_id)
        .await?
        .into_iter()
        .map(|session| SessionView { current: current.as_deref() == Some(session.token_hash.as_str()), session })
        .collect();
    Ok(Json(SecurityView {
        two_factor: totp.enabled_at.is_some(),
        two_factor_since: totp.enabled_at,
        recovery_codes_left: s.db.recovery_codes_left(user_id).await?,
        passkeys: s.db.passkeys_of(user_id).await?,
        sessions,
        passkeys_here: passkeys_here(&headers),
    }))
}

#[derive(Deserialize)]
pub struct Confirm {
    password: String,
    #[serde(default)]
    code: Option<String>,
}

/// `POST /auth/2fa/setup`: a new authenticator-app secret, shown as a QR
/// code; two-step sign-in starts once a code from it is entered.
pub async fn setup_two_factor(
    State(s): State<SharedState>,
    user: AuthUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<Confirm>,
) -> ApiResult<Json<Value>> {
    let user_id = account(&user)?.to_string();
    if two_factor_on(&s, &user_id).await? {
        return Err(NexusError::Conflict("Two-step sign-in is already on".into()).into());
    }
    confirm_identity(&s, &user, peer.ip(), &req.password, None).await?;
    let secret = totp::new_secret();
    s.db.set_totp_pending(&user_id, &protect(&secret)?).await?;
    let node = s.db.get_device(&s.node.device_id).await?.name;
    Ok(Json(json!({
        "secret": totp::base32(&secret),
        "uri": totp::uri(&secret, &format!("NEXUS ({node})"), &user.username),
    })))
}

#[derive(Deserialize)]
pub struct EnableTwoFactor {
    code: String,
}

fn hashed(codes: &[String]) -> Vec<String> {
    codes.iter().map(|c| hash_token(&totp::normalize_recovery_code(c))).collect()
}

/// `POST /auth/2fa/enable`: turns two-step sign-in on with a first code from
/// the app, and hands out recovery codes (shown once).
pub async fn enable_two_factor(
    State(s): State<SharedState>,
    user: AuthUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<EnableTwoFactor>,
) -> ApiResult<Json<Value>> {
    let user_id = account(&user)?.to_string();
    s.throttle.check(peer.ip())?;
    s.security.code_failures.check(user_id.clone())?;
    let state = s.db.totp_state(&user_id).await?;
    if state.enabled_at.is_some() {
        return Err(NexusError::Conflict("Two-step sign-in is already on".into()).into());
    }
    let stored = state.secret.ok_or_else(|| NexusError::invalid("Start setting up two-step sign-in again"))?;
    let secret = unprotect(&stored)?;
    let Some(step) = totp::verify(&secret, &req.code, now_ms() / 1000, None) else {
        s.security.code_failures.fail(user_id);
        slow_down().await;
        return Err(NexusError::PermissionDenied(
            "That code is not right. Check the app shows NEXUS and that your phone sets its clock automatically.".into(),
        )
        .into());
    };
    let codes = totp::recovery_codes();
    s.db.enable_totp(&user_id, step, &hashed(&codes)).await?;
    audit(&s, &user, "auth.2fa.enabled", None).await;
    Ok(Json(json!({ "recovery_codes": codes })))
}

/// `POST /auth/2fa/disable`: turns two-step sign-in off (password and code).
pub async fn disable_two_factor(
    State(s): State<SharedState>,
    user: AuthUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<Confirm>,
) -> ApiResult<StatusCode> {
    let user_id = account(&user)?.to_string();
    if !two_factor_on(&s, &user_id).await? {
        // Forget a setup that was never finished.
        s.db.disable_totp(&user_id).await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    confirm_identity(&s, &user, peer.ip(), &req.password, req.code.as_deref()).await?;
    s.db.disable_totp(&user_id).await?;
    audit(&s, &user, "auth.2fa.disabled", None).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /auth/2fa/recovery-codes`: ten new recovery codes; the old ones stop
/// working.
pub async fn new_recovery_codes(
    State(s): State<SharedState>,
    user: AuthUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<Confirm>,
) -> ApiResult<Json<Value>> {
    let user_id = account(&user)?.to_string();
    if !two_factor_on(&s, &user_id).await? {
        return Err(NexusError::invalid("Turn on two-step sign-in first").into());
    }
    confirm_identity(&s, &user, peer.ip(), &req.password, req.code.as_deref()).await?;
    let codes = totp::recovery_codes();
    s.db.replace_recovery_codes(&user_id, &hashed(&codes)).await?;
    audit(&s, &user, "auth.recovery_codes.renewed", None).await;
    Ok(Json(json!({ "recovery_codes": codes })))
}

/// `POST /auth/passkeys/start`: what the browser needs to make a passkey for
/// the caller, after they confirm it is them.
pub async fn add_passkey_start(
    State(s): State<SharedState>,
    user: AuthUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<Confirm>,
) -> ApiResult<Json<Value>> {
    let user_id = account(&user)?.to_string();
    need_origin(&headers)?;
    confirm_identity(&s, &user, peer.ip(), &req.password, req.code.as_deref()).await?;
    let existing: Vec<String> = s.db.passkeys_of(&user_id).await?.into_iter().map(|p| p.credential_id).collect();
    let challenge = s.security.new_challenge(Some(user_id.clone()))?;
    let node = s.db.get_device(&s.node.device_id).await?.name;
    Ok(Json(json!({
        "challenge": b64(&challenge),
        "rp": { "id": webauthn::RP_ID, "name": format!("NEXUS ({node})") },
        "user": { "id": b64(user_id.as_bytes()), "name": user.username, "display_name": user.username },
        "algorithms": webauthn::ALGORITHMS,
        "exclude": existing,
    })))
}

#[derive(Deserialize)]
pub struct NewPasskey {
    challenge: String,
    client_data: String,
    attestation_object: String,
    #[serde(default)]
    name: String,
}

/// `POST /auth/passkeys`: stores the passkey the browser just made.
pub async fn add_passkey(State(s): State<SharedState>, user: AuthUser, headers: HeaderMap, Json(req): Json<NewPasskey>) -> ApiResult<(StatusCode, Json<Passkey>)> {
    let user_id = account(&user)?.to_string();
    let origin = need_origin(&headers)?;
    let refuse = |why: &str| NexusError::invalid(why);
    let challenge = unb64(&req.challenge).map_err(|e| refuse(e.0))?;
    if s.security.take(&challenge).and_then(|c| c.adding_for).as_deref() != Some(user_id.as_str()) {
        return Err(refuse("This took too long. Add the passkey again.").into());
    }
    let client_data = unb64(&req.client_data).map_err(|e| refuse(e.0))?;
    let attestation = unb64(&req.attestation_object).map_err(|e| refuse(e.0))?;
    let made = webauthn::verify_registration(&client_data, &attestation, &challenge, &origin).map_err(|e| refuse(e.0))?;
    let name: String = req.name.trim().chars().filter(|c| !c.is_control()).take(64).collect();
    let name = if name.is_empty() { "Passkey".to_string() } else { name };
    let passkey = s.db.add_passkey(&user_id, &b64(&made.credential_id), &made.public_key, made.algorithm, made.sign_count, &name).await?;
    audit(&s, &user, "auth.passkey.added", Some(&name)).await;
    Ok((StatusCode::CREATED, Json(passkey)))
}

/// `DELETE /auth/passkeys/{id}`.
pub async fn remove_passkey(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let user_id = account(&user)?;
    let name = s.db.remove_passkey(user_id, &id).await?.ok_or_else(|| NexusError::NotFound("Passkey".into()))?;
    audit(&s, &user, "auth.passkey.removed", Some(&name)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /auth/sessions/{id}`: signs one of the caller's sessions out.
pub async fn end_session(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let user_id = account(&user)?;
    if !s.db.end_session(user_id, &id).await? {
        return Err(NexusError::NotFound("Session".into()).into());
    }
    audit(&s, &user, "auth.session.ended", None).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /auth/sessions/end-others`: signs out everywhere else.
pub async fn end_other_sessions(State(s): State<SharedState>, user: AuthUser, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let user_id = account(&user)?;
    let keep = if user.local { None } else { bearer(&headers).map(hash_token) };
    let ended = s.db.end_other_sessions(user_id, keep.as_deref()).await?;
    if ended > 0 {
        audit(&s, &user, "auth.session.ended", Some(&format!("{ended} other session(s)"))).await;
    }
    Ok(Json(json!({ "ended": ended })))
}

#[derive(Deserialize)]
pub struct LogQuery {
    limit: Option<i64>,
}

/// `GET /auth/security/log`: sign-ins and account and device changes;
/// everyone but owners and administrators sees only their own.
pub async fn log(State(s): State<SharedState>, user: AuthUser, Query(q): Query<LogQuery>) -> ApiResult<Json<Vec<ActivityEntry>>> {
    account(&user)?;
    let limit = clamp_limit(q.limit, 100, 500);
    if user.require(Action::ManageUsers).is_ok() {
        return Ok(Json(s.db.security_activity(limit).await?));
    }
    let mut entries = s.db.security_activity(500).await?;
    entries.retain(|e| e.username.as_deref() == Some(user.username.as_str()));
    entries.truncate(limit as usize);
    Ok(Json(entries))
}
