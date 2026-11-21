//! Shared application state and effects. Frontends only select operations.
use crate::{
    auth::CookieJar,
    bili::{BiliClient, BiliOptions},
    config::{self, Account, Config},
    schema,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[path = "obs_observer.rs"]
mod observer;

struct EventConnection {
    socket: crate::danmaku::DanmakuClient,
    client: Arc<BiliClient>,
    uid: String,
    buvid: String,
}

struct State {
    config: Config,
    cookie_override: Option<String>,
    account_generation: u64,
    last: Value,
    danmaku: Option<EventConnection>,
    events: VecDeque<Value>,
    pk_armed: bool,
    wants_events: bool,
    reconnect_attempts: u8,
    retry_at: Instant,
    obs_ignore_start_until: Instant,
    obs_ignore_stop_until: Instant,
}
struct PendingUpdateReply {
    staged_id: String,
    restart: bool,
    value: Value,
    expires_at: Instant,
}
struct Inner {
    directory: PathBuf,
    state: Mutex<State>,
    account_gate: Mutex<()>,
    software_gate: Mutex<()>,
    retiring: AtomicBool,
    update_helper_ready: AtomicBool,
    update_exit_requested: AtomicBool,
    pending_update_reply: Mutex<Option<PendingUpdateReply>>,
    updater: crate::updater::Updater,
    pk: Mutex<crate::pk_session::PkSession>,
    media: crate::media::MediaManager,
    observer: Mutex<Option<observer::Observer>>,
}

#[derive(Clone)]
pub struct App {
    inner: Arc<Inner>,
    read_only: bool,
}

#[derive(Clone, PartialEq, Eq)]
enum CredentialOwner {
    Anonymous,
    Override,
    Saved(String),
}
// Never derive Debug/Serialize: this is a short-lived credential capability.
struct CredentialContext {
    config: Config,
    owner: CredentialOwner,
    generation: u64,
    cookie: String,
}

impl App {
    pub fn new(directory: Option<PathBuf>) -> Result<Self, String> {
        let directory = directory.unwrap_or_else(config::default_dir);
        let config = Config::load(&directory)?;
        let updater = crate::updater::Updater::new(directory.clone());
        let app = Self {
            inner: Arc::new(Inner {
                directory,
                state: Mutex::new(State {
                    config,
                    cookie_override: std::env::var("BILIBILIVE_COOKIE").ok(),
                    account_generation: 0,
                    last: json!({}),
                    danmaku: None,
                    events: VecDeque::new(),
                    pk_armed: false,
                    wants_events: false,
                    reconnect_attempts: 0,
                    retry_at: Instant::now(),
                    obs_ignore_start_until: Instant::now(),
                    obs_ignore_stop_until: Instant::now(),
                }),
                media: crate::media::MediaManager::new(),
                account_gate: Mutex::new(()),
                software_gate: Mutex::new(()),
                retiring: AtomicBool::new(false),
                update_helper_ready: AtomicBool::new(false),
                update_exit_requested: AtomicBool::new(false),
                pending_update_reply: Mutex::new(None),
                updater,
                pk: Mutex::new(crate::pk_session::PkSession::new()),
                observer: Mutex::new(None),
            }),
            read_only: false,
        };
        let weak = Arc::downgrade(&app.inner);
        std::thread::spawn(move || {
            loop {
                {
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    let app = Self {
                        inner,
                        read_only: false,
                    };
                    if let Err(error) = app.event_tick() {
                        app.push_event(json!({"cmd":"BILIBILIVE_ERROR","message":error}));
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        Ok(app)
    }

    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
    pub fn directory(&self) -> &Path {
        &self.inner.directory
    }
    pub fn set_cookie_override(&self, cookie: String) -> Result<(), String> {
        CookieJar::parse(&cookie)?.validate()?;
        self.with_account_lease(|| {
            self.reset_interaction()?;
            let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
            state.cookie_override = Some(cookie);
            state.account_generation = state.account_generation.wrapping_add(1);
            Ok(())
        })
    }
    /// One operation owns account selection, its credential and its follow-up
    /// effects across every frontend of this profile. Lock order is always
    /// account_gate -> profile.lock -> short State/PK scopes.
    fn with_account_lease<T>(
        &self,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _account = self
            .inner
            .account_gate
            .lock()
            .map_err(|_| "Account lock failed")?;
        let _profile = config::lock_profile(&self.inner.directory)?;
        self.reload_config()?;
        operation()
    }
    /// Only lease entry points read disk. Internal helpers use the same cached
    /// Config, so a cookie captured for A can never be paired with B's settings.
    fn config(&self) -> Result<Config, String> {
        Ok(self
            .inner
            .state
            .lock()
            .map_err(|_| "State lock failed")?
            .config
            .clone())
    }
    fn reload_config(&self) -> Result<(), String> {
        let config = Config::load(&self.inner.directory)?;
        let changed = {
            let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
            let changed = active_identity(&state.config) != active_identity(&config);
            if changed {
                state.cookie_override = None;
                state.account_generation = state.account_generation.wrapping_add(1);
            }
            state.config = config;
            changed
        };
        if changed {
            self.reset_interaction()?;
        }
        Ok(())
    }
    fn save_config(&self, next: Config) -> Result<Value, String> {
        next.save(&self.inner.directory)?;
        let result = next.snapshot();
        self.inner
            .state
            .lock()
            .map_err(|_| "State lock failed")?
            .config = next;
        Ok(result)
    }
    pub fn snapshot(&self) -> Value {
        self.with_account_lease(|| Ok(self.cached_snapshot()))
            .unwrap_or_else(|error| json!({"error":error}))
    }
    fn cached_snapshot(&self) -> Value {
        self.inner
            .state
            .lock()
            .map(|s| {
                let mut value = s.config.snapshot();
                value["last_operation"] = s.last.clone();
                value["credential_source"] = json!(if s.cookie_override.is_some() {
                    "override"
                } else if s.config.active.is_some() {
                    "os"
                } else {
                    "none"
                });
                value["effective_uid"] = json!(
                    active_uid(&s.config, s.cookie_override.as_deref())
                        .ok()
                        .flatten()
                );
                value
            })
            .unwrap_or_else(|_| json!({"error":"State lock failed"}))
    }
    fn with_context(&self, mut value: Value) -> Value {
        let context = self
            .inner
            .state
            .lock()
            .map(|state| {
                json!({"generation":state.account_generation,
            "uid":active_uid(&state.config,state.cookie_override.as_deref()).ok().flatten()})
            })
            .unwrap_or(Value::Null);
        if !value.is_object() {
            value = json!({"data":value});
        }
        value["_context"] = context;
        value
    }
    fn credentials(&self) -> Result<CredentialContext, String> {
        let (config, override_cookie, generation) = {
            let state = self.inner.state.lock().map_err(|_| "State lock failed")?;
            (
                state.config.clone(),
                state.cookie_override.clone(),
                state.account_generation,
            )
        };
        let (owner, cookie) = match override_cookie {
            Some(cookie) => (CredentialOwner::Override, cookie),
            None => match &config.active {
                Some(name) => (
                    CredentialOwner::Saved(name.clone()),
                    config::load_secret(&self.inner.directory, &format!("account/{name}"))?,
                ),
                None => (CredentialOwner::Anonymous, String::new()),
            },
        };
        if let CredentialOwner::Saved(name) = &owner {
            let expected = config
                .accounts
                .iter()
                .find(|account| &account.name == name)
                .ok_or("Selected account metadata is missing")?;
            if CookieJar::parse(&cookie)?.uid()? != expected.uid {
                return Err("Stored credential UID does not match the selected account".into());
            }
        }
        Ok(CredentialContext {
            config,
            owner,
            generation,
            cookie,
        })
    }
    fn cookie(&self) -> Result<String, String> {
        Ok(self.credentials()?.cookie)
    }
    fn client_with_cookie(&self, cookie: &str) -> Result<BiliClient, String> {
        let cfg = self.config()?;
        self.client_for_config(cookie, &cfg)
    }
    fn client_for_config(&self, cookie: &str, cfg: &Config) -> Result<BiliClient, String> {
        let proxy = if cfg.proxy_credentials_set {
            Some(config::load_secret(&self.inner.directory, "proxy")?)
        } else {
            (!cfg.proxy.is_empty()).then(|| cfg.proxy.clone())
        };
        BiliClient::with_options(
            cookie,
            BiliOptions {
                proxy,
                use_system_proxy: cfg.use_system_proxy,
                timeout_secs: cfg.timeout_secs,
            },
        )
    }
    fn client(&self) -> Result<BiliClient, String> {
        let context = self.credentials()?;
        self.client_for_config(&context.cookie, &context.config)
    }
    fn persist_refresh(&self, context: &CredentialContext, cookie: &str) -> Result<(), String> {
        let original = CookieJar::parse(&context.cookie)?;
        let refreshed = CookieJar::parse(cookie)?;
        refreshed.validate()?;
        if original.uid()? != refreshed.uid()? {
            return Err("Refreshed credential belongs to a different account".into());
        }
        {
            let state = self.inner.state.lock().map_err(|_| "State lock failed")?;
            if state.account_generation != context.generation
                || active_identity(&state.config) != active_identity(&context.config)
            {
                return Err("Account changed while refreshing credentials".into());
            }
        }
        match &context.owner {
            CredentialOwner::Saved(name) => {
                config::store_secret(&self.inner.directory, &format!("account/{name}"), cookie)
            }
            CredentialOwner::Override => {
                let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
                if state.cookie_override.is_none() {
                    return Err("Credential override was retired during refresh".into());
                }
                state.cookie_override = Some(cookie.into());
                Ok(())
            }
            CredentialOwner::Anonymous => {
                Err("Cannot persist an anonymous credential refresh".into())
            }
        }
    }
    fn obs(&self) -> Result<crate::obs::ObsClient, String> {
        let config = self.config()?;
        let password = if let Ok(value) = std::env::var("BILIBILIVE_OBS_PASSWORD") {
            value
        } else if config.obs_password_set {
            config::load_secret(&self.inner.directory, "obs")?
        } else {
            String::new()
        };
        crate::obs::ObsClient::connect(&config.obs_url, &password)
    }
    fn save_account(&self, name: &str, cookie: &str, identity: &Value) -> Result<Value, String> {
        config::validate_account_name(name)?;
        let jar = CookieJar::parse(cookie)?;
        jar.validate()?;
        let uid = jar.uid()?.to_owned();
        let mut cfg = self.config()?;
        let record = Account {
            name: name.into(),
            uid,
            uname: identity
                .get("uname")
                .or_else(|| identity.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(name)
                .into(),
            room_id: identity["room_id"].as_u64().unwrap_or(0),
        };
        config::store_secret(&self.inner.directory, &format!("account/{name}"), cookie)?;
        cfg.accounts.retain(|a| a.name != name);
        cfg.accounts.push(record);
        cfg.active = Some(name.into());
        self.save_config(cfg)?;
        {
            let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
            state.cookie_override = None;
            state.danmaku = None;
            state.account_generation = state.account_generation.wrapping_add(1);
        }
        self.reset_interaction()?;
        Ok(
            json!({"status":"authenticated","account":name,"uid":jar.uid()?,"credential_store":"os"}),
        )
    }

    pub fn call(&self, method: &str, mut args: Value) -> Result<Value, String> {
        let spec = schema::validate(method, &args)?;
        if self.read_only && !spec.read_only && method != "zuse.run" {
            return Err(format!("{method} is disabled in read-only mode"));
        }
        self.check_retirement(method, spec.read_only)?;
        let _software = if matches!(
            method,
            "obs.plugin.install" | "updates.download" | "updates.apply"
        ) {
            Some(
                self.inner
                    .software_gate
                    .try_lock()
                    .map_err(|error| match error {
                        std::sync::TryLockError::WouldBlock => {
                            "Another local software operation is in progress".to_owned()
                        }
                        std::sync::TryLockError::Poisoned(_) => {
                            "Local software operation lock failed".to_owned()
                        }
                    })?,
            )
        } else {
            None
        };
        self.check_retirement(method, spec.read_only)?;
        // Embedded programs acquire a fresh lease for each step. Software
        // management is not account-bound: it neither holds that lease during
        // file I/O nor waits for another account operation after scheduling an
        // update helper. Its envelope uses this App's current cached context.
        let result = if matches!(method, "zuse.run" | "zuse.check") {
            self.dispatch(method, &mut args).and_then(|value| {
                if is_success(&value)
                    && value["quit_requested"] == true
                    && value["exit_required"] == true
                {
                    // The terminal step has already retired local sessions and
                    // launched the helper. Do not wait for a new account lease
                    // before returning the response that lets this host exit.
                    Ok(self.with_context(value))
                } else {
                    self.with_account_lease(|| Ok(self.with_context(value)))
                }
            })
        } else if method.starts_with("obs.plugin.") || method.starts_with("updates.") {
            self.dispatch(method, &mut args)
                .map(|value| self.with_context(value))
        } else {
            self.with_account_lease(|| {
                self.check_retirement(method, spec.read_only)?;
                self.dispatch(method, &mut args)
                    .map(|value| self.with_context(value))
            })
        };
        let ok = result.as_ref().is_ok_and(is_success);
        if let Ok(mut state) = self.inner.state.lock() {
            // Arguments and responses may contain credentials, stream keys or
            // private chat. The operation log contains only the name and outcome.
            state.last = json!({"operation":method,"ok":ok});
        }
        let polling = method == "face.poll"
            || (method == "auth.qr.poll"
                && result
                    .as_ref()
                    .is_ok_and(|value| value["status"] != "authenticated"));
        if !ok || (!spec.read_only && !polling) {
            if result.as_ref().is_ok_and(|value| {
                value["quit_requested"] == true && value["exit_required"] == true
            }) {
                let _ = crate::logging::try_record(&self.inner.directory, method, ok);
            } else {
                let _ = crate::logging::record(&self.inner.directory, method, ok);
            }
        }
        result
    }

    fn check_retirement(&self, method: &str, read_only: bool) -> Result<(), String> {
        if self.inner.retiring.load(Ordering::Acquire)
            && !read_only
            && !matches!(method, "zuse.run" | "updates.apply")
        {
            return Err(
                "An application update is waiting for this host to exit; new effects are disabled"
                    .into(),
            );
        }
        Ok(())
    }

    /// A transport sets this only after flushing the successful update reply.
    /// GUI hosts can therefore honor an update initiated through their HTTP API.
    pub fn update_exit_requested(&self) -> bool {
        self.inner.update_exit_requested.load(Ordering::Acquire)
    }

    pub(crate) fn acknowledge_update_reply(&self, value: &Value) -> bool {
        let ready = self.inner.update_helper_ready.load(Ordering::Acquire)
            && is_success(value)
            && value["quit_requested"] == true
            && value["exit_required"] == true
            && self
                .inner
                .pending_update_reply
                .lock()
                .ok()
                .is_some_and(|pending| {
                    pending.as_ref().is_some_and(|pending| {
                        Instant::now() < pending.expires_at
                            && value["staged_id"].as_str() == Some(pending.staged_id.as_str())
                    })
                });
        if ready {
            self.inner
                .update_exit_requested
                .store(true, Ordering::Release);
        }
        ready
    }

    fn apply_update(&self, args: &Value) -> Result<Value, String> {
        // Keep the successful helper receipt until its exit deadline. A broken
        // HTTP connection must not make a retry launch another updater, nor
        // leave the caller unable to acknowledge the original successful call.
        let mut pending = self
            .inner
            .pending_update_reply
            .lock()
            .map_err(|_| "Update reply lock failed")?;
        if self.inner.retiring.load(Ordering::Acquire) {
            let previous = pending.as_ref().ok_or("An application update is waiting for this host to exit, but its reply is unavailable; close this host before retrying")?;
            if args["staged_id"].as_str() != Some(previous.staged_id.as_str())
                || args["restart"].as_bool().unwrap_or(true) != previous.restart
            {
                return Err("An application update is already waiting for this host to exit; only the same staged_id and restart choice can recover its reply".into());
            }
            if Instant::now() >= previous.expires_at {
                return Err("The update helper's exit deadline has passed; close this host, reopen the application and retry the downloaded update".into());
            }
            let mut value = previous.value.clone();
            value["receipt_replayed"] = json!(true);
            return Ok(value);
        }
        let apply_started = Instant::now();
        let prepared = std::cell::Cell::new(false);
        let result = self.inner.updater.apply_with_prepare(args, || {
            self.with_account_lease(|| {
                self.inner.retiring.store(true, Ordering::Release);
                self.observer_stop()?;
                self.reset_interaction()?;
                prepared.set(true);
                Ok(())
            })
        });
        match result {
            Ok(mut value) => {
                if prepared.get() {
                    value["local_sessions_retired"] = json!(true);
                    value["room_changed"] = json!(false);
                    value["obs_output_changed"] = json!(false);
                    if is_success(&value)
                        && value["quit_requested"] == true
                        && value["exit_required"] == true
                    {
                        *pending = Some(PendingUpdateReply {
                            staged_id: args["staged_id"].as_str().unwrap_or_default().to_owned(),
                            restart: args["restart"].as_bool().unwrap_or(true),
                            value: value.clone(),
                            expires_at: apply_started
                                + Duration::from_secs(
                                    value["exit_timeout_seconds"].as_u64().unwrap_or(60).min(60),
                                ),
                        });
                        self.inner
                            .update_helper_ready
                            .store(true, Ordering::Release);
                    }
                }
                Ok(value)
            }
            Err(error) if self.inner.retiring.swap(false, Ordering::AcqRel) => Ok(json!({
                "ok":false,"status":"failed","phase":"failed","error":error,
                "local_shutdown_attempted":true,"local_sessions_retired":prepared.get(),
                "room_changed":false,"obs_output_changed":false,"quit_requested":false,
                "message":"The update did not start. Local media or observer state may have changed; reconnect explicitly after reviewing the error."
            })),
            Err(error) => Err(error),
        }
    }

    fn dispatch(&self, method: &str, args: &mut Value) -> Result<Value, String> {
        match method {
            "doctor" => Ok(json!({
                "name":"bilibilive","version":crate::VERSION,"platform":std::env::consts::OS,
                "architecture":std::env::consts::ARCH,"gui":cfg!(feature="gui"),
                "zuse_version":zuse::VERSION,"mcp":"stdio and loopback HTTP",
                "accounts":self.config()?.accounts.len(),"credential_store":"OS keyring",
                "read_only":self.read_only,"config_dir":self.inner.directory,
                "bundle":crate::bundle::diagnostics(),
                "native_media":self.inner.media.call("status", &json!({})).unwrap_or_else(|e|json!({"available":false,"message":e}))
            })),
            "config.get" => Ok(self.cached_snapshot()),
            "config.set" => {
                let mut cfg = self.config()?;
                let key = text(args, "key")?;
                if key == "proxy" && args["value"].as_str().is_some_and(|v| !v.is_empty()) {
                    let original = text(args, "value")?;
                    let mut url = reqwest::Url::parse(original).map_err(|_| "Invalid proxy URL")?;
                    if !["http", "https"].contains(&url.scheme()) {
                        return Err("Proxy must use HTTP or HTTPS".into());
                    }
                    cfg.proxy_credentials_set =
                        !url.username().is_empty() || url.password().is_some();
                    if cfg.proxy_credentials_set {
                        config::store_secret(&self.inner.directory, "proxy", original)?;
                        url.set_username("").map_err(|_| "Invalid proxy username")?;
                        url.set_password(None)
                            .map_err(|_| "Invalid proxy password")?;
                    }
                    cfg.set("proxy", &json!(url.to_string()))?;
                } else {
                    cfg.set(key, &args["value"])?;
                    if key == "proxy" {
                        cfg.proxy_credentials_set = false;
                    }
                }
                self.save_config(cfg)
            }
            "config.reset" => {
                let section = text(args, "section")?;
                let mut cfg = self.config()?;
                let required = match section {
                    "obs" => cfg.obs_password_set,
                    "network" => cfg.proxy_credentials_set,
                    _ => false,
                };
                cfg.reset(section)?;
                // Remove the corresponding captured vault entry while the
                // profile lease prevents another frontend from reusing it.
                if let Some(name) = match section {
                    "obs" => Some("obs"),
                    "network" => Some("proxy"),
                    _ => None,
                } {
                    let removed = config::remove_secret(&self.inner.directory, name);
                    if required {
                        removed?;
                    }
                }
                self.save_config(cfg)
            }
            "logs.read" => crate::logging::recent(
                &self.inner.directory,
                args["limit"].as_u64().unwrap_or(100).min(200) as usize,
            ),
            "logs.clear" => crate::logging::clear(&self.inner.directory),
            "account.list" => {
                let cfg = self.cached_snapshot();
                Ok(json!({"accounts":cfg["accounts"],"active":cfg["active"]}))
            }
            "account.use" => {
                let name = text(args, "name")?;
                let mut cfg = self.config()?;
                if !cfg.accounts.iter().any(|a| a.name == name) {
                    return Err("Account not found".into());
                }
                cfg.active = Some(name.into());
                self.save_config(cfg)?;
                let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
                state.cookie_override = None;
                state.danmaku = None;
                state.account_generation = state.account_generation.wrapping_add(1);
                drop(state);
                self.reset_interaction()?;
                Ok(json!({"active":name}))
            }
            "account.remove" => {
                let name = text(args, "name")?;
                let mut cfg = self.config()?;
                if !cfg.accounts.iter().any(|a| a.name == name) {
                    return Err("Account not found".into());
                }
                config::remove_secret(&self.inner.directory, &format!("account/{name}"))?;
                cfg.accounts.retain(|a| a.name != name);
                if cfg.active.as_deref() == Some(name) {
                    cfg.active = cfg.accounts.first().map(|a| a.name.clone());
                }
                let result = self.save_config(cfg)?;
                {
                    let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
                    state.danmaku = None;
                    state.cookie_override = None;
                    state.account_generation = state.account_generation.wrapping_add(1);
                }
                self.reset_interaction()?;
                Ok(result)
            }
            "auth.import" => {
                let name = text(args, "name")?;
                let cookie = text(args, "cookie")?;
                let client = self.client_with_cookie(cookie)?;
                let identity = client.call("auth.validate", &json!({}))?;
                self.save_account(name, cookie, &identity)
            }
            "auth.qr.create" => self.client_with_cookie("")?.call(method, args),
            "auth.qr.poll" => {
                let mut result = self.client_with_cookie("")?.call(method, args)?;
                if let Some(cookie) = result
                    .as_object_mut()
                    .and_then(|v| v.remove("_private_cookie"))
                    .and_then(|v| v.as_str().map(str::to_owned))
                {
                    let name = args["name"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| result["uid"].as_str().unwrap_or("default").to_owned());
                    let account = self.save_account(&name, &cookie, &result)?;
                    result["account"] = account["account"].clone();
                }
                Ok(result)
            }
            "auth.refresh" => {
                let context = self.credentials()?;
                let mut result = self
                    .client_for_config(&context.cookie, &context.config)?
                    .call(method, args)?;
                if let Some(cookie) = result
                    .as_object_mut()
                    .and_then(|v| v.remove("_private_cookie"))
                    .and_then(|v| v.as_str().map(str::to_owned))
                {
                    self.persist_refresh(&context, &cookie)?;
                }
                Ok(result)
            }
            "room.titles" => {
                let state = self.inner.state.lock().map_err(|_| "State lock failed")?;
                let uid = active_uid(&state.config, state.cookie_override.as_deref())?;
                Ok(
                    json!({"uid":uid,"titles":uid.as_ref().and_then(|uid|state.config.title_history.get(uid)).cloned().unwrap_or_default()}),
                )
            }
            "room.update" => {
                let context = self.credentials()?;
                let uid = CookieJar::parse(&context.cookie)?.uid()?.to_owned();
                let mut result = self
                    .client_for_config(&context.cookie, &context.config)?
                    .call(method, args)?;
                if let Some(title) = args["title"].as_str() {
                    let mut cfg = self.config()?;
                    cfg.remember_title(&uid, title);
                    match self.save_config(cfg) {
                        Ok(saved) => result["recent_titles"] = saved["title_history"][&uid].clone(),
                        Err(error) => {
                            result["history_warning"] =
                                json!({"saved":false,"error":error,"room_update_applied":true})
                        }
                    }
                }
                Ok(result)
            }
            "live.status" => self.client()?.call("room.get", args),
            "live.start" | "live.stream" => {
                let cfg = self.config()?;
                let fields = args.as_object_mut().ok_or("Arguments must be an object")?;
                fields.entry("protocol").or_insert(json!(cfg.protocol));
                fields.entry("strategy").or_insert(json!(cfg.strategy));
                if method == "live.start" && cfg.default_area > 0 {
                    fields.entry("area_id").or_insert(json!(cfg.default_area));
                }
                let client = self.client()?;
                if method == "live.start" && cfg.delay != 0 {
                    client.call("delay.set", &json!({"seconds":cfg.delay}))?;
                }
                let mut result = client.call(method, args)?;
                if method == "live.start" && result["status"] == "live" && cfg.obs_auto_start {
                    let obs = self.configure_obs_stream(&result, true);
                    record_obs_start_result(&mut result, obs);
                }
                Ok(result)
            }
            "live.stop" => {
                self.reset_interaction()?;
                let mut result = self.client()?.call(method, args)?;
                if self.config()?.obs_auto_stop {
                    result["obs"] = self
                        .stop_obs_output()
                        .unwrap_or_else(|e| json!({"ok":false,"error":e}));
                }
                Ok(result)
            }
            "obs.plugin.status" => crate::obs_plugin::status(args),
            "obs.plugin.install" => crate::obs_plugin::install(args),
            "obs.configure" => {
                let mut cfg = self.config()?;
                if let Some(url) = args.get("url") {
                    cfg.set("obs_url", url)?;
                }
                if let Some(password) = args["password"].as_str() {
                    if password.is_empty() {
                        config::remove_secret(&self.inner.directory, "obs")?;
                        cfg.obs_password_set = false;
                    } else {
                        config::store_secret(&self.inner.directory, "obs", password)?;
                        cfg.obs_password_set = true;
                    }
                }
                self.save_config(cfg)
            }
            "obs.status" => {
                let mut result = self.obs()?.status()?;
                result["connected"] = true.into();
                Ok(result)
            }
            "obs.sync" | "obs.start" => {
                let stream = self.dispatch("live.stream", &mut json!({}))?;
                self.configure_obs_stream(&stream, method == "obs.start")
            }
            "obs.stop" => self.stop_obs_output(),
            "obs.watch.start" => self.observer_start(),
            "obs.watch.stop" => self.observer_stop(),
            "obs.watch.status" => self.observer_status(),
            "obs.request" => self.obs()?.request(
                text(args, "request_type")?,
                args.get("data").cloned().unwrap_or_else(|| json!({})),
            ),
            "danmaku.connect" => self.connect_events(),
            "danmaku.read" => {
                let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
                let mut events = Vec::new();
                for _ in 0..args["limit"].as_u64().unwrap_or(1).min(100) {
                    match state.events.pop_front() {
                        Some(event) => events.push(event),
                        None => break,
                    }
                }
                Ok(json!({"events":events,"connected":state.danmaku.is_some()}))
            }
            "danmaku.disconnect" => {
                self.reset_interaction()?;
                Ok(json!({"connected":false}))
            }
            "pk.session" => Ok(self
                .inner
                .pk
                .lock()
                .map_err(|_| "PK lock failed")?
                .snapshot()),
            "pk.match" | "pk.invite" | "pk.accept" | "pk.link.connect" | "pk.link.start" => {
                self.start_pk(method, args)
            }
            "pk.cancel" | "pk.invitation.cancel" | "pk.link.cancel" => self.cancel_pk(method, args),
            "pk.link.disconnect" => {
                let result = self.client()?.call(method, args);
                self.reset_interaction()?;
                result
            }
            "pk.request" if args["operation"] == "quit" => {
                let result = self.client()?.call(method, args);
                self.reset_interaction()?;
                result
            }
            "convert.discover" => Ok(
                json!({"files":crate::convert::discover_configs(args["root"].as_str().map(Path::new))?}),
            ),
            "convert.livehime" => {
                let resolution = match (args["width"].as_u64(), args["height"].as_u64()) {
                    (None, None) => None,
                    (Some(w), Some(h)) if w > 0 && h > 0 && w <= 16384 && h <= 16384 => {
                        Some((w as u32, h as u32))
                    }
                    _ => return Err("Provide both width and height in 1..16384".into()),
                };
                crate::convert::convert_file(
                    Path::new(text(args, "input")?),
                    Path::new(text(args, "output")?),
                    args["name"].as_str().unwrap_or("bilibilive"),
                    args["filter_prefixes"].as_bool().unwrap_or(true),
                    resolution,
                )
            }
            "zuse.schema" => Ok(schema::catalogue()),
            "zuse.check" | "zuse.run" => crate::zuse_host::execute(
                self,
                text(args, "script")?,
                args["base"].as_str().map(Path::new),
                method == "zuse.check",
                self.read_only || args["read_only"].as_bool().unwrap_or(false),
            ),
            "updates.status" => self.inner.updater.status(),
            "updates.check" => self.inner.updater.check(),
            "updates.download" => self.inner.updater.download(args),
            "updates.apply" => self.apply_update(args),
            "rtc.disconnect" => {
                self.reset_interaction()?;
                Ok(json!({"disconnected":true}))
            }
            name if name.starts_with("rtc.") => {
                self.inner.media.call(name.trim_start_matches("rtc."), args)
            }
            _ => self.client()?.call(method, args),
        }
    }

    fn configure_obs_stream(&self, stream: &Value, start: bool) -> Result<Value, String> {
        if stream["available"] == false {
            return Err("Requested stream protocol is unavailable".into());
        }
        let mut obs = self.obs()?;
        obs.configure(text(stream, "server")?, text(stream, "key")?)?;
        if start {
            self.inner
                .state
                .lock()
                .map_err(|_| "State lock failed")?
                .obs_ignore_start_until = Instant::now() + Duration::from_secs(30);
            if let Err(error) = obs.start() {
                self.inner
                    .state
                    .lock()
                    .map_err(|_| "State lock failed")?
                    .obs_ignore_start_until = Instant::now();
                return Err(error);
            }
        }
        Ok(json!({"configured":true,"started":start}))
    }

    fn stop_obs_output(&self) -> Result<Value, String> {
        let mut obs = self.obs()?;
        self.inner
            .state
            .lock()
            .map_err(|_| "State lock failed")?
            .obs_ignore_stop_until = Instant::now() + Duration::from_secs(30);
        let result = obs.stop();
        if result.is_err() {
            self.inner
                .state
                .lock()
                .map_err(|_| "State lock failed")?
                .obs_ignore_stop_until = Instant::now();
        }
        result
    }

    fn reset_interaction(&self) -> Result<(), String> {
        {
            let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
            state.danmaku = None;
            state.pk_armed = false;
            state.wants_events = false;
            state.events.clear();
        }
        let owned = {
            let mut pk = self.inner.pk.lock().map_err(|_| "PK lock failed")?;
            let owned = !pk.snapshot()["interaction"].is_null();
            pk.reset(&self.inner.media)?;
            owned
        };
        // A manually opened rtc.connect is also owned by this account, even if
        // it did not enter through the automatic PK business orchestrator.
        if !owned {
            self.inner.media.call("disconnect", &json!({}))?;
        }
        Ok(())
    }

    fn connect_events(&self) -> Result<Value, String> {
        if self
            .inner
            .state
            .lock()
            .map_err(|_| "State lock failed")?
            .danmaku
            .is_some()
        {
            return Ok(json!({"connected":true}));
        }
        let context = self.credentials()?;
        let mut cookie = context.cookie.clone();
        let client = Arc::new(self.client_for_config(&cookie, &context.config)?);
        if CookieJar::parse(&cookie)?.get("buvid3").is_none() {
            let mut refresh = client.call("auth.refresh", &json!({}))?;
            if let Some(updated) = refresh
                .as_object_mut()
                .and_then(|v| v.remove("_private_cookie"))
                .and_then(|v| v.as_str().map(str::to_owned))
            {
                cookie = updated;
                self.persist_refresh(&context, &cookie)?;
            }
        }
        let jar = CookieJar::parse(&cookie)?;
        let uid = jar.uid()?.to_owned();
        let buvid = jar.get("buvid3").unwrap_or("").to_owned();
        let connection = client.danmaku_connection()?;
        let ack = client.clone();
        let socket = crate::danmaku::DanmakuClient::connect(
            text(&connection, "endpoint")?,
            connection["login"].clone(),
            Box::new(move |sequences| ack.ack_sequences(sequences)),
        )?;
        let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
        if state.account_generation != context.generation {
            return Err("Account changed before room-event connection was installed".into());
        }
        state.danmaku = Some(EventConnection {
            socket,
            client,
            uid,
            buvid,
        });
        state.wants_events = true;
        state.reconnect_attempts = 0;
        Ok(json!({"connected":true}))
    }

    fn start_pk(&self, method: &str, args: &Value) -> Result<Value, String> {
        let cfg = self.config()?;
        let automate = cfg.auto_pk_media;
        let prepared_client = if method == "pk.link.start" {
            let client = self.client()?;
            // Inspect local readiness before any Start request. Do not hold the
            // PK lock while querying media or HTTP: config reload may retire PK.
            let snapshot = self
                .inner
                .pk
                .lock()
                .map_err(|_| "PK lock failed")?
                .snapshot();
            let media = self.inner.media.call("status", &json!({}))?;
            validate_link_start(&snapshot, &media, text(args, "session_id")?)?;
            // Push state may lag the platform. A fresh read closes the interval
            // in which a second click could start another round before PK_INFO.
            let current = client.call("pk.status", &json!({"modern":true}))?;
            if !current["pk_basic"].is_object() {
                return Err("The platform did not return a confirmed PK round snapshot".into());
            }
            if pk_round_active(&current)? {
                return Err(
                    "A PK round is already active; finish it before converting the link".into(),
                );
            }
            Some(client)
        } else {
            None
        };
        if automate && method != "pk.link.start" {
            self.connect_events()?;
            let mut pk = self.inner.pk.lock().map_err(|_| "PK lock failed")?;
            pk.set_media_preferences(cfg.pk_media.clone())?;
            let peer = args.get("peer_uid").or_else(|| args.get("uid")).map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            });
            if method == "pk.accept" {
                pk.expect_invitation(
                    text(args, "session_id")?,
                    peer.as_deref().ok_or("peer_uid is required")?,
                )?;
            } else if let Some(peer) = peer {
                pk.expect_peer(&peer)?;
            } else {
                pk.expect_match();
            }
        }
        // connect_events may refresh and persist buvid/ticket cookies. Create
        // the mutation client afterwards so its native identity matches JOINs.
        let client = match prepared_client {
            Some(client) => client,
            None => self.client()?,
        };
        let mut result = match client.call(method, args) {
            Ok(value) => value,
            Err(error) => {
                if method != "pk.link.start" {
                    self.revoke_pending_pk()?;
                }
                return Err(error);
            }
        };
        if !is_success(&result) {
            if method != "pk.link.start" {
                self.revoke_pending_pk()?;
            }
            return Ok(result);
        }
        if automate {
            if method == "pk.link.connect" {
                let binding = {
                    let mut pk = self.inner.pk.lock().map_err(|_| "PK lock failed")?;
                    bind_link_response(&mut pk, args, &result)
                };
                if let Err(error) = binding {
                    self.revoke_pending_pk()?;
                    return Err(error);
                }
            }
            self.inner
                .state
                .lock()
                .map_err(|_| "State lock failed")?
                .pk_armed = true;
            let jar = CookieJar::parse(&self.cookie()?)?;
            let state = self
                .inner
                .pk
                .lock()
                .map_err(|_| "PK lock failed")?
                .handle_response(
                    &self.inner.media,
                    &client,
                    &result,
                    jar.uid()?,
                    jar.get("buvid3").unwrap_or(""),
                );
            result["media"] = state.unwrap_or_else(|e| json!({"connected":false,"error":e}));
        }
        redact(&mut result);
        Ok(result)
    }

    fn cancel_pk(&self, method: &str, args: &Value) -> Result<Value, String> {
        let mut result = self.client()?.call(method, args)?;
        if is_success(&result) {
            let retained = self.revoke_pending_pk()?;
            if !result.is_object() {
                result = json!({"data":result});
            }
            result["automation"] = json!({"pending_join_revoked":true,"rtc_retained":retained});
        }
        Ok(result)
    }

    fn revoke_pending_pk(&self) -> Result<bool, String> {
        // account_gate serializes this against event_tick. Release PK before
        // touching State; no State->PK/PK->State lock pair is held here.
        let retain = {
            let mut pk = self.inner.pk.lock().map_err(|_| "PK lock failed")?;
            pk.cancel_pending();
            pk.snapshot()["interaction"]["joined"].as_bool() == Some(true)
        };
        self.inner
            .state
            .lock()
            .map_err(|_| "State lock failed")?
            .pk_armed = retain;
        Ok(retain)
    }

    fn push_event(&self, mut event: Value) {
        redact(&mut event);
        if let Ok(mut state) = self.inner.state.lock() {
            if state.events.len() >= 256 {
                state.events.pop_front();
            }
            state.events.push_back(event);
        }
    }

    fn event_tick(&self) -> Result<(), String> {
        if self.inner.retiring.load(Ordering::Acquire) {
            return Ok(());
        }
        let Ok(_guard) = self.inner.account_gate.try_lock() else {
            return Ok(());
        };
        if self.inner.retiring.load(Ordering::Acquire) {
            return Ok(());
        }
        // Reading an atomic config file is only a hint here. Recheck under the
        // profile lock before any callback, credential refresh or media effect.
        // Idle instances need no file creation or profile lock on every tick.
        let disk = Config::load(&self.inner.directory)?;
        let work = {
            let state = self.inner.state.lock().map_err(|_| "State lock failed")?;
            state.wants_events
                || state.danmaku.is_some()
                || state.pk_armed
                || active_identity(&state.config) != active_identity(&disk)
        };
        if !work {
            return Ok(());
        }
        let _profile = config::lock_profile(&self.inner.directory)?;
        self.reload_config()?;
        let reconnect = {
            let state = self.inner.state.lock().map_err(|_| "State lock failed")?;
            state.wants_events && state.danmaku.is_none() && Instant::now() >= state.retry_at
        };
        if reconnect {
            if let Err(error) = self.connect_events() {
                let exhausted = {
                    let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
                    state.reconnect_attempts += 1;
                    state.retry_at = Instant::now()
                        + Duration::from_secs(2u64.pow(state.reconnect_attempts.min(4) as u32));
                    state.reconnect_attempts >= 3
                };
                if exhausted {
                    self.reset_interaction()?;
                }
                return Err(format!("Room events reconnect failed: {error}"));
            }
        }
        let next = {
            let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
            let armed = state.pk_armed && state.config.auto_pk_media;
            match &mut state.danmaku {
                None => None,
                Some(connection) => Some((
                    connection.socket.next_event(),
                    connection.client.clone(),
                    connection.uid.clone(),
                    connection.buvid.clone(),
                    armed,
                )),
            }
        };
        let Some((event, client, uid, buvid, armed)) = next else {
            return Ok(());
        };
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                let mut state = self.inner.state.lock().map_err(|_| "State lock failed")?;
                state.danmaku = None;
                state.retry_at = Instant::now() + Duration::from_secs(2);
                drop(state);
                return Err(error);
            }
        };
        if armed {
            let mut pk = self.inner.pk.lock().map_err(|_| "PK lock failed")?;
            if let Some(event) = &event {
                if let Err(error) = pk.handle_event(&self.inner.media, &client, event, &uid, &buvid)
                {
                    self.push_event(json!({"cmd":"BILIBILIVE_PK_ERROR","message":error}));
                }
            }
            if let Err(error) = pk.poll_media(&self.inner.media, &client, &uid, &buvid) {
                self.push_event(json!({"cmd":"BILIBILIVE_PK_ERROR","message":error}));
            }
        }
        if let Some(event) = event {
            self.push_event(event);
        }
        Ok(())
    }

    /// Block on OBS events for the explicit `obs watch` command or daemon.
    pub fn watch_obs(&self, stop: &std::sync::atomic::AtomicBool) -> Result<(), String> {
        if self.read_only {
            return Err("OBS event linking is disabled in read-only mode".into());
        }
        use std::sync::atomic::Ordering;
        self.call("obs.watch.start", json!({}))?;
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        self.call("obs.watch.stop", json!({}))?;
        Ok(())
    }
}

fn active_identity(config: &Config) -> Option<(String, String)> {
    config.active.as_ref().map(|name| {
        (
            name.clone(),
            config
                .accounts
                .iter()
                .find(|account| &account.name == name)
                .map(|account| account.uid.clone())
                .unwrap_or_default(),
        )
    })
}

fn active_uid(config: &Config, cookie_override: Option<&str>) -> Result<Option<String>, String> {
    if let Some(cookie) = cookie_override {
        return Ok(Some(CookieJar::parse(cookie)?.uid()?.to_owned()));
    }
    Ok(config
        .active
        .as_ref()
        .and_then(|name| config.accounts.iter().find(|account| &account.name == name))
        .map(|account| account.uid.clone()))
}

fn bind_link_response(
    pk: &mut crate::pk_session::PkSession,
    args: &Value,
    response: &Value,
) -> Result<(), String> {
    let peer = args
        .get("uid")
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string())
        })
        .ok_or("uid is required")?;
    let session = text(response, "session_id")?;
    pk.expect_invitation(session, &peer)
}

fn validate_link_start(snapshot: &Value, media: &Value, session: &str) -> Result<(), String> {
    let interaction = &snapshot["interaction"];
    if interaction["joined"].as_bool() != Some(true)
        || interaction["session_id"].as_str() != Some(session)
        || interaction["business"].as_str() != Some("universal_multi_conn")
    {
        return Err("Start PK requires the exact currently joined video-link session".into());
    }
    let rtc = &media["rtc"];
    let same_id = |left: &Value, right: &Value| -> bool {
        let number = |value: &Value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
        };
        number(left).is_some_and(|n| n > 0 && Some(n) == number(right))
    };
    if rtc["connected"].as_bool() != Some(true)
        || rtc["joined"].as_bool() != Some(true)
        || !same_id(&rtc["channel_id"], &interaction["channel_id"])
        || !same_id(&rtc["uid"], &interaction["uid"])
    {
        return Err("The current video link has no matching live RTC connection".into());
    }
    let publication = &interaction["publication"];
    let tracks = rtc["tracks"]
        .as_array()
        .ok_or("RTC publication state is unavailable")?;
    let mut published = false;
    for kind in ["audio", "video"] {
        if publication[kind].as_bool() == Some(true) {
            if !tracks.iter().any(|track| {
                track["outgoing"].as_bool() == Some(true) && track["kind"].as_str() == Some(kind)
            }) {
                return Err(format!("Local {kind} publication is not ready"));
            }
            published = true;
        }
    }
    if !published
        || media["capture"]["running"].as_bool() != Some(true)
        || !media["capture"]["error"].is_null()
        || !media["error"].is_null()
    {
        return Err("Local media publication and capture must be ready before starting PK".into());
    }
    if pk_round_active(&snapshot["round"])? {
        return Err("A PK round is already active; finish it before converting the link".into());
    }
    Ok(())
}

/// Conservative projection of the exact round shapes retained by PkSession.
/// Terminal modern status is 1401; the classic >=1001 rule is not reused here.
fn pk_round_active(round: &Value) -> Result<bool, String> {
    if round.is_null() {
        return Ok(false);
    }
    let integer = |value: &Value| {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
    };
    if let Some(event) = round["event"].as_str() {
        return match event {
            "PK_BATTLE_END" => Ok(false),
            "PK_BATTLE_PRE" | "PK_BATTLE_START" => Ok(true),
            _ => Err("Cannot confirm the current PK round state".into()),
        };
    }
    if let Some(basic) = round.get("pk_basic").filter(|value| value.is_object()) {
        let status = integer(&basic["status"]).ok_or("Cannot confirm the current PK status")?;
        // Native absent matching metadata defaults to zero; malformed present
        // values are rejected rather than treated as an idle room.
        let matching = match round.get("pk_match_info").filter(|value| !value.is_null()) {
            None => 0,
            Some(value) if value.is_object() => match value.get("match_status") {
                None => 0,
                Some(value) => {
                    integer(value).ok_or("Cannot confirm the current matching status")?
                }
            },
            Some(_) => return Err("Cannot confirm the current matching state".into()),
        };
        return Ok(!((status == 0 && matching == 0) || status == 1401));
    }
    if let Some(status) = round.get("pk_status") {
        return integer(status)
            .map(|status| status != 0 && status != 1401)
            .ok_or_else(|| "Cannot confirm the current PK status".into());
    }
    Err("Cannot confirm the current PK round state".into())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key} is required"))
}

/// A presentation helper. Callers must explicitly opt in to printing stream keys.
pub fn redact(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                if [
                    "cookie",
                    "_private_cookie",
                    "password",
                    "obs_password",
                    "key",
                    "stream_key",
                    "token",
                    "ticket",
                    "v_voucher",
                    "join_token",
                    "access_token",
                    "qrcode_key",
                    "bili_ticket",
                    "sessdata",
                    "bili_jct",
                    "access_key",
                    "refresh_token",
                ]
                .contains(&name.to_ascii_lowercase().as_str())
                    || (name == "code" && value.is_string())
                {
                    *value = json!("[redacted]");
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact),
        _ => (),
    }
}

fn record_obs_start_result(stream: &mut Value, result: Result<Value, String>) {
    match result {
        Ok(obs) => stream["obs"] = obs,
        Err(error) => {
            stream["ok"] = false.into();
            stream["status"] = "partial_failure".into();
            stream["room_remains_live"] = true.into();
            stream["message"] = "The room is live, but automatic OBS streaming failed. Check OBS connection and output status, then handle streaming manually; do not repeat the room start.".into();
            stream["obs"] = json!({"ok":false,"error":error,"room_remains_live":true});
        }
    }
}

pub fn is_success(value: &Value) -> bool {
    value.get("ok") != Some(&Value::Bool(false))
        && !matches!(
            value["status"].as_str(),
            Some(
                "failed"
                    | "blocked"
                    | "cancelled"
                    | "partial_failure"
                    | "verification_required"
                    | "live_without_requested_protocol"
                    | "expired_or_failed"
            )
        )
}

#[cfg(test)]
mod broadcast_result_tests {
    use super::*;

    #[test]
    fn obs_start_failure_reports_partial_success_without_losing_live_room_state() {
        let started = json!({
            "status": "live",
            "room_id": "123",
            "protocol": "rtmp",
            "server": "rtmp://fixture.invalid/live",
            "key": "fixture-stream-secret",
        });
        let mut failed = started.clone();
        record_obs_start_result(&mut failed, Err("OBS fixture failure".into()));
        assert!(!is_success(&failed));
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["status"], "partial_failure");
        assert_eq!(failed["room_remains_live"], true);
        for field in ["room_id", "protocol", "server", "key"] {
            assert_eq!(failed[field], started[field]);
        }
        assert_eq!(failed["obs"]["error"], "OBS fixture failure");
        assert_eq!(failed["obs"]["room_remains_live"], true);
        assert!(failed["message"].as_str().unwrap().contains("manually"));
        redact(&mut failed);
        assert!(!failed.to_string().contains("fixture-stream-secret"));

        let mut complete = started;
        record_obs_start_result(&mut complete, Ok(json!({"configured":true,"started":true})));
        assert!(is_success(&complete));
        assert_eq!(complete["status"], "live");
        assert_eq!(complete["obs"]["started"], true);
        assert!(complete.get("room_remains_live").is_none());
    }
}

#[cfg(test)]
mod pk_integration_tests {
    use super::*;

    fn ready_link() -> (Value, Value) {
        (
            json!({"interaction":{"joined":true,"session_id":"link-a","business":"universal_multi_conn","uid":123,"channel_id":9,"publication":{"audio":true,"video":true}},"round":null}),
            json!({"rtc":{"connected":true,"joined":true,"uid":123,"channel_id":9,"tracks":[{"kind":"audio","outgoing":true},{"kind":"video","outgoing":true}]},"capture":{"running":true,"error":null},"error":null}),
        )
    }

    #[test]
    fn link_response_binds_the_server_session_and_cancellation_closes_admission() {
        let mut pk = crate::pk_session::PkSession::new();
        pk.expect_peer("456").unwrap();
        bind_link_response(
            &mut pk,
            &json!({"uid":456}),
            &json!({"session_id":"assigned-by-platform"}),
        )
        .unwrap();
        assert_eq!(pk.snapshot()["expected_session"], "assigned-by-platform");
        assert_eq!(pk.snapshot()["expected_peer"], 456);
        pk.cancel_pending();
        assert_eq!(pk.snapshot()["accepting_join"], false);
        assert!(pk.snapshot()["expected_peer"].is_null());
        pk.expect_match();
        assert_eq!(pk.snapshot()["accepting_join"], true);
        assert!(bind_link_response(&mut pk, &json!({"uid":456}), &json!({})).is_err());
    }

    #[test]
    fn link_to_pk_requires_the_current_session_and_live_local_publication() {
        let (snapshot, media) = ready_link();
        validate_link_start(&snapshot, &media, "link-a").unwrap();
        assert!(validate_link_start(&snapshot, &media, "another-link").is_err());
        let mut broken = media.clone();
        broken["rtc"]["channel_id"] = 10.into();
        assert!(validate_link_start(&snapshot, &broken, "link-a").is_err());
        let mut broken = media.clone();
        broken["capture"]["running"] = false.into();
        assert!(validate_link_start(&snapshot, &broken, "link-a").is_err());
        let mut broken = media.clone();
        broken["rtc"]["tracks"][1]["outgoing"] = false.into();
        assert!(validate_link_start(&snapshot, &broken, "link-a").is_err());
        let mut active = snapshot.clone();
        active["round"] = json!({"event":"PK_BATTLE_START","pk_id":77});
        assert!(validate_link_start(&active, &media, "link-a").is_err());
        active["round"] = json!({"event":"PK_BATTLE_END","pk_id":77});
        validate_link_start(&active, &media, "link-a").unwrap();
    }

    #[test]
    fn modern_round_gate_rejects_matching_and_unknown_status_shapes() {
        assert!(!pk_round_active(&json!({"pk_basic":{"status":0},"pk_match_info":null})).unwrap());
        assert!(
            pk_round_active(&json!({"pk_basic":{"status":0},"pk_match_info":{"match_status":1}}))
                .unwrap()
        );
        assert!(pk_round_active(&json!({"pk_basic":{"status":201}})).unwrap());
        assert!(pk_round_active(&json!({"pk_basic":{"status":1001}})).unwrap());
        assert!(!pk_round_active(&json!({"pk_basic":{"status":1401}})).unwrap());
        assert!(pk_round_active(&json!({"pk_basic":{"status":"invalid"}})).is_err());
        assert!(pk_round_active(&json!({"pk_id":77})).is_err());
    }
}

#[cfg(test)]
mod account_lease_tests {
    use super::*;
    use std::{
        fs,
        sync::{Weak, mpsc},
        thread,
    };

    struct Fixture {
        directory: PathBuf,
        owners: Vec<Weak<Inner>>,
    }
    impl Fixture {
        fn new() -> Self {
            let mut random = [0u8; 8];
            getrandom::fill(&mut random).unwrap();
            let directory = std::env::temp_dir().join(format!(
                "bilibilive-account-lease-{}-{:016x}",
                std::process::id(),
                u64::from_ne_bytes(random)
            ));
            let mut cfg = Config::default();
            cfg.accounts = vec![
                Account {
                    name: "alpha".into(),
                    uid: "111".into(),
                    uname: "Alpha".into(),
                    room_id: 11,
                },
                Account {
                    name: "beta".into(),
                    uid: "222".into(),
                    uname: "Beta".into(),
                    room_id: 22,
                },
            ];
            cfg.active = Some("alpha".into());
            cfg.remember_title("111", "Alpha title");
            cfg.remember_title("222", "Beta title");
            cfg.save(&directory).unwrap();
            Self {
                directory,
                owners: Vec::new(),
            }
        }
        fn app(&mut self) -> App {
            let app = App::new(Some(self.directory.clone())).unwrap();
            self.owners.push(Arc::downgrade(&app.inner));
            app
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Let a weak-owned background tick finish before deleting its test
            // profile; it must not recreate a directory during fixture cleanup.
            let started = Instant::now();
            while self.owners.iter().any(|owner| owner.strong_count() != 0)
                && started.elapsed() < Duration::from_secs(3)
            {
                thread::sleep(Duration::from_millis(1));
            }
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    fn cookie(secret: &str) -> String {
        format!("DedeUserID=111; SESSDATA={secret}; bili_jct=fixture-csrf; buvid3=fixture-device")
    }

    #[test]
    fn independent_frontend_switch_waits_for_the_whole_captured_account_chain() {
        let mut fixture = Fixture::new();
        let first = fixture.app();
        let second = fixture.app();
        first
            .set_cookie_override(cookie("initial-fixture"))
            .unwrap();
        first.inner.pk.lock().unwrap().expect_peer("333").unwrap();
        let (worker_started, started) = mpsc::channel();
        let (release, continue_work) = mpsc::channel();
        let worker = first.clone();
        let active = thread::spawn(move || {
            worker.with_account_lease(|| {
                let context = worker.credentials()?;
                worker_started.send(()).unwrap();
                continue_work
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| "fixture release timed out")?;
                assert_eq!(worker.config()?.active.as_deref(), Some("alpha"));
                assert_eq!(CookieJar::parse(&context.cookie)?.uid()?, "111");
                worker.persist_refresh(&context, &cookie("refreshed-fixture"))?;
                Ok(context)
            })
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let (attempted, attempt) = mpsc::channel();
        let (finished, finish) = mpsc::channel();
        let switcher = thread::spawn(move || {
            attempted.send(()).unwrap();
            let result = second.call("account.use", json!({"name":"beta"}));
            finished.send(result).unwrap();
        });
        attempt.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            finish.recv_timeout(Duration::from_millis(40)).is_err(),
            "account switch bypassed the profile lease"
        );
        release.send(()).unwrap();
        let captured = active.join().unwrap().unwrap();
        let switched = finish
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        switcher.join().unwrap();
        assert_eq!(switched["active"], "beta");
        // No UI request is needed to observe an external selection change. The
        // background tick invalidates the old override and pending PK context.
        let deadline = Instant::now() + Duration::from_secs(3);
        while first.inner.state.lock().unwrap().config.active.as_deref() != Some("beta")
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        {
            let state = first.inner.state.lock().unwrap();
            assert_eq!(state.config.active.as_deref(), Some("beta"));
            assert!(state.cookie_override.is_none());
        }
        assert!(first.inner.pk.lock().unwrap().snapshot()["expected_peer"].is_null());
        // The old callback cannot save even a valid old-account credential into
        // the newly selected account. This fails before touching any OS vault.
        assert!(
            first
                .with_account_lease(|| first.persist_refresh(&captured, &cookie("stale-fixture")))
                .unwrap_err()
                .contains("Account changed")
        );
        let titles = first.call("room.titles", json!({})).unwrap();
        assert_eq!(titles["uid"], "222");
        assert_eq!(titles["titles"], json!(["Beta title"]));
        assert_eq!(first.snapshot()["effective_uid"], "222");
    }

    #[test]
    fn embedded_zuse_reenters_through_per_step_leases_and_reads_account_titles() {
        let mut fixture = Fixture::new();
        let app = fixture.app();
        let worker = app.clone();
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            tx.send(worker.call("zuse.run",json!({"script":"config.set(key=\"default_area\",value=371); config.get(); room.titles()"}))).unwrap();
        });
        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("embedded zuse deadlocked on an outer account lease")
            .unwrap();
        thread.join().unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["completed_steps"], 3);
        assert_eq!(
            result["steps"][2]["result"]["titles"],
            json!(["Alpha title"])
        );
        assert_eq!(result["_context"]["uid"], "111");
        let logs = app.call("logs.read", json!({"limit":20})).unwrap();
        let operations = logs["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|record| record["operation"].as_str())
            .collect::<Vec<_>>();
        assert!(operations.contains(&"config.set"));
        assert!(operations.contains(&"zuse.run"));
        assert!(!operations.contains(&"config.get"));
        assert!(!operations.contains(&"room.titles"));
    }

    #[test]
    fn read_only_programs_execute_queries_but_preflight_all_mutations() {
        let mut fixture = Fixture::new();
        let app = fixture.app().with_read_only(true);
        let queried = app
            .call("zuse.run", json!({"script":"config.get(); room.titles()"}))
            .unwrap();
        assert_eq!(queried["status"], "completed");
        assert!(app.call("zuse.run",json!({"script":"config.get(); config.set(key=\"default_area\",value=999)","read_only":false})).is_err());
        assert_ne!(
            app.call("config.get", json!({})).unwrap()["default_area"],
            999
        );
        let denied = app
            .call("obs.plugin.install", json!({}))
            .expect_err("read-only mode must reject installation before inspecting any bundle");
        assert!(denied.contains("disabled in read-only mode"));
        assert!(
            app.call(
                "zuse.run",
                json!({"script":"config.get(); obs.plugin.install()"})
            )
            .is_err()
        );
        for operation in ["updates.download", "updates.apply"] {
            let denied = app
                .call(operation, json!({"staged_id":"not-a-reviewed-update"}))
                .expect_err("read-only mode must reject update effects before source inspection");
            assert!(denied.contains("disabled in read-only mode"));
            let program = format!("config.get(); {operation}(staged_id=\"not-a-reviewed-update\")");
            assert!(app.call("zuse.run", json!({"script":program})).is_err());
        }
    }

    #[test]
    fn update_retirement_keeps_queries_available_and_rejects_new_effects() {
        let mut fixture = Fixture::new();
        let app = fixture.app();
        let before = app.call("config.get", json!({})).unwrap()["default_area"].clone();
        app.inner.retiring.store(true, Ordering::Release);
        assert!(
            app.call("config.set", json!({"key":"default_area","value":999}))
                .unwrap_err()
                .contains("waiting for this host to exit")
        );
        assert_eq!(
            app.call("config.get", json!({})).unwrap()["default_area"],
            before
        );
        let queried = app
            .call("zuse.run", json!({"script":"config.get()"}))
            .unwrap();
        assert_eq!(queried["status"], "completed");
        let mutation = app
            .call(
                "zuse.run",
                json!({"script":"config.set(key=\"default_area\",value=999)"}),
            )
            .unwrap();
        assert_eq!(mutation["status"], "failed");
        assert_eq!(
            app.call("config.get", json!({})).unwrap()["default_area"],
            before
        );
    }

    fn pending_update(app: &App, expires_at: Instant) {
        // Model only a helper that has already accepted the update. These tests
        // never start an installer or modify the executable running the suite.
        *app.inner.pending_update_reply.lock().unwrap() = Some(PendingUpdateReply {
            staged_id: "fixture-reviewed-update".into(),
            restart: true,
            expires_at,
            value: json!({"ok":true,"phase":"restart_pending","staged_id":"fixture-reviewed-update", "quit_requested":true,"exit_required":true,"restart_requested":true,"exit_timeout_seconds":60,"local_sessions_retired":true,"room_changed":false,"obs_output_changed":false}),
        });
        app.inner.retiring.store(true, Ordering::Release);
        app.inner.update_helper_ready.store(true, Ordering::Release);
    }

    #[test]
    fn interrupted_update_reply_can_only_be_recovered_for_the_same_pending_request() {
        let mut fixture = Fixture::new();
        let app = fixture.app();
        pending_update(&app, Instant::now() + Duration::from_secs(60));
        let args = json!({"staged_id":"fixture-reviewed-update"});
        let response = app.call("updates.apply", args.clone()).unwrap();
        assert_eq!(response["receipt_replayed"], true);
        assert_eq!(response["local_sessions_retired"], true);
        assert!(!app.update_exit_requested());
        for args in [
            json!({"staged_id":"another-update"}),
            json!({"staged_id":"fixture-reviewed-update","restart":false}),
        ] {
            assert!(
                app.call("updates.apply", args)
                    .unwrap_err()
                    .contains("same staged_id")
            );
        }
        assert!(
            app.clone()
                .with_read_only(true)
                .call("updates.apply", args.clone())
                .is_err()
        );
        let mut unrelated = response.clone();
        unrelated["staged_id"] = "another-update".into();
        assert!(!app.acknowledge_update_reply(&unrelated));
        assert!(!app.update_exit_requested());
        app.inner
            .pending_update_reply
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .expires_at = Instant::now();
        assert!(
            app.call("updates.apply", args)
                .unwrap_err()
                .contains("deadline has passed")
        );
        assert!(!app.acknowledge_update_reply(&response));
        assert!(!app.update_exit_requested());
    }

    #[test]
    fn direct_and_terminal_zuse_mcp_updates_wait_for_the_flushed_reply() {
        for (method, arguments) in [
            (
                "updates.apply",
                json!({"staged_id":"fixture-reviewed-update"}),
            ),
            (
                "zuse.run",
                json!({"script":"updates.apply(staged_id=\"fixture-reviewed-update\")"}),
            ),
        ] {
            let mut fixture = Fixture::new();
            let app = fixture.app();
            pending_update(&app, Instant::now() + Duration::from_secs(60));
            let mut session = crate::mcp::Session::new(app.clone());
            session.handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"update-fixture","version":"1"}}}).to_string().as_bytes()).unwrap();
            session
                .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
            let bytes = session.handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":method,"arguments":arguments}}).to_string().as_bytes()).unwrap().unwrap();
            let reply: Value = serde_json::from_slice(&bytes).unwrap();
            assert_ne!(reply["result"]["isError"], true);
            assert_eq!(reply["result"]["structuredContent"]["quit_requested"], true);
            assert_eq!(
                reply["result"]["structuredContent"]["receipt_replayed"],
                true
            );
            assert!(
                !app.update_exit_requested(),
                "encoding a reply must not acknowledge delivery"
            );
            assert!(session.complete_reply());
            assert!(app.update_exit_requested());
            assert!(!session.complete_reply());
        }
    }

    #[test]
    fn a_busy_operation_log_cannot_hold_the_update_helper_host_open() {
        let mut fixture = Fixture::new();
        let app = fixture.app();
        pending_update(&app, Instant::now() + Duration::from_secs(60));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(fixture.directory.join("operations.lock"))
            .unwrap();
        file.lock().unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            sender
                .send(app.call(
                    "zuse.run",
                    json!({"script":"updates.apply(staged_id=\"fixture-reviewed-update\")"}),
                ))
                .unwrap();
        });
        let result = receiver.recv_timeout(Duration::from_secs(2));
        drop(file);
        worker.join().unwrap();
        let result = result
            .expect("an unrelated log lock blocked a terminal update reply")
            .unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["receipt_replayed"], true);
        assert_eq!(result["exit_required"], true);
    }
}
