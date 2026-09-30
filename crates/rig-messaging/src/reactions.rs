//! Per-message reaction status and Rig lifecycle observation.
//!
//! ```
//! use rig_messaging::reactions::ReactionConfig;
//! let config = ReactionConfig::default();
//! assert_eq!(config.timing.debounce_ms, 700);
//! ```

use crate::{ChatAdapter, MessageRef};
use rig_agent::agent::{
    AgentHook, DispatchAction, DispatchEvent, HookContext, ObservationAction, OutcomeAction,
    OutcomeEvent, ReasoningDelta, TextDelta,
};
use rig_core::effect::EffectKind;
// Copyright (c) 2026 openabdev. Licensed under MIT; see LICENSE.OpenAB.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::{
    sync::{mpsc, oneshot},
    time::{Duration, Instant},
};

/// Emoji used for each progress state.
#[derive(Debug, Clone)]
pub struct ReactionEmojis {
    /// Queued input.
    pub queued: String,
    /// Active model reasoning.
    pub thinking: String,
    /// Generic tool work.
    pub tool: String,
    /// Coding tool work.
    pub coding: String,
    /// Web tool work.
    pub web: String,
    /// Successfully delivered reply.
    pub done: String,
    /// Failed run, persistence or delivery.
    pub error: String,
    /// First stalled-progress threshold.
    pub stall_soft: String,
    /// Second stalled-progress threshold.
    pub stall_hard: String,
}
impl Default for ReactionEmojis {
    fn default() -> Self {
        Self {
            queued: "👀".into(),
            thinking: "🤔".into(),
            tool: "🔥".into(),
            coding: "👨‍💻".into(),
            web: "⚡".into(),
            done: "🆗".into(),
            error: "😱".into(),
            stall_soft: "🥱".into(),
            stall_hard: "😨".into(),
        }
    }
}

/// Status update and retention timing in milliseconds.
#[derive(Debug, Clone)]
pub struct ReactionTiming {
    /// Delay before applying a progress state.
    pub debounce_ms: u64,
    /// Delay without progress before the soft stall state.
    pub stall_soft_ms: u64,
    /// Delay without progress before the hard stall state.
    pub stall_hard_ms: u64,
    /// Retention after success when removal is enabled.
    pub done_hold_ms: u64,
    /// Retention after error when removal is enabled.
    pub error_hold_ms: u64,
}
impl Default for ReactionTiming {
    fn default() -> Self {
        Self {
            debounce_ms: 700,
            stall_soft_ms: 10000,
            stall_hard_ms: 30000,
            done_hold_ms: 1500,
            error_hold_ms: 2500,
        }
    }
}

/// Per-run reaction configuration. Unsupported adapters disable reactions automatically.
#[derive(Debug, Clone)]
pub struct ReactionConfig {
    /// Enable status reactions.
    pub enabled: bool,
    /// Emoji for each state.
    pub emojis: ReactionEmojis,
    /// Update and retention intervals.
    pub timing: ReactionTiming,
    /// Remove status and mood reactions after the retention interval.
    pub remove_after_reply: bool,
}
impl Default for ReactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            emojis: Default::default(),
            timing: Default::default(),
            remove_after_reply: false,
        }
    }
}

fn classify_tool<'a>(name: &str, emojis: &'a ReactionEmojis) -> &'a str {
    let name = name.to_lowercase();
    if [
        "web_search",
        "web_fetch",
        "web-search",
        "web-fetch",
        "browser",
    ]
    .iter()
    .any(|token| name.contains(token))
    {
        &emojis.web
    } else if ["exec", "process", "read", "write", "edit", "bash", "shell"]
        .iter()
        .any(|token| name.contains(token))
    {
        &emojis.coding
    } else {
        &emojis.tool
    }
}

enum Command {
    Queued(oneshot::Sender<()>),
    Progress(String),
    Touch,
    Finish {
        error: bool,
        ack: oneshot::Sender<()>,
    },
    Clear(oneshot::Sender<()>),
}

/// Owns one message's status. Progress methods enqueue updates without network waits.
/// Queued, terminal and clear methods wait for their worker operations to finish.
pub struct StatusReactions {
    tx: Option<mpsc::UnboundedSender<Command>>,
    emojis: ReactionEmojis,
    finished: AtomicBool,
    epoch: Instant,
    last_touch: AtomicU64,
}
impl StatusReactions {
    /// Start a worker when configuration and adapter capabilities enable reactions.
    /// Call within a Tokio runtime when reactions are enabled.
    pub fn new(adapter: Arc<dyn ChatAdapter>, message: MessageRef, cfg: ReactionConfig) -> Self {
        let tx = if cfg.enabled && adapter.supports_reactions() {
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(
                Worker {
                    adapter,
                    message,
                    cfg: cfg.clone(),
                    current: None,
                    mood: None,
                    finished: false,
                    pending: None,
                    progress: None,
                    soft_shown: false,
                    hard_shown: false,
                }
                .run(rx),
            );
            Some(tx)
        } else {
            None
        };
        Self {
            tx,
            emojis: cfg.emojis,
            finished: AtomicBool::new(false),
            epoch: Instant::now(),
            last_touch: AtomicU64::new(u64::MAX),
        }
    }
    fn send(&self, command: Command) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(command);
        }
    }
    /// Apply queued status before waiting on the conversation lock.
    pub async fn set_queued(&self) {
        if self.tx.is_none() || self.finished.load(Ordering::Acquire) {
            return;
        }
        let (ack, wait) = oneshot::channel();
        self.send(Command::Queued(ack));
        let _ = wait.await;
    }
    /// Schedule thinking status with debounce. Does not wait on the platform API.
    pub fn set_thinking(&self) {
        if self.tx.is_some() && !self.finished.load(Ordering::Acquire) {
            self.send(Command::Progress(self.emojis.thinking.clone()));
        }
    }
    /// Schedule a tool status based on its name. Web classification has precedence.
    pub fn set_tool(&self, name: &str) {
        if self.tx.is_some() && !self.finished.load(Ordering::Acquire) {
            self.send(Command::Progress(classify_tool(name, &self.emojis).into()));
        }
    }
    /// Record text progress at most once per second without changing the current emoji.
    pub fn touch(&self) {
        if self.tx.is_none() || self.finished.load(Ordering::Acquire) {
            return;
        }
        let now = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX - 1);
        let last = self.last_touch.load(Ordering::Relaxed);
        if (last == u64::MAX || now.saturating_sub(last) >= 1000)
            && self
                .last_touch
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.send(Command::Touch);
        }
    }
    /// Apply success and a random mood reaction. Later progress updates are ignored.
    pub async fn set_done(&self) {
        self.finish(false).await;
    }
    /// Apply error status. Later progress updates are ignored.
    pub async fn set_error(&self) {
        self.finish(true).await;
    }
    async fn finish(&self, error: bool) {
        if self.tx.is_none() || self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        let (ack, wait) = oneshot::channel();
        self.send(Command::Finish { error, ack });
        let _ = wait.await;
    }
    /// Remove applied status and mood reactions and cancel future status updates.
    pub async fn clear(&self) {
        if self.tx.is_none() {
            return;
        }
        self.finished.store(true, Ordering::Release);
        let (ack, wait) = oneshot::channel();
        self.send(Command::Clear(ack));
        let _ = wait.await;
    }
}

struct Worker {
    adapter: Arc<dyn ChatAdapter>,
    message: MessageRef,
    cfg: ReactionConfig,
    current: Option<String>,
    mood: Option<String>,
    finished: bool,
    pending: Option<(Instant, String)>,
    progress: Option<Instant>,
    soft_shown: bool,
    hard_shown: bool,
}
impl Worker {
    fn touch(&mut self) {
        self.progress = Some(Instant::now());
        self.soft_shown = false;
        self.hard_shown = false;
    }
    fn next_deadline(&self) -> Option<Instant> {
        let soft = self
            .progress
            .filter(|_| !self.soft_shown)
            .map(|at| at + Duration::from_millis(self.cfg.timing.stall_soft_ms));
        let hard = self
            .progress
            .filter(|_| !self.hard_shown)
            .map(|at| at + Duration::from_millis(self.cfg.timing.stall_hard_ms));
        [self.pending.as_ref().map(|(at, _)| *at), soft, hard]
            .into_iter()
            .flatten()
            .min()
    }
    async fn apply(&mut self, emoji: String) {
        if self.current.as_deref() == Some(&emoji) {
            return;
        }
        match self.adapter.add_reaction(&self.message, &emoji).await {
            Ok(()) => {
                if let Some(old) = self.current.replace(emoji)
                    && let Err(error) = self.adapter.remove_reaction(&self.message, &old).await
                {
                    tracing::debug!(%error,"reaction removal failed");
                }
            }
            Err(error) => tracing::debug!(%error,"reaction addition failed"),
        }
    }
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Command>) {
        loop {
            let deadline = self.next_deadline();
            tokio::select! {
                biased;
                command=rx.recv()=>match command {
                    None=>break,
                    Some(Command::Queued(ack))=>{
                        if !self.finished {self.pending=None;self.apply(self.cfg.emojis.queued.clone()).await;self.touch();}
                        let _=ack.send(());
                    }
                    Some(Command::Progress(emoji))=>{
                        if !self.finished {
                            self.touch();
                            self.pending=if self.current.as_deref()==Some(&emoji) {None} else {Some((Instant::now()+Duration::from_millis(self.cfg.timing.debounce_ms),emoji))};
                        }
                    }
                    Some(Command::Touch)=>{if !self.finished {self.touch();}}
                    Some(Command::Finish{error,ack})=>{
                        if !self.finished {
                            self.finished=true;self.pending=None;self.progress=None;
                            self.apply(if error {self.cfg.emojis.error.clone()} else {self.cfg.emojis.done.clone()}).await;
                            if !error {
                                let faces=["😊","😎","🫡","🤓","😏","✌️","💪","🦾"];
                                if let Some(face)=faces.get(fastrand::usize(..faces.len())) {
                                    match self.adapter.add_reaction(&self.message,face).await {
                                        Ok(())=>self.mood=Some((*face).into()),
                                        Err(error)=>tracing::debug!(%error,"mood reaction failed"),
                                    }
                                }
                            }
                        }
                        let _=ack.send(());
                    }
                    Some(Command::Clear(ack))=>{
                        self.finished=true;self.pending=None;self.progress=None;
                        for emoji in [self.current.take(),self.mood.take()].into_iter().flatten() {
                            if let Err(error)=self.adapter.remove_reaction(&self.message,&emoji).await {tracing::debug!(%error,"reaction cleanup failed");}
                        }
                        let _=ack.send(());
                    }
                },
                ()=async {match deadline {Some(at)=>tokio::time::sleep_until(at).await,None=>futures::future::pending().await}}=>{
                    let now=Instant::now();
                    if self.pending.as_ref().is_some_and(|(at,_)|*at<=now) && let Some((_,emoji))=self.pending.take() {self.apply(emoji).await;}
                    if let Some(progress)=self.progress {
                        if !self.hard_shown && now>=progress+Duration::from_millis(self.cfg.timing.stall_hard_ms) {
                            self.soft_shown=true;self.hard_shown=true;self.apply(self.cfg.emojis.stall_hard.clone()).await;
                        } else if !self.soft_shown && now>=progress+Duration::from_millis(self.cfg.timing.stall_soft_ms) {
                            self.soft_shown=true;self.apply(self.cfg.emojis.stall_soft.clone()).await;
                        }
                    }
                }
            }
        }
    }
}

/// Observe one run's real-time effect and streaming events without changing its outcome.
pub struct ReactionHook {
    ctl: Arc<StatusReactions>,
}
impl ReactionHook {
    /// Bind one controller to one Agent run through `AgentRunner::add_hook`.
    pub fn new(ctl: Arc<StatusReactions>) -> Self {
        Self { ctl }
    }
}
impl AgentHook for ReactionHook {
    async fn on_dispatch(&self, _: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        match event.kind {
            EffectKind::ToolCall { name, .. } => self.ctl.set_tool(name.as_str()),
            EffectKind::Completion { .. } => self.ctl.set_thinking(),
            _ => {}
        }
        DispatchAction::Proceed
    }
    async fn on_outcome(&self, _: &HookContext, event: OutcomeEvent<'_>) -> OutcomeAction {
        if matches!(event.kind, EffectKind::ToolCall { .. }) {
            self.ctl.set_thinking();
        }
        OutcomeAction::Proceed
    }
    async fn on_reasoning_delta(
        &self,
        _: &HookContext,
        _: ReasoningDelta<'_>,
    ) -> ObservationAction {
        self.ctl.set_thinking();
        ObservationAction::Continue
    }
    async fn on_text_delta(&self, _: &HookContext, _: TextDelta<'_>) -> ObservationAction {
        self.ctl.touch();
        ObservationAction::Continue
    }
}

#[cfg(test)]
mod tests;
