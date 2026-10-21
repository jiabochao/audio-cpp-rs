//! Keeps offline sessions warm and bounds how many are resident at once.
//!
//! Creating a session uploads a full copy of the model's weights to the
//! compute device, which costs seconds and gigabytes for large models. A
//! [`SessionPool`] hands the same session to every request with the same
//! [`SessionSpec`], keeps at most [`PoolConfig::max_sessions`] resident, and
//! evicts the least recently used idle session to make room. It frees before
//! it creates, so peak memory stays at the limit rather than one past it.
//!
//! Call [`SessionPool::shutdown`] before the process exits. A session still
//! alive at `exit()` outlives the GPU backend's static teardown, and ggml's
//! Metal backend aborts on that (`GGML_ASSERT([rsets->data count] == 0)`).

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::error::{Error, ErrorKind, Result};
use crate::model::{ModelConfig, Options, Registry};
use crate::session::OfflineSession;
use crate::task::{BackendConfig, Task};

/// Everything that determines a session: two requests with equal specs can
/// share one session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionSpec {
    pub model_path: PathBuf,
    pub model: ModelConfigKey,
    pub task: String,
    pub backend: String,
    pub device: u32,
    pub threads: u32,
    /// `--load-option`s, sorted by key.
    pub load_options: BTreeMap<String, String>,
    /// `--session-option`s, sorted by key.
    pub session_options: BTreeMap<String, String>,
}

/// [`ModelConfig`] in a form that orders and hashes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ModelConfigKey {
    pub family: Option<String>,
    pub config_id: Option<String>,
    pub weight_id: Option<String>,
    pub model_spec_override: Option<String>,
}

impl From<&ModelConfig> for ModelConfigKey {
    fn from(c: &ModelConfig) -> Self {
        Self {
            family: c.family.clone(),
            config_id: c.config_id.clone(),
            weight_id: c.weight_id.clone(),
            model_spec_override: c.model_spec_override.clone(),
        }
    }
}

impl From<&ModelConfigKey> for ModelConfig {
    fn from(c: &ModelConfigKey) -> Self {
        Self {
            family: c.family.clone(),
            config_id: c.config_id.clone(),
            weight_id: c.weight_id.clone(),
            model_spec_override: c.model_spec_override.clone(),
        }
    }
}

impl SessionSpec {
    pub fn new(
        model_path: impl Into<PathBuf>,
        model: &ModelConfig,
        task: impl Into<Task>,
        backend: &BackendConfig,
    ) -> Self {
        Self {
            model_path: model_path.into(),
            model: model.into(),
            task: task.into().as_str().to_string(),
            backend: backend.backend.as_str().to_string(),
            device: backend.device,
            threads: backend.threads,
            load_options: BTreeMap::new(),
            session_options: BTreeMap::new(),
        }
    }

    pub fn load_option(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.load_options.insert(key.into(), value.into());
        self
    }

    pub fn session_option(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.session_options.insert(key.into(), value.into());
        self
    }

    pub fn task(&self) -> Task {
        Task::from(self.task.as_str())
    }

    fn backend_config(&self) -> Result<BackendConfig> {
        Ok(BackendConfig { backend: self.backend.parse()?, device: self.device, threads: self.threads })
    }

    fn create(&self, registry: &Registry) -> Result<OfflineSession> {
        let load = (!self.load_options.is_empty()).then(|| Options::from_pairs(&self.load_options)).transpose()?;
        let model = registry.load_model(&self.model_path, &(&self.model).into(), load.as_ref())?;
        let session =
            (!self.session_options.is_empty()).then(|| Options::from_pairs(&self.session_options)).transpose()?;
        // The session keeps the model alive; dropping our handle frees nothing
        // it still needs.
        model.offline_session(self.task(), &self.backend_config()?, session.as_ref())
    }
}

/// Cooperative cancellation shared between a UI and the work it started.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// `Err(Cancelled)` once cancelled, for `?` between units of work.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() { Err(Error::new(ErrorKind::Cancelled, "cancelled")) } else { Ok(()) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolConfig {
    /// Sessions resident at once, idle or in use. Each holds a full copy of
    /// its model's weights. At least 1.
    pub max_sessions: usize,
    /// Idle sessions unused for this long are freed. `None` keeps them until
    /// space is needed.
    pub idle_timeout: Option<Duration>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self { max_sessions: 1, idle_timeout: None }
    }
}

/// A snapshot of the pool for status displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolStats {
    pub idle: Vec<SessionSpec>,
    pub in_use: Vec<SessionSpec>,
    /// Sessions being created right now.
    pub creating: usize,
    pub max_sessions: usize,
}

struct Idle {
    spec: SessionSpec,
    session: OfflineSession,
    last_used: Instant,
}

#[derive(Default)]
struct State {
    idle: Vec<Idle>,
    in_use: Vec<SessionSpec>,
    creating: usize,
    /// Set by [`SessionPool::shutdown`]: no new checkouts, returned sessions
    /// are freed.
    closed: bool,
}

impl State {
    fn resident(&self) -> usize {
        self.idle.len() + self.in_use.len() + self.creating
    }
}

struct Inner {
    state: Mutex<State>,
    changed: Condvar,
    config: Mutex<PoolConfig>,
    registry: Mutex<Option<Registry>>,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        // State is only bookkeeping; every mutation leaves it consistent.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn config(&self) -> PoolConfig {
        *self.config.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn create(&self, spec: &SessionSpec) -> Result<OfflineSession> {
        let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        if registry.is_none() {
            *registry = Some(Registry::new()?);
        }
        spec.create(registry.as_ref().expect("registry initialized above"))
    }
}

/// A pool of warm offline sessions. Cheap to clone; clones share the pool.
#[derive(Clone)]
pub struct SessionPool {
    inner: Arc<Inner>,
}

enum Wait<'a> {
    Forever,
    Until(Instant),
    Cancellable(&'a CancelToken),
}

impl SessionPool {
    pub fn new(config: PoolConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
                config: Mutex::new(config),
                registry: Mutex::new(None),
            }),
        }
    }

    pub fn config(&self) -> PoolConfig {
        self.inner.config()
    }

    /// Changes the limits; idle sessions beyond the new limit are freed now.
    pub fn set_config(&self, config: PoolConfig) {
        *self.inner.config.lock().unwrap_or_else(PoisonError::into_inner) = config;
        self.trim(config.max_sessions.max(1));
        self.inner.changed.notify_all();
    }

    /// A session for `spec`, reusing an idle one, creating one if there is
    /// room, or else evicting the least recently used idle session. Blocks
    /// while every resident session is in use.
    pub fn checkout(&self, spec: &SessionSpec) -> Result<PooledSession> {
        self.checkout_inner(spec, Wait::Forever)
    }

    /// As [`Self::checkout`], failing with [`ErrorKind::Timeout`] after `timeout`.
    pub fn checkout_timeout(&self, spec: &SessionSpec, timeout: Duration) -> Result<PooledSession> {
        self.checkout_inner(spec, Wait::Until(Instant::now() + timeout))
    }

    /// As [`Self::checkout`], failing with [`ErrorKind::Cancelled`] once
    /// `cancel` fires while waiting. Session creation itself cannot be
    /// interrupted.
    pub fn checkout_cancellable(&self, spec: &SessionSpec, cancel: &CancelToken) -> Result<PooledSession> {
        self.checkout_inner(spec, Wait::Cancellable(cancel))
    }

    fn checkout_inner(&self, spec: &SessionSpec, wait: Wait<'_>) -> Result<PooledSession> {
        let mut state = self.inner.state();
        loop {
            if state.closed {
                return Err(Error::new(ErrorKind::Closed, "the session pool is shut down"));
            }
            if let Wait::Cancellable(token) = &wait {
                token.check()?;
            }
            let config = self.inner.config();
            let evicted = Self::expire(&mut state, config.idle_timeout);

            if let Some(index) = state.idle.iter().rposition(|idle| idle.spec == *spec) {
                let idle = state.idle.remove(index);
                state.in_use.push(spec.clone());
                drop(state);
                drop(evicted);
                return Ok(PooledSession { session: Some(idle.session), spec: idle.spec, pool: self.inner.clone() });
            }

            let max = config.max_sessions.max(1);
            if state.resident() < max {
                state.creating += 1;
                drop(state);
                drop(evicted);
                let created = self.inner.create(spec);
                let mut state = self.inner.state();
                state.creating -= 1;
                return match created {
                    Ok(session) if state.closed => {
                        drop(state);
                        drop(session);
                        self.inner.changed.notify_all();
                        Err(Error::new(ErrorKind::Closed, "the session pool is shut down"))
                    }
                    Ok(session) => {
                        state.in_use.push(spec.clone());
                        Ok(PooledSession { session: Some(session), spec: spec.clone(), pool: self.inner.clone() })
                    }
                    Err(e) => {
                        drop(state);
                        self.inner.changed.notify_all();
                        Err(e)
                    }
                };
            }

            if !state.idle.is_empty() {
                // Least recently used goes first. Freed outside the lock:
                // releasing gigabytes of device memory is not instant.
                let lru = (0..state.idle.len()).min_by_key(|&i| state.idle[i].last_used).expect("idle is non-empty");
                let victim = state.idle.remove(lru);
                drop(state);
                drop(evicted);
                drop(victim);
                state = self.inner.state();
                continue;
            }
            drop(evicted);

            state = match wait {
                Wait::Forever => self.inner.changed.wait(state).unwrap_or_else(PoisonError::into_inner),
                Wait::Until(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(Error::new(ErrorKind::Timeout, "every pooled session is in use"));
                    }
                    self.inner.changed.wait_timeout(state, deadline - now).unwrap_or_else(PoisonError::into_inner).0
                }
                Wait::Cancellable(_) => {
                    self.inner
                        .changed
                        .wait_timeout(state, Duration::from_millis(50))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
    }

    /// Removes idle sessions past `timeout`, returning them so the caller can
    /// free them after releasing the lock.
    fn expire(state: &mut State, timeout: Option<Duration>) -> Vec<Idle> {
        let Some(timeout) = timeout else { return Vec::new() };
        let now = Instant::now();
        let (expired, kept) =
            std::mem::take(&mut state.idle).into_iter().partition(|idle| now - idle.last_used >= timeout);
        state.idle = kept;
        expired
    }

    fn trim(&self, limit: usize) {
        let victims = {
            let mut state = self.inner.state();
            let mut victims = Vec::new();
            while state.resident() > limit && !state.idle.is_empty() {
                let lru = (0..state.idle.len()).min_by_key(|&i| state.idle[i].last_used).expect("idle is non-empty");
                victims.push(state.idle.remove(lru));
            }
            victims
        };
        drop(victims);
    }

    /// Frees every idle session, e.g. for a "release memory" button. Sessions
    /// in use are untouched and return to the pool when dropped.
    pub fn evict_idle(&self) {
        let victims = std::mem::take(&mut self.inner.state().idle);
        drop(victims);
        self.inner.changed.notify_all();
    }

    /// Frees idle sessions whose spec matches `predicate`, e.g. all sessions of
    /// a model that is about to be deleted.
    pub fn evict_where(&self, predicate: impl Fn(&SessionSpec) -> bool) {
        let victims: Vec<Idle> = {
            let mut state = self.inner.state();
            let (victims, kept) = std::mem::take(&mut state.idle).into_iter().partition(|idle| predicate(&idle.spec));
            state.idle = kept;
            victims
        };
        drop(victims);
        self.inner.changed.notify_all();
    }

    /// Closes the pool and frees its sessions: idle ones now, ones in use as
    /// their tasks return them. Waits up to `timeout` for those, and returns
    /// whether every session is gone. Later checkouts, and those blocked
    /// waiting, fail with [`ErrorKind::Closed`].
    ///
    /// Call it before the process exits (see [Resources](crate#resources)). A
    /// session cannot be interrupted mid-inference, so if this returns
    /// `false`, one is still running and the backend will be torn down under
    /// it: exiting with `_exit` (skipping static destructors) is then safer
    /// than `exit`.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let idle = {
            let mut state = self.inner.state();
            state.closed = true;
            std::mem::take(&mut state.idle)
        };
        self.inner.changed.notify_all();
        drop(idle);

        let mut state = self.inner.state();
        while !state.in_use.is_empty() || state.creating > 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self.inner.changed.wait_timeout(state, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
        }
        drop(state);
        self.inner.registry.lock().unwrap_or_else(PoisonError::into_inner).take();
        true
    }

    /// Whether [`Self::shutdown`] was called.
    pub fn is_closed(&self) -> bool {
        self.inner.state().closed
    }

    pub fn stats(&self) -> PoolStats {
        let state = self.inner.state();
        PoolStats {
            idle: state.idle.iter().map(|idle| idle.spec.clone()).collect(),
            in_use: state.in_use.clone(),
            creating: state.creating,
            max_sessions: self.inner.config().max_sessions.max(1),
        }
    }
}

impl std::fmt::Debug for SessionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionPool").field("stats", &self.stats()).finish()
    }
}

/// A session borrowed from a [`SessionPool`]; returned to it on drop.
/// Dereferences to [`OfflineSession`].
pub struct PooledSession {
    session: Option<OfflineSession>,
    spec: SessionSpec,
    pool: Arc<Inner>,
}

impl PooledSession {
    pub fn spec(&self) -> &SessionSpec {
        &self.spec
    }

    /// Frees the session instead of returning it, e.g. after an error that
    /// may have left it in a bad state.
    pub fn discard(mut self) {
        let session = self.session.take();
        self.release(None);
        drop(session);
    }

    fn release(&mut self, session: Option<OfflineSession>) {
        let mut state = self.pool.state();
        if let Some(index) = state.in_use.iter().position(|spec| *spec == self.spec) {
            state.in_use.remove(index);
        }
        let over_limit = state.closed || state.resident() >= self.pool.config().max_sessions.max(1);
        let freed = match session {
            // Over the limit (it was lowered while this was out), or the pool
            // is shut down: free it.
            Some(session) if over_limit => Some(session),
            Some(session) => {
                state.idle.push(Idle { spec: self.spec.clone(), session, last_used: Instant::now() });
                None
            }
            None => None,
        };
        drop(state);
        drop(freed);
        self.pool.changed.notify_all();
    }
}

impl Deref for PooledSession {
    type Target = OfflineSession;

    fn deref(&self) -> &OfflineSession {
        self.session.as_ref().expect("session present until drop")
    }
}

impl DerefMut for PooledSession {
    fn deref_mut(&mut self) -> &mut OfflineSession {
        self.session.as_mut().expect("session present until drop")
    }
}

impl Drop for PooledSession {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            self.release(Some(session));
        }
    }
}

impl std::fmt::Debug for PooledSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledSession").field("spec", &self.spec).finish()
    }
}
