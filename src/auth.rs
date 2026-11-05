//! Optional owner login and revocable bearer tokens for the management API.
//! Credentials are enabled by the explicit, one-time setup flow, never by deploy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::storage::Storage;

const COOKIE: &str = "quasar_session";
const SESSION_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const LOGIN_WINDOW: Duration = Duration::from_secs(5 * 60);
const MAX_LOGIN_FAILURES: usize = 15;

#[derive(Default, Serialize, Deserialize)]
struct Credentials {
    password_hash: Option<String>,
    #[serde(default)]
    tokens: Vec<ApiToken>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ApiToken {
    id: String,
    name: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    hash: String,
}

pub struct AuthManager {
    storage: Arc<Storage>,
    available: bool,
    credentials: tokio::sync::Mutex<Credentials>,
    sessions: Mutex<HashMap<String, Instant>>,
    failures: Mutex<Vec<Instant>>,
    // Bound the memory used by password hashing even when the endpoint is busy.
    password_jobs: Arc<tokio::sync::Semaphore>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Open,
    Owner,
    Token,
}

impl AuthManager {
    pub async fn load(storage: Arc<Storage>) -> anyhow::Result<Arc<Self>> {
        let available = storage.is_persistent();
        let credentials = match storage.get_control_value("auth").await? {
            Some(value) => serde_json::from_value(value)?,
            None => Credentials::default(),
        };
        Ok(Arc::new(Self {
            storage,
            available,
            credentials: tokio::sync::Mutex::new(credentials),
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new(Vec::new()),
            password_jobs: Arc::new(tokio::sync::Semaphore::new(2)),
        }))
    }

    async fn refresh(&self) -> Result<(), ()> {
        if !self.available {
            return Err(());
        }
        let mut credentials = self.credentials.lock().await;
        *credentials = match self
            .storage
            .get_control_value("auth")
            .await
            .map_err(|_| ())?
        {
            Some(value) => serde_json::from_value(value).map_err(|_| ())?,
            None => Credentials::default(),
        };
        Ok(())
    }

    async fn access(&self, headers: &HeaderMap) -> Result<Option<Access>, ()> {
        // Re-read the small credential document so revocation and first setup
        // also take effect in other server processes. A DB error fails closed.
        self.refresh().await?;
        let credentials = self.credentials.lock().await;
        if credentials.password_hash.is_none() {
            return Ok(Some(Access::Open));
        }
        if let Some(token) = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
        {
            let Some((scheme, token)) = token.split_once(' ') else {
                return Ok(None);
            };
            if !scheme.eq_ignore_ascii_case("bearer") || token.len() > 256 {
                return Ok(None);
            }
            let hash = secret_hash(token);
            return Ok(credentials
                .tokens
                .iter()
                .any(|token| equal_hash(&token.hash, &hash))
                .then_some(Access::Token));
        }
        drop(credentials);
        let Some(token) = cookie_token(headers) else {
            return Ok(None);
        };
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, expiry| *expiry > Instant::now());
        Ok(sessions
            .contains_key(&secret_hash(token))
            .then_some(Access::Owner))
    }

    fn session(&self) -> String {
        let token = new_secret();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, expiry| *expiry > Instant::now());
        if sessions.len() >= 128 {
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, expiry)| **expiry)
                .map(|(key, _)| key.clone())
            {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(secret_hash(&token), Instant::now() + SESSION_LIFETIME);
        token
    }

    fn login_allowed(&self) -> bool {
        let mut failures = self.failures.lock().unwrap();
        failures.retain(|at| at.elapsed() < LOGIN_WINDOW);
        failures.len() < MAX_LOGIN_FAILURES
    }
}

pub fn router(manager: Arc<AuthManager>) -> Router {
    Router::new()
        .route("/auth/status", get(status))
        .route("/auth/setup", post(setup))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/tokens", get(tokens).post(create_token))
        .route("/auth/tokens/{id}", delete(revoke_token))
        .layer(DefaultBodyLimit::max(8 * 1024))
        .with_state(manager)
}

/// Apply to management API routes; keep the UI assets and auth router public.
pub async fn require_auth(
    State(manager): State<Arc<AuthManager>>,
    request: Request,
    next: Next,
) -> Response {
    if !manager.available {
        return storage_unavailable();
    }
    let access = match manager.access(request.headers()).await {
        Ok(Some(access)) => access,
        Ok(None) => {
            return error(
                StatusCode::UNAUTHORIZED,
                "Войдите в Quasar или передайте API-токен",
            )
        }
        Err(()) => return storage_unavailable(),
    };
    if access != Access::Token
        && !safe_method(request.method())
        && !same_origin(request.headers(), access == Access::Owner)
    {
        return error(
            StatusCode::FORBIDDEN,
            "Запрос должен быть отправлен с этой страницы Quasar",
        );
    }
    no_store(next.run(request).await)
}

async fn status(State(manager): State<Arc<AuthManager>>, headers: HeaderMap) -> Response {
    let access = manager.access(&headers).await;
    let configured = manager.credentials.lock().await.password_hash.is_some();
    let available = access.is_ok();
    let authenticated = matches!(access, Ok(Some(Access::Owner)));
    no_store(Json(json!({"available":available, "configured": configured, "authenticated": authenticated})).into_response())
}

/// Provider credentials and global configuration belong to the owner. Preserve
/// the existing open setup mode, but do not grant integration tokens this scope.
pub async fn require_settings_access(
    State(manager): State<Arc<AuthManager>>,
    request: Request,
    next: Next,
) -> Response {
    match manager.access(request.headers()).await {
        Ok(Some(Access::Owner | Access::Open)) => next.run(request).await,
        Ok(Some(Access::Token)) => error(StatusCode::FORBIDDEN, "Настройки сервера доступны только владельцу Quasar"),
        Ok(None) => error(StatusCode::UNAUTHORIZED, "Войдите как владелец Quasar"),
        Err(()) => storage_unavailable(),
    }
}

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
}

async fn setup(
    State(manager): State<Arc<AuthManager>>,
    headers: HeaderMap,
    Json(body): Json<PasswordBody>,
) -> Response {
    if manager.refresh().await.is_err() {
        return storage_unavailable();
    }
    if !same_origin(&headers, false) {
        return error(StatusCode::FORBIDDEN, "Настройте вход со страницы Quasar");
    }
    if !(12..=256).contains(&body.password.chars().count()) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Пароль должен содержать от 12 до 256 символов",
        );
    }
    // Serialize setup and token updates; the unique storage key also prevents
    // two server processes from both winning the first-setup race.
    let mut credentials = manager.credentials.lock().await;
    if credentials.password_hash.is_some() {
        return error(
            StatusCode::CONFLICT,
            "Пароль уже настроен. Войдите в Quasar",
        );
    }
    let permit = match manager.password_jobs.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "Подождите перед следующей попыткой",
            )
        }
    };
    let hash = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let nonce = uuid::Uuid::new_v4();
        let salt = SaltString::encode_b64(nonce.as_bytes()).map_err(|_| ())?;
        Argon2::default()
            .hash_password(body.password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| ())
    })
    .await;
    let hash = match hash {
        Ok(Ok(hash)) => hash,
        _ => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Не удалось настроить пароль",
            )
        }
    };
    let next = Credentials {
        password_hash: Some(hash),
        tokens: Vec::new(),
    };
    let value = match serde_json::to_value(&next) {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Не удалось сохранить настройки входа",
            )
        }
    };
    match manager.storage.insert_control_value("auth", &value).await {
        Ok(true) => *credentials = next,
        Ok(false) => {
            return error(
                StatusCode::CONFLICT,
                "Пароль уже настроен. Перезагрузите страницу",
            )
        }
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Не удалось сохранить настройки входа",
            )
        }
    }
    drop(credentials);
    signed_in(&manager, &headers)
}

async fn login(
    State(manager): State<Arc<AuthManager>>,
    headers: HeaderMap,
    Json(body): Json<PasswordBody>,
) -> Response {
    if manager.refresh().await.is_err() {
        return storage_unavailable();
    }
    if !same_origin(&headers, false) {
        return error(StatusCode::FORBIDDEN, "Войдите со страницы Quasar");
    }
    if body.password.len() > 1024 || !manager.login_allowed() {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "Слишком много попыток. Повторите через 5 минут",
        );
    }
    let hash = match manager.credentials.lock().await.password_hash.clone() {
        Some(hash) => hash,
        None => return error(StatusCode::CONFLICT, "Сначала настройте пароль"),
    };
    let permit = match manager.password_jobs.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "Подождите перед следующей попыткой",
            )
        }
    };
    let verified = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        PasswordHash::new(&hash).ok().is_some_and(|hash| {
            Argon2::default()
                .verify_password(body.password.as_bytes(), &hash)
                .is_ok()
        })
    })
    .await
    .unwrap_or(false);
    if !verified {
        manager.failures.lock().unwrap().push(Instant::now());
        return error(StatusCode::UNAUTHORIZED, "Неверный пароль");
    }
    manager.failures.lock().unwrap().clear();
    signed_in(&manager, &headers)
}

async fn logout(State(manager): State<Arc<AuthManager>>, headers: HeaderMap) -> Response {
    if !same_origin(&headers, true) {
        return error(StatusCode::FORBIDDEN, "Выйдите со страницы Quasar");
    }
    if let Some(token) = cookie_token(&headers) {
        manager.sessions.lock().unwrap().remove(&secret_hash(token));
    }
    let mut response = no_store(StatusCode::NO_CONTENT.into_response());
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("quasar_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
    );
    response
}

async fn tokens(State(manager): State<Arc<AuthManager>>, headers: HeaderMap) -> Response {
    if let Err(response) = require_owner(&manager, &headers).await {
        return response;
    }
    let credentials = manager.credentials.lock().await;
    no_store(
        Json(
            credentials
                .tokens
                .iter()
                .map(token_metadata)
                .collect::<Vec<_>>(),
        )
        .into_response(),
    )
}

#[derive(Deserialize)]
struct TokenBody {
    name: String,
}

async fn create_token(
    State(manager): State<Arc<AuthManager>>,
    headers: HeaderMap,
    Json(body): Json<TokenBody>,
) -> Response {
    if let Err(response) = require_owner(&manager, &headers).await {
        return response;
    }
    if !same_origin(&headers, true) {
        return error(StatusCode::FORBIDDEN, "Создайте токен со страницы Quasar");
    }
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Укажите название токена до 80 символов",
        );
    }
    let mut credentials = manager.credentials.lock().await;
    if credentials.tokens.len() >= 32 {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Достигнут лимит 32 токенов. Отзовите ненужный токен",
        );
    }
    let secret = format!("qsr_{}", new_secret());
    let token = ApiToken {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        hash: secret_hash(&secret),
    };
    let expected = json!(&*credentials);
    credentials.tokens.push(token.clone());
    let result = match serde_json::to_value(&*credentials) {
        Ok(value) => {
            manager
                .storage
                .compare_control_value("auth", &expected, &value)
                .await
        }
        Err(error) => Err(error.into()),
    };
    match result {
        Ok(true) => {}
        Ok(false) => {
            credentials.tokens.pop();
            return changed_credentials();
        }
        Err(_) => {
            credentials.tokens.pop();
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Не удалось сохранить токен",
            );
        }
    }
    let mut value = token_metadata(&token);
    value["token"] = json!(secret);
    no_store((StatusCode::CREATED, Json(value)).into_response())
}

async fn revoke_token(
    State(manager): State<Arc<AuthManager>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_owner(&manager, &headers).await {
        return response;
    }
    if !same_origin(&headers, true) {
        return error(StatusCode::FORBIDDEN, "Отзовите токен со страницы Quasar");
    }
    let mut credentials = manager.credentials.lock().await;
    let Some(index) = credentials.tokens.iter().position(|token| token.id == id) else {
        return error(StatusCode::NOT_FOUND, "Токен не найден");
    };
    let expected = json!(&*credentials);
    let removed = credentials.tokens.remove(index);
    let result = match serde_json::to_value(&*credentials) {
        Ok(value) => {
            manager
                .storage
                .compare_control_value("auth", &expected, &value)
                .await
        }
        Err(error) => Err(error.into()),
    };
    match result {
        Ok(true) => {}
        Ok(false) => {
            credentials.tokens.insert(index, removed);
            return changed_credentials();
        }
        Err(_) => {
            credentials.tokens.insert(index, removed);
            return error(StatusCode::SERVICE_UNAVAILABLE, "Не удалось отозвать токен");
        }
    }
    no_store(StatusCode::NO_CONTENT.into_response())
}

async fn require_owner(manager: &AuthManager, headers: &HeaderMap) -> Result<(), Response> {
    match manager.access(headers).await {
        Ok(Some(Access::Owner)) => Ok(()),
        Ok(_) => Err(error(
            StatusCode::UNAUTHORIZED,
            "Войдите как владелец Quasar",
        )),
        Err(()) => Err(storage_unavailable()),
    }
}

fn signed_in(manager: &AuthManager, headers: &HeaderMap) -> Response {
    let mut response = no_store(
        Json(json!({"available":true,"configured":true,"authenticated":true})).into_response(),
    );
    let secure = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin.starts_with("https://"));
    let cookie = format!(
        "{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        manager.session(),
        SESSION_LIFETIME.as_secs(),
        if secure { "; Secure" } else { "" }
    );
    // The cookie only contains a generated ASCII token and fixed attributes.
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    response
}

fn token_metadata(token: &ApiToken) -> serde_json::Value {
    json!({"id":token.id,"name":token.name,"createdAt":token.created_at})
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == COOKIE && value.len() <= 256).then_some(value)
        })
}

fn same_origin(headers: &HeaderMap, required: bool) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|site| !matches!(site, "same-origin" | "none"))
    {
        return false;
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return !required;
    };
    let Some(origin) = origin
        .to_str()
        .ok()
        .and_then(|value| reqwest::Url::parse(value).ok())
    else {
        return false;
    };
    if !matches!(origin.scheme(), "http" | "https")
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return false;
    }
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(expected) = reqwest::Url::parse(&format!("{}://{host}", origin.scheme())) else {
        return false;
    };
    origin.host_str() == expected.host_str()
        && origin.port_or_known_default() == expected.port_or_known_default()
}

fn safe_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn new_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn secret_hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn equal_hash(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .as_bytes()
            .iter()
            .zip(right.as_bytes())
            .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn error(status: StatusCode, message: &str) -> Response {
    no_store((status, Json(json!({"error":message}))).into_response())
}

fn storage_unavailable() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Хранилище недоступно. Проверьте PostgreSQL и перезапустите сервер",
    )
}

fn changed_credentials() -> Response {
    error(
        StatusCode::CONFLICT,
        "Настройки доступа изменились. Обновите страницу и повторите запрос",
    )
}
