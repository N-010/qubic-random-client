use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::io::{Error as IoError, ErrorKind};
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike as _, Timelike as _, Utc, Weekday};
use scapi::{QubicId, QubicWallet, build_ticket_tx_bytes};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::AppResult;
use crate::backend::{BackendError, ContractFunctionRequest, NetworkBackend, TickInfo};
use crate::console;
use crate::contract::{
    GET_PROVIDER_STATUS_FUNCTION, ProviderStatus, RANDOM_CONTRACT_INDEX,
    REVEAL_AND_COMMIT_PROCEDURE, RevealAndCommitInput, SlotKey,
};
use crate::entropy::{commit_digest, fill_secure_bits};

const STREAM_COUNT: u32 = 3;
const SEND_LEAD_TICKS: u32 = 6;
const STATUS_CONFIRMATIONS_REQUIRED: u8 = 3;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const BACKEND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BROADCAST_ATTEMPT: Duration = Duration::from_secs(2);
const MIN_BROADCAST_ATTEMPT: Duration = Duration::from_millis(100);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(90);

type NetworkTask<T> = JoinHandle<Result<T, String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallKind {
    FirstCommit,
    RevealAndCommit,
    TerminalReveal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BroadcastState {
    Ready,
    Broadcasting,
    Retry,
    Accepted,
    Failed,
}

struct PlannedCall {
    target_tick: u32,
    tx_bytes: Vec<u8>,
    kind: CallKind,
    state: BroadcastState,
    broadcast: Option<NetworkTask<String>>,
}

struct Chain {
    first_target: u32,
    last_target: u32,
    confirmed_through: Option<u32>,
    status_suspicion: Option<StatusSuspicion>,
    outstanding_preimage: Box<[u8; 512]>,
    calls: VecDeque<PlannedCall>,
}

impl Chain {
    fn owns_target(&self, target: u32) -> bool {
        target >= self.first_target
            && target <= self.last_target
            && (target - self.first_target).is_multiple_of(STREAM_COUNT)
    }

    fn latest_target_before(&self, tick: u32) -> Option<u32> {
        let upper_bound = tick.checked_sub(1)?.min(self.last_target);
        let distance = upper_bound.checked_sub(self.first_target)?;
        let offset = distance
            .checked_div(STREAM_COUNT)?
            .checked_mul(STREAM_COUNT)?;
        self.first_target.checked_add(offset)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusEvidence {
    Absent,
    Foreign { last_update_tick: u32 },
    Lag { missing_target: u32 },
}

impl StatusEvidence {
    fn agrees_with(self, next: Self) -> bool {
        match (self, next) {
            (Self::Absent, Self::Absent) => true,
            (
                Self::Foreign {
                    last_update_tick: previous,
                },
                Self::Foreign {
                    last_update_tick: next,
                },
            ) => next >= previous,
            (
                Self::Lag {
                    missing_target: previous,
                },
                Self::Lag {
                    missing_target: next,
                },
            ) => next == previous,
            (Self::Absent, Self::Foreign { .. } | Self::Lag { .. })
            | (Self::Foreign { .. }, Self::Absent | Self::Lag { .. })
            | (Self::Lag { .. }, Self::Absent | Self::Foreign { .. }) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatusSuspicion {
    evidence: StatusEvidence,
    confirmations: u8,
    first_requested_tick: u32,
    last_requested_tick: u32,
}

impl StatusSuspicion {
    fn new(evidence: StatusEvidence, requested_tick: u32) -> Self {
        Self {
            evidence,
            confirmations: 1,
            first_requested_tick: requested_tick,
            last_requested_tick: requested_tick,
        }
    }

    fn observe(self, evidence: StatusEvidence, requested_tick: u32) -> Self {
        if requested_tick <= self.last_requested_tick {
            return self;
        }
        if !self.evidence.agrees_with(evidence) {
            return Self::new(evidence, requested_tick);
        }
        Self {
            evidence,
            confirmations: self
                .confirmations
                .saturating_add(1)
                .min(STATUS_CONFIRMATIONS_REQUIRED),
            first_requested_tick: self.first_requested_tick,
            last_requested_tick: requested_tick,
        }
    }

    fn is_confirmed(self) -> bool {
        self.confirmations >= STATUS_CONFIRMATIONS_REQUIRED
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainReason {
    Epoch,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainOutcome {
    NothingToDrain,
    Accepted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Waiting {
        absence_after_tick: u32,
        absence_observed: bool,
    },
    Starting {
        first_target: u32,
    },
    Active,
    Restarting {
        frozen_tail: u32,
        absence_after_tick: u32,
        absence_observed: bool,
        drain_reason: Option<DrainReason>,
    },
    Draining {
        reason: DrainReason,
        terminal_target: u32,
    },
    Drained(DrainOutcome),
}

struct ManagedSlot {
    key: SlotKey,
    state: SlotState,
    chain: Option<Chain>,
    restart_at: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EpochPhase {
    Warmup { epoch: u32, ready_tick: u32 },
    Active { epoch: u32 },
    Draining { epoch: u32 },
}

struct PendingStatusQuery {
    epoch: u32,
    requested_tick: u32,
    task: NetworkTask<ProviderStatus>,
}

struct StatusObservation {
    epoch: u32,
    requested_tick: u32,
    status: ProviderStatus,
}

struct PendingTickCheck {
    target_tick: u32,
    task: NetworkTask<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendOutcome {
    Ok,
    Failed,
    Empty,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SendStats {
    ok: u64,
    failed: u64,
    empty: u64,
}

impl SendStats {
    fn increment(&mut self, outcome: SendOutcome) {
        match outcome {
            SendOutcome::Ok => self.ok = self.ok.saturating_add(1),
            SendOutcome::Failed => self.failed = self.failed.saturating_add(1),
            SendOutcome::Empty => self.empty = self.empty.saturating_add(1),
        }
    }

    fn decrement(&mut self, outcome: SendOutcome) {
        match outcome {
            SendOutcome::Ok => self.ok = self.ok.saturating_sub(1),
            SendOutcome::Failed => self.failed = self.failed.saturating_sub(1),
            SendOutcome::Empty => self.empty = self.empty.saturating_sub(1),
        }
    }

    fn publish(self) {
        console::set_send_stats(self.ok, self.failed, self.empty);
    }
}

pub struct ProviderEngine {
    backend: Arc<dyn NetworkBackend>,
    wallet: QubicWallet,
    collateral: u64,
    slots: [ManagedSlot; 3],
    status_query: Option<PendingStatusQuery>,
    epoch_phase: Option<EpochPhase>,
    epoch_stop_lead_time_secs: u64,
    epoch_resume_delay_ticks: u32,
    empty_tick_check_interval: Duration,
    reveal_check_delay_ticks: u32,
    send_stats: SendStats,
    send_outcomes: BTreeMap<u32, SendOutcome>,
    pending_tick_checks: BTreeSet<u32>,
    checked_target_ticks: BTreeSet<u32>,
    tick_check: Option<PendingTickCheck>,
    next_tick_check_at: Instant,
}

impl ProviderEngine {
    pub fn new(
        backend: Arc<dyn NetworkBackend>,
        wallet: QubicWallet,
        collateral: u64,
        epoch_stop_lead_time_secs: u64,
        epoch_resume_delay_ticks: u32,
        empty_tick_check_interval_ms: u64,
        reveal_check_delay_ticks: u32,
    ) -> Self {
        let tier = collateral_tier(collateral);
        Self {
            backend,
            wallet,
            collateral,
            slots: std::array::from_fn(|stream| ManagedSlot {
                key: SlotKey {
                    stream: stream as u8,
                    collateral_tier: tier,
                },
                state: SlotState::Waiting {
                    absence_after_tick: 0,
                    absence_observed: false,
                },
                chain: None,
                restart_at: 0,
            }),
            status_query: None,
            epoch_phase: None,
            epoch_stop_lead_time_secs,
            epoch_resume_delay_ticks,
            empty_tick_check_interval: Duration::from_millis(empty_tick_check_interval_ms),
            reveal_check_delay_ticks,
            send_stats: SendStats::default(),
            send_outcomes: BTreeMap::new(),
            pending_tick_checks: BTreeSet::new(),
            checked_target_ticks: BTreeSet::new(),
            tick_check: None,
            next_tick_check_at: Instant::now(),
        }
    }

    fn record_send_outcome(&mut self, target_tick: u32, next: SendOutcome) {
        let previous = self.send_outcomes.get(&target_tick).copied();
        let transition_allowed = match (previous, next) {
            (None, SendOutcome::Ok | SendOutcome::Failed | SendOutcome::Empty)
            | (Some(SendOutcome::Ok), SendOutcome::Failed | SendOutcome::Empty)
            | (Some(SendOutcome::Failed), SendOutcome::Empty) => true,
            (Some(SendOutcome::Ok), SendOutcome::Ok)
            | (Some(SendOutcome::Failed), SendOutcome::Ok | SendOutcome::Failed)
            | (
                Some(SendOutcome::Empty),
                SendOutcome::Ok | SendOutcome::Failed | SendOutcome::Empty,
            ) => false,
        };
        if !transition_allowed {
            return;
        }
        if let Some(previous) = previous {
            self.send_stats.decrement(previous);
        }
        self.send_stats.increment(next);
        self.send_outcomes.insert(target_tick, next);
        self.send_stats.publish();
    }

    fn queue_tick_check(&mut self, target_tick: u32) {
        if !self.checked_target_ticks.contains(&target_tick) {
            self.pending_tick_checks.insert(target_tick);
        }
    }

    fn mark_financial_failure(&mut self, target_tick: u32) {
        self.record_send_outcome(target_tick, SendOutcome::Failed);
        self.queue_tick_check(target_tick);
    }

    pub async fn run(
        mut self,
        shutdown: impl Future<Output = AppResult<()>> + Send + 'static,
    ) -> AppResult<()> {
        let cancellation = CancellationToken::new();
        let observer_token = cancellation.clone();
        let mut shutdown_task = Some(tokio::spawn(async move {
            let result = shutdown.await;
            let requested_at = Instant::now();
            observer_token.cancel();
            (result, requested_at)
        }));
        let mut shutting_down = false;
        let mut shutdown_deadline = None;

        loop {
            if shutting_down && self.shutdown_complete() {
                if self.shutdown_failed() {
                    return Err("one or more final reveal chains could not be submitted".into());
                }
                console::log_info("All managed final reveal chains were submitted");
                return Ok(());
            }
            if shutdown_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err("shutdown timed out before final reveals could be submitted".into());
            }

            sleep(POLL_INTERVAL).await;
            if let Err(err) = self
                .cycle(shutting_down || cancellation.is_cancelled())
                .await
            {
                console::log_warn(format!("Provider cycle failed: {err}"));
            }

            let observer_finished = shutdown_task.as_ref().is_some_and(JoinHandle::is_finished);
            if !shutting_down && observer_finished {
                let observer = shutdown_task
                    .take()
                    .ok_or("shutdown observer disappeared")?;
                let (result, requested_at) = observer
                    .await
                    .map_err(|err| format!("shutdown observer failed to join: {err}"))?;
                result?;
                shutting_down = true;
                shutdown_deadline = Some(requested_at + SHUTDOWN_TIMEOUT);
                console::log_info("Shutdown requested; normal planning is frozen");
            }
        }
    }

    async fn cycle(&mut self, shutting_down: bool) -> AppResult<()> {
        let tick = self
            .backend_call("get tick info", self.backend.tick_info())
            .await?;
        console::set_tick_value(tick.epoch, tick.tick);
        self.update_epoch_phase(&tick, Utc::now());
        self.harvest_tick_check().await;
        self.ensure_tick_check(tick.tick);

        let observation = self.harvest_status_query().await;
        self.ensure_status_query(tick.epoch, tick.tick);
        for index in 0..self.slots.len() {
            self.harvest_broadcasts(index).await;
        }
        if let Some(observation) = observation.as_ref()
            && observation.epoch == tick.epoch
        {
            for index in 0..self.slots.len() {
                self.apply_status(index, observation);
            }
        }

        let epoch_draining = matches!(
            self.epoch_phase,
            Some(EpochPhase::Draining { epoch }) if epoch == tick.epoch
        );
        if shutting_down || epoch_draining {
            let reason = if shutting_down {
                DrainReason::Shutdown
            } else {
                DrainReason::Epoch
            };
            for index in 0..self.slots.len() {
                self.enter_drain(index, reason)?;
            }
        } else if matches!(
            self.epoch_phase,
            Some(EpochPhase::Active { epoch }) if epoch == tick.epoch
        ) {
            for index in 0..self.slots.len() {
                self.start_if_ready(index, tick.tick)?;
                self.extend_chain(index, tick.tick)?;
            }
        }

        for index in 0..self.slots.len() {
            self.expire_calls(index, tick.tick);
            self.dispatch_calls(index, tick.tick, tick.tick_duration_ms);
            self.finish_drain(index);
            self.finish_status_restart(index, tick.tick);
            self.prune_calls(index, tick.tick);
        }
        Ok(())
    }

    fn update_epoch_phase(&mut self, tick: &TickInfo, now: chrono::DateTime<Utc>) {
        let previous_epoch = self.epoch_phase.map(|phase| match phase {
            EpochPhase::Warmup { epoch, .. }
            | EpochPhase::Active { epoch }
            | EpochPhase::Draining { epoch } => epoch,
        });
        if previous_epoch != Some(tick.epoch) {
            self.reset_slots();
            let ready_tick = tick
                .initial_tick
                .saturating_add(self.epoch_resume_delay_ticks);
            self.epoch_phase = Some(EpochPhase::Warmup {
                epoch: tick.epoch,
                ready_tick,
            });
            console::log_info(format!(
                "Epoch {} observed; enrollment is paused through tick {}",
                tick.epoch, ready_tick
            ));
        }

        if matches!(
            self.epoch_phase,
            Some(EpochPhase::Warmup { epoch, ready_tick })
                if epoch == tick.epoch && tick.tick >= ready_tick
        ) {
            self.epoch_phase = Some(EpochPhase::Active { epoch: tick.epoch });
            console::log_info(format!("Epoch {} warmup is complete", tick.epoch));
        }

        if matches!(self.epoch_phase, Some(EpochPhase::Active { epoch }) if epoch == tick.epoch)
            && is_epoch_stop_window(now, self.epoch_stop_lead_time_secs)
        {
            self.epoch_phase = Some(EpochPhase::Draining { epoch: tick.epoch });
            console::log_info(format!(
                "Epoch {} is approaching its wall-clock boundary; draining managed streams",
                tick.epoch
            ));
        }
    }

    fn reset_slots(&mut self) {
        if let Some(query) = self.status_query.take() {
            query.task.abort();
        }
        if let Some(check) = self.tick_check.take() {
            check.task.abort();
        }
        self.pending_tick_checks.clear();
        self.checked_target_ticks.clear();
        self.send_outcomes.clear();
        self.next_tick_check_at = Instant::now();
        for index in 0..self.slots.len() {
            abort_chain(self.slots[index].chain.take());
            self.slots[index].state = SlotState::Waiting {
                absence_after_tick: 0,
                absence_observed: false,
            };
            self.slots[index].restart_at = 0;
        }
    }

    async fn harvest_status_query(&mut self) -> Option<StatusObservation> {
        if !self
            .status_query
            .as_ref()
            .is_some_and(|query| query.task.is_finished())
        {
            return None;
        }
        let query = self.status_query.take()?;
        match join_network_task(query.task).await {
            Ok(status) => Some(StatusObservation {
                epoch: query.epoch,
                requested_tick: query.requested_tick,
                status,
            }),
            Err(err) => {
                console::log_warn(format!(
                    "Provider status is temporarily unavailable; current chains continue: {err}"
                ));
                None
            }
        }
    }

    fn ensure_status_query(&mut self, epoch: u32, requested_tick: u32) {
        if self.status_query.is_some() {
            return;
        }
        let backend = Arc::clone(&self.backend);
        let public_key = self.wallet.public_key.0.to_vec();
        self.status_query = Some(PendingStatusQuery {
            epoch,
            requested_tick,
            task: spawn_network_task("query provider status", async move {
                let output = backend
                    .query_contract_function(ContractFunctionRequest {
                        contract_index: RANDOM_CONTRACT_INDEX,
                        input_type: GET_PROVIDER_STATUS_FUNCTION,
                        input: public_key,
                    })
                    .await
                    .map_err(|err| format!("provider-status query failed: {err}"))?;
                ProviderStatus::decode(&output)
                    .map_err(|err| format!("provider-status response is malformed: {err}"))
            }),
        });
    }

    fn apply_status(&mut self, index: usize, observation: &StatusObservation) {
        if matches!(
            self.slots[index].state,
            SlotState::Draining { .. } | SlotState::Drained(_)
        ) {
            return;
        }

        let observed_target = observation
            .status
            .slot(self.slots[index].key)
            .map(|slot| slot.last_update_tick);
        match self.slots[index].state {
            SlotState::Waiting {
                absence_after_tick, ..
            } => match observed_target {
                None if observation.requested_tick >= absence_after_tick => {
                    self.slots[index].state = SlotState::Waiting {
                        absence_after_tick,
                        absence_observed: true,
                    };
                }
                None => {}
                Some(_) => {
                    self.slots[index].state = SlotState::Waiting {
                        absence_after_tick: observation.requested_tick.saturating_add(1),
                        absence_observed: false,
                    };
                }
            },
            SlotState::Starting { first_target } => match observed_target {
                Some(confirmed_tick) if self.chain_owns_target(index, confirmed_tick) => {
                    self.slots[index].state = SlotState::Active;
                    console::log_info(format!(
                        "Stream {} is active at contract tick {}",
                        self.slots[index].key.stream, confirmed_tick
                    ));
                    self.apply_confirmation_watermark(index, confirmed_tick, observation);
                }
                Some(last_update_tick) if observation.requested_tick > first_target => self
                    .observe_status_evidence(
                        index,
                        StatusEvidence::Foreign { last_update_tick },
                        observation.requested_tick,
                    ),
                None if observation.requested_tick > first_target => self.observe_status_evidence(
                    index,
                    StatusEvidence::Absent,
                    observation.requested_tick,
                ),
                Some(_) | None => {}
            },
            SlotState::Active => match observed_target {
                Some(confirmed_tick) if self.chain_owns_target(index, confirmed_tick) => {
                    self.apply_confirmation_watermark(index, confirmed_tick, observation);
                }
                Some(last_update_tick) => self.observe_status_evidence(
                    index,
                    StatusEvidence::Foreign { last_update_tick },
                    observation.requested_tick,
                ),
                None => self.observe_status_evidence(
                    index,
                    StatusEvidence::Absent,
                    observation.requested_tick,
                ),
            },
            SlotState::Restarting {
                frozen_tail,
                absence_after_tick,
                absence_observed: _,
                drain_reason,
            } => match observed_target {
                None if observation.requested_tick >= absence_after_tick => {
                    self.slots[index].state = SlotState::Restarting {
                        frozen_tail,
                        absence_after_tick,
                        absence_observed: true,
                        drain_reason,
                    };
                }
                None => {}
                Some(_) => {
                    self.slots[index].state = SlotState::Restarting {
                        frozen_tail,
                        absence_after_tick: observation.requested_tick.saturating_add(1),
                        absence_observed: false,
                        drain_reason,
                    };
                }
            },
            SlotState::Draining { .. } | SlotState::Drained(_) => unreachable!(),
        }
    }

    fn apply_confirmation_watermark(
        &mut self,
        index: usize,
        confirmed_tick: u32,
        observation: &StatusObservation,
    ) {
        let missing_target = {
            let Some(chain) = self.slots[index].chain.as_mut() else {
                return;
            };
            if chain
                .confirmed_through
                .is_some_and(|previous| confirmed_tick < previous)
            {
                return;
            }

            let advanced = chain.confirmed_through != Some(confirmed_tick);
            if advanced {
                chain.confirmed_through = Some(confirmed_tick);
                chain.status_suspicion = None;
            }
            let Some(latest_due) = chain.latest_target_before(observation.requested_tick) else {
                chain.status_suspicion = None;
                return;
            };
            if confirmed_tick >= latest_due {
                chain.status_suspicion = None;
                return;
            }
            let Some(missing_target) = confirmed_tick.checked_add(STREAM_COUNT) else {
                return;
            };
            missing_target
        };

        self.observe_status_evidence(
            index,
            StatusEvidence::Lag { missing_target },
            observation.requested_tick,
        );
    }

    fn observe_status_evidence(
        &mut self,
        index: usize,
        evidence: StatusEvidence,
        requested_tick: u32,
    ) {
        let Some(chain) = self.slots[index].chain.as_mut() else {
            return;
        };
        if chain
            .status_suspicion
            .is_some_and(|current| requested_tick <= current.last_requested_tick)
        {
            return;
        }
        let suspicion = chain.status_suspicion.map_or_else(
            || StatusSuspicion::new(evidence, requested_tick),
            |current| current.observe(evidence, requested_tick),
        );
        chain.status_suspicion = Some(suspicion);

        if !suspicion.is_confirmed() {
            console::log_info(format!(
                "Stream {} status suspicion {}/{} at request tick {}: {}",
                self.slots[index].key.stream,
                suspicion.confirmations,
                STATUS_CONFIRMATIONS_REQUIRED,
                requested_tick,
                status_evidence_description(suspicion.evidence)
            ));
            return;
        }

        let (absence_after_tick, absence_observed) = match suspicion.evidence {
            StatusEvidence::Absent => (suspicion.last_requested_tick, true),
            StatusEvidence::Foreign { .. } | StatusEvidence::Lag { .. } => {
                (suspicion.last_requested_tick.saturating_add(1), false)
            }
        };
        if let Some(target_tick) = self.financial_failure_target(index, suspicion.evidence) {
            self.mark_financial_failure(target_tick);
        }
        self.begin_status_restart(
            index,
            absence_after_tick,
            absence_observed,
            &format!(
                "{} confirmed by {} observations from request tick {} through {}",
                status_evidence_description(suspicion.evidence),
                suspicion.confirmations,
                suspicion.first_requested_tick,
                suspicion.last_requested_tick
            ),
        );
    }

    fn financial_failure_target(&self, index: usize, evidence: StatusEvidence) -> Option<u32> {
        let chain = self.slots[index].chain.as_ref()?;
        let target_tick = match evidence {
            StatusEvidence::Absent => chain.confirmed_through?.checked_add(STREAM_COUNT)?,
            StatusEvidence::Lag { missing_target } => missing_target,
            StatusEvidence::Foreign { .. } => return None,
        };
        (target_tick > chain.first_target && target_tick <= chain.last_target)
            .then_some(target_tick)
    }

    fn chain_owns_target(&self, index: usize, target: u32) -> bool {
        self.slots[index]
            .chain
            .as_ref()
            .is_some_and(|chain| chain.owns_target(target))
    }

    fn begin_status_restart(
        &mut self,
        index: usize,
        absence_after_tick: u32,
        absence_observed: bool,
        reason: &str,
    ) {
        let Some(frozen_tail) = self.slots[index]
            .chain
            .as_ref()
            .map(|chain| chain.last_target)
        else {
            return;
        };
        let Some(restart_at) = frozen_tail.checked_add(STREAM_COUNT) else {
            abort_chain(self.slots[index].chain.take());
            self.slots[index].restart_at = u32::MAX;
            self.slots[index].state = SlotState::Drained(DrainOutcome::Failed);
            console::log_warn(format!(
                "Stream {} has no future restart tick in this epoch",
                self.slots[index].key.stream
            ));
            return;
        };

        self.slots[index].restart_at = restart_at;
        self.slots[index].state = SlotState::Restarting {
            frozen_tail,
            absence_after_tick,
            absence_observed,
            drain_reason: None,
        };
        console::log_warn(format!(
            "Stream {} froze its signed tail at tick {frozen_tail}: {reason}",
            self.slots[index].key.stream
        ));
    }

    fn lose_chain(
        &mut self,
        index: usize,
        absence_after_tick: u32,
        absence_observed: bool,
        reason: &str,
    ) {
        let old_tail = self.slots[index]
            .chain
            .as_ref()
            .map_or(self.slots[index].restart_at, |chain| chain.last_target);
        abort_chain(self.slots[index].chain.take());
        let Some(restart_at) = old_tail.checked_add(STREAM_COUNT) else {
            self.slots[index].restart_at = u32::MAX;
            self.slots[index].state = SlotState::Drained(DrainOutcome::Failed);
            console::log_warn(format!(
                "Stream {} has no future restart tick in this epoch",
                self.slots[index].key.stream
            ));
            return;
        };
        self.slots[index].restart_at = restart_at;
        self.slots[index].state = SlotState::Waiting {
            absence_after_tick,
            absence_observed,
        };
        console::log_warn(format!(
            "Stream {} stopped its local chain: {reason}",
            self.slots[index].key.stream
        ));
    }

    fn start_if_ready(&mut self, index: usize, current_tick: u32) -> AppResult<()> {
        let SlotState::Waiting {
            absence_observed: true,
            ..
        } = self.slots[index].state
        else {
            return Ok(());
        };
        let earliest = current_tick
            .checked_add(SEND_LEAD_TICKS)
            .ok_or("initial target tick overflowed")?
            .max(self.slots[index].restart_at);
        let target = next_stream_tick(earliest, self.slots[index].key.stream)
            .ok_or("initial stream target overflowed")?;
        let next_preimage = random_preimage();
        let input = RevealAndCommitInput {
            reveal: [0; 512],
            commit: commit_digest(&next_preimage),
        };
        let tx_bytes = self.build_transaction(input, target, self.slots[index].key)?;
        self.slots[index].chain = Some(Chain {
            first_target: target,
            last_target: target,
            confirmed_through: None,
            status_suspicion: None,
            outstanding_preimage: Box::new(next_preimage),
            calls: VecDeque::from([PlannedCall {
                target_tick: target,
                tx_bytes,
                kind: CallKind::FirstCommit,
                state: BroadcastState::Ready,
                broadcast: None,
            }]),
        });
        self.slots[index].state = SlotState::Starting {
            first_target: target,
        };
        console::log_info(format!(
            "Stream {} fresh chain starts at tick {target}",
            self.slots[index].key.stream
        ));
        Ok(())
    }

    fn extend_chain(&mut self, index: usize, current_tick: u32) -> AppResult<()> {
        if !matches!(
            self.slots[index].state,
            SlotState::Starting { .. } | SlotState::Active
        ) {
            return Ok(());
        }
        let horizon = current_tick
            .checked_add(SEND_LEAD_TICKS)
            .ok_or("planning horizon overflowed")?;
        loop {
            let Some((target, reveal)) = self.slots[index].chain.as_ref().and_then(|chain| {
                chain
                    .last_target
                    .checked_add(STREAM_COUNT)
                    .filter(|target| *target <= horizon)
                    .map(|target| (target, *chain.outstanding_preimage))
            }) else {
                return Ok(());
            };
            let next_preimage = random_preimage();
            let input = RevealAndCommitInput {
                reveal,
                commit: commit_digest(&next_preimage),
            };
            let tx_bytes = self.build_transaction(input, target, self.slots[index].key)?;
            let chain = self.slots[index]
                .chain
                .as_mut()
                .ok_or("active chain disappeared")?;
            chain.calls.push_back(PlannedCall {
                target_tick: target,
                tx_bytes,
                kind: CallKind::RevealAndCommit,
                state: BroadcastState::Ready,
                broadcast: None,
            });
            chain.last_target = target;
            *chain.outstanding_preimage = next_preimage;
        }
    }

    fn enter_drain(&mut self, index: usize, reason: DrainReason) -> AppResult<()> {
        match self.slots[index].state {
            SlotState::Draining { .. } | SlotState::Drained(_) => return Ok(()),
            SlotState::Waiting { .. } => {
                self.slots[index].state = SlotState::Drained(DrainOutcome::NothingToDrain);
                return Ok(());
            }
            SlotState::Restarting {
                frozen_tail,
                absence_after_tick,
                absence_observed,
                drain_reason,
            } => {
                let drain_reason = match (drain_reason, reason) {
                    (_, DrainReason::Shutdown) => DrainReason::Shutdown,
                    (Some(DrainReason::Shutdown), DrainReason::Epoch) => DrainReason::Shutdown,
                    (Some(DrainReason::Epoch) | None, DrainReason::Epoch) => DrainReason::Epoch,
                };
                self.slots[index].state = SlotState::Restarting {
                    frozen_tail,
                    absence_after_tick,
                    absence_observed,
                    drain_reason: Some(drain_reason),
                };
                return Ok(());
            }
            SlotState::Starting { .. } | SlotState::Active => {}
        }

        let chain = self.slots[index]
            .chain
            .as_ref()
            .ok_or("managed stream has no local chain")?;
        let terminal_target = chain
            .last_target
            .checked_add(STREAM_COUNT)
            .ok_or("terminal reveal target overflowed")?;
        let input = RevealAndCommitInput {
            reveal: *chain.outstanding_preimage,
            commit: [0; 32],
        };
        let tx_bytes = self.build_transaction(input, terminal_target, self.slots[index].key)?;
        let chain = self.slots[index]
            .chain
            .as_mut()
            .ok_or("managed stream has no local chain")?;
        chain.calls.push_back(PlannedCall {
            target_tick: terminal_target,
            tx_bytes,
            kind: CallKind::TerminalReveal,
            state: BroadcastState::Ready,
            broadcast: None,
        });
        self.slots[index].state = SlotState::Draining {
            reason,
            terminal_target,
        };
        console::log_info(format!(
            "Stream {} terminal reveal is scheduled at tick {terminal_target}",
            self.slots[index].key.stream
        ));
        Ok(())
    }

    fn expire_calls(&mut self, index: usize, current_tick: u32) {
        let mut expired = false;
        let mut first_expired_normal = None;
        let mut attempted_terminal = None;
        if let Some(chain) = self.slots[index].chain.as_mut() {
            for call in &mut chain.calls {
                if current_tick < call.target_tick
                    || matches!(
                        call.state,
                        BroadcastState::Accepted | BroadcastState::Failed
                    )
                {
                    continue;
                }
                if let Some(task) = call.broadcast.take() {
                    task.abort();
                }
                match call.kind {
                    CallKind::FirstCommit => {}
                    CallKind::RevealAndCommit => {
                        first_expired_normal.get_or_insert(call.target_tick);
                    }
                    CallKind::TerminalReveal
                        if matches!(
                            call.state,
                            BroadcastState::Broadcasting | BroadcastState::Retry
                        ) =>
                    {
                        attempted_terminal = Some(call.target_tick);
                    }
                    CallKind::TerminalReveal => {}
                }
                call.state = BroadcastState::Failed;
                expired = true;
            }
        }
        if !expired {
            return;
        }
        let financial_failure = match self.slots[index].state {
            SlotState::Starting { .. } | SlotState::Active => first_expired_normal,
            SlotState::Draining { .. } => first_expired_normal.or(attempted_terminal),
            SlotState::Restarting { .. } | SlotState::Waiting { .. } | SlotState::Drained(_) => {
                None
            }
        };
        if let Some(target_tick) = financial_failure {
            self.mark_financial_failure(target_tick);
        }
        match self.slots[index].state {
            SlotState::Starting { .. } | SlotState::Active => self.lose_chain(
                index,
                current_tick.saturating_add(1),
                false,
                "a required target tick expired before backend acceptance",
            ),
            SlotState::Restarting { .. } => {}
            SlotState::Draining { .. } => self.complete_drain(index, DrainOutcome::Failed),
            SlotState::Waiting { .. } | SlotState::Drained(_) => {}
        }
    }

    fn dispatch_calls(&mut self, index: usize, current_tick: u32, tick_duration_ms: u32) {
        let horizon = current_tick.saturating_add(SEND_LEAD_TICKS);
        let drain = match self.slots[index].state {
            SlotState::Draining {
                reason,
                terminal_target,
            } => Some((reason, terminal_target)),
            _ => None,
        };
        let terminal_ready = drain.is_none_or(|(_, terminal_target)| {
            self.slots[index].chain.as_ref().is_none_or(|chain| {
                chain.calls.iter().all(|call| {
                    call.kind == CallKind::TerminalReveal
                        || call.target_tick > terminal_target
                        || call.state == BroadcastState::Accepted
                })
            })
        });
        let key = self.slots[index].key;
        let mut failed_terminal_target = None;
        let Some(chain) = self.slots[index].chain.as_mut() else {
            return;
        };

        for call in &mut chain.calls {
            if current_tick >= call.target_tick || call.target_tick > horizon {
                continue;
            }
            if call.kind == CallKind::TerminalReveal && !terminal_ready {
                continue;
            }
            if !matches!(call.state, BroadcastState::Ready | BroadcastState::Retry) {
                continue;
            }
            if call.kind == CallKind::TerminalReveal
                && matches!(drain, Some((DrainReason::Shutdown, _)))
                && call.state == BroadcastState::Retry
            {
                call.state = BroadcastState::Failed;
                failed_terminal_target = Some(call.target_tick);
                continue;
            }

            let backend = Arc::clone(&self.backend);
            let tx_bytes = call.tx_bytes.clone();
            let attempt_timeout =
                broadcast_attempt_timeout(call.target_tick - current_tick, tick_duration_ms);
            call.broadcast = Some(spawn_network_task_with_timeout(
                "broadcast transaction",
                attempt_timeout,
                async move {
                    backend
                        .broadcast_transaction(tx_bytes)
                        .await
                        .map_err(|err| err.to_string())
                },
            ));
            call.state = BroadcastState::Broadcasting;
            console::log_info(format!(
                "Broadcast for stream {} tick {} started",
                key.stream, call.target_tick,
            ));
        }
        if let Some(target_tick) = failed_terminal_target {
            self.mark_financial_failure(target_tick);
        }
    }

    async fn harvest_broadcasts(&mut self, index: usize) {
        let key = self.slots[index].key;
        let shutdown_drain = matches!(
            self.slots[index].state,
            SlotState::Draining {
                reason: DrainReason::Shutdown,
                ..
            }
        );
        let mut accepted_normal_targets = Vec::new();
        let mut failed_terminal_targets = Vec::new();
        let Some(chain) = self.slots[index].chain.as_mut() else {
            return;
        };
        for call in &mut chain.calls {
            if !call.broadcast.as_ref().is_some_and(JoinHandle::is_finished) {
                continue;
            }
            let Some(task) = call.broadcast.take() else {
                continue;
            };
            match join_network_task(task).await {
                Ok(tx_id) => {
                    call.state = BroadcastState::Accepted;
                    if call.kind == CallKind::RevealAndCommit {
                        accepted_normal_targets.push(call.target_tick);
                    }
                    console::log_info(format!(
                        "Backend accepted transaction {} for stream {} tick {}",
                        console::shorten_id(&tx_id),
                        key.stream,
                        call.target_tick
                    ));
                }
                Err(err) => {
                    call.state = if shutdown_drain && call.kind == CallKind::TerminalReveal {
                        failed_terminal_targets.push(call.target_tick);
                        BroadcastState::Failed
                    } else {
                        BroadcastState::Retry
                    };
                    console::log_warn(format!(
                        "Transaction for stream {} tick {} failed: {err}",
                        key.stream, call.target_tick
                    ));
                }
            }
        }
        for target_tick in accepted_normal_targets {
            self.record_send_outcome(target_tick, SendOutcome::Ok);
            self.queue_tick_check(target_tick);
        }
        for target_tick in failed_terminal_targets {
            self.mark_financial_failure(target_tick);
        }
    }

    async fn harvest_tick_check(&mut self) {
        if !self
            .tick_check
            .as_ref()
            .is_some_and(|check| check.task.is_finished())
        {
            return;
        }
        let Some(check) = self.tick_check.take() else {
            return;
        };
        match join_network_task(check.task).await {
            Ok(true) => {
                self.checked_target_ticks.insert(check.target_tick);
                console::log_info(format!(
                    "Target tick {} contains transactions",
                    check.target_tick
                ));
            }
            Ok(false) => {
                self.checked_target_ticks.insert(check.target_tick);
                self.record_send_outcome(check.target_tick, SendOutcome::Empty);
                console::log_warn(format!("Target tick {} is empty", check.target_tick));
            }
            Err(err) => {
                self.pending_tick_checks.insert(check.target_tick);
                console::log_warn(format!(
                    "Could not verify target tick {} yet: {err}",
                    check.target_tick
                ));
            }
        }
    }

    fn ensure_tick_check(&mut self, current_tick: u32) {
        let now = Instant::now();
        if self.tick_check.is_some() || now < self.next_tick_check_at {
            return;
        }
        let Some(target_tick) = self
            .pending_tick_checks
            .iter()
            .copied()
            .find(|target_tick| {
                current_tick >= target_tick.saturating_add(self.reveal_check_delay_ticks)
            })
        else {
            return;
        };
        self.pending_tick_checks.remove(&target_tick);
        let backend = Arc::clone(&self.backend);
        self.tick_check = Some(PendingTickCheck {
            target_tick,
            task: spawn_network_task("query target tick data", async move {
                backend
                    .tick_has_transactions(target_tick)
                    .await
                    .map_err(|err| err.to_string())
            }),
        });
        self.next_tick_check_at = now + self.empty_tick_check_interval;
    }

    fn finish_drain(&mut self, index: usize) {
        if !matches!(self.slots[index].state, SlotState::Draining { .. }) {
            return;
        }
        let terminal_state = self.slots[index].chain.as_ref().and_then(|chain| {
            chain
                .calls
                .iter()
                .find(|call| call.kind == CallKind::TerminalReveal)
                .map(|call| call.state)
        });
        match terminal_state {
            Some(BroadcastState::Accepted) => self.complete_drain(index, DrainOutcome::Accepted),
            Some(BroadcastState::Failed) => self.complete_drain(index, DrainOutcome::Failed),
            _ => {}
        }
    }

    fn complete_drain(&mut self, index: usize, outcome: DrainOutcome) {
        abort_chain(self.slots[index].chain.take());
        self.slots[index].state = SlotState::Drained(outcome);
    }

    fn finish_status_restart(&mut self, index: usize, current_tick: u32) {
        let SlotState::Restarting {
            frozen_tail,
            absence_after_tick,
            absence_observed,
            drain_reason,
        } = self.slots[index].state
        else {
            return;
        };
        if current_tick < frozen_tail {
            return;
        }

        abort_chain(self.slots[index].chain.take());
        if drain_reason.is_some() {
            self.slots[index].state = SlotState::Drained(DrainOutcome::NothingToDrain);
            console::log_warn(format!(
                "Stream {} finished its untrusted signed tail without a terminal reveal",
                self.slots[index].key.stream
            ));
        } else {
            self.slots[index].state = SlotState::Waiting {
                absence_after_tick,
                absence_observed,
            };
            console::log_info(format!(
                "Stream {} finished its frozen tail and is waiting for exact absence",
                self.slots[index].key.stream
            ));
        }
    }

    fn prune_calls(&mut self, index: usize, current_tick: u32) {
        if matches!(self.slots[index].state, SlotState::Draining { .. }) {
            return;
        }
        if let Some(chain) = self.slots[index].chain.as_mut() {
            chain.calls.retain(|call| {
                call.target_tick >= current_tick || call.state != BroadcastState::Accepted
            });
        }
    }

    fn build_transaction(
        &self,
        input: RevealAndCommitInput,
        target_tick: u32,
        key: SlotKey,
    ) -> AppResult<Vec<u8>> {
        if target_tick % STREAM_COUNT != u32::from(key.stream) {
            return Err(format!(
                "internal scheduling error: tick {target_tick} does not match stream {}",
                key.stream
            )
            .into());
        }
        Ok(build_ticket_tx_bytes(
            &self.wallet,
            QubicId::from_contract_id(RANDOM_CONTRACT_INDEX),
            self.collateral,
            target_tick,
            REVEAL_AND_COMMIT_PROCEDURE,
            input.encode(),
        )?)
    }

    async fn backend_call<T>(
        &self,
        operation: &'static str,
        future: impl Future<Output = Result<T, BackendError>>,
    ) -> AppResult<T> {
        timeout(BACKEND_TIMEOUT, future)
            .await
            .map_err(|_| {
                IoError::new(
                    ErrorKind::TimedOut,
                    format!("{operation} timed out after {}s", BACKEND_TIMEOUT.as_secs()),
                )
            })?
            .map_err(Into::into)
    }

    fn shutdown_complete(&self) -> bool {
        self.slots
            .iter()
            .all(|slot| matches!(slot.state, SlotState::Drained(_)))
    }

    fn shutdown_failed(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.state == SlotState::Drained(DrainOutcome::Failed))
    }
}

impl Drop for ProviderEngine {
    fn drop(&mut self) {
        if let Some(query) = self.status_query.take() {
            query.task.abort();
        }
        if let Some(check) = self.tick_check.take() {
            check.task.abort();
        }
        for slot in &mut self.slots {
            abort_chain(slot.chain.take());
        }
    }
}

fn status_evidence_description(evidence: StatusEvidence) -> String {
    match evidence {
        StatusEvidence::Absent => "provider status does not report the slot".to_string(),
        StatusEvidence::Foreign { last_update_tick } => {
            format!("provider status reports non-local update tick {last_update_tick}")
        }
        StatusEvidence::Lag { missing_target } => {
            format!("signed tick {missing_target} remains unconfirmed by contract status")
        }
    }
}

fn abort_chain(chain: Option<Chain>) {
    if let Some(mut chain) = chain {
        for call in &mut chain.calls {
            if let Some(task) = call.broadcast.take() {
                task.abort();
            }
        }
    }
}

fn spawn_network_task<T>(
    operation: &'static str,
    future: impl Future<Output = Result<T, String>> + Send + 'static,
) -> NetworkTask<T>
where
    T: Send + 'static,
{
    tokio::spawn(async move {
        timeout(BACKEND_TIMEOUT, future)
            .await
            .map_err(|_| format!("{operation} timed out after {}s", BACKEND_TIMEOUT.as_secs()))?
    })
}

fn spawn_network_task_with_timeout<T>(
    operation: &'static str,
    attempt_timeout: Duration,
    future: impl Future<Output = Result<T, String>> + Send + 'static,
) -> NetworkTask<T>
where
    T: Send + 'static,
{
    tokio::spawn(async move {
        timeout(attempt_timeout, future).await.map_err(|_| {
            format!(
                "{operation} timed out after {}ms",
                attempt_timeout.as_millis()
            )
        })?
    })
}

async fn join_network_task<T>(task: NetworkTask<T>) -> Result<T, String> {
    task.await
        .map_err(|err| format!("network task failed to join: {err}"))
        .and_then(|result| result)
}

fn collateral_tier(collateral: u64) -> u8 {
    collateral.ilog10() as u8
}

fn next_stream_tick(at_or_after: u32, stream: u8) -> Option<u32> {
    let stream = u32::from(stream);
    let offset = (stream + STREAM_COUNT - at_or_after % STREAM_COUNT) % STREAM_COUNT;
    at_or_after.checked_add(offset)
}

fn broadcast_attempt_timeout(lead_ticks: u32, tick_duration_ms: u32) -> Duration {
    let remaining_ms = u64::from(lead_ticks).saturating_mul(u64::from(tick_duration_ms.max(1)));
    let reserve_ms = remaining_ms.min(POLL_INTERVAL.as_millis() as u64);
    let budget_ms = remaining_ms.saturating_sub(reserve_ms).max(1);
    let attempt_ms = (budget_ms / 2)
        .max(MIN_BROADCAST_ATTEMPT.as_millis() as u64)
        .min(MAX_BROADCAST_ATTEMPT.as_millis() as u64)
        .min(remaining_ms.max(1));
    Duration::from_millis(attempt_ms)
}

fn is_epoch_stop_window(now: chrono::DateTime<Utc>, stop_lead_time_secs: u64) -> bool {
    const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
    const SECONDS_PER_WEEK: u64 = 7 * SECONDS_PER_DAY;
    const WEDNESDAY_NOON: u64 = 2 * SECONDS_PER_DAY + 12 * 60 * 60;

    let weekday = match now.weekday() {
        Weekday::Mon => 0,
        Weekday::Tue => 1,
        Weekday::Wed => 2,
        Weekday::Thu => 3,
        Weekday::Fri => 4,
        Weekday::Sat => 5,
        Weekday::Sun => 6,
    };
    let seconds = weekday * SECONDS_PER_DAY
        + u64::from(now.hour()) * 60 * 60
        + u64::from(now.minute()) * 60
        + u64::from(now.second());
    let until_boundary = if seconds <= WEDNESDAY_NOON {
        WEDNESDAY_NOON - seconds
    } else {
        SECONDS_PER_WEEK - (seconds - WEDNESDAY_NOON)
    };
    until_boundary <= stop_lead_time_secs.min(SECONDS_PER_WEEK)
}

fn random_preimage() -> [u8; 512] {
    let mut preimage = [0; 512];
    fill_secure_bits(&mut preimage);
    preimage
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use chrono::{TimeDelta, TimeZone as _};
    use proptest::prelude::*;

    use super::*;
    use crate::contract::ProviderSlot;

    #[derive(Default)]
    struct MockBackend {
        broadcasts: Mutex<Vec<Vec<u8>>>,
        failures_left: AtomicUsize,
        tick_checks: AtomicUsize,
        tick_results: Mutex<VecDeque<Result<bool, BackendError>>>,
    }

    #[async_trait]
    impl NetworkBackend for MockBackend {
        async fn tick_info(&self) -> Result<TickInfo, BackendError> {
            Err(BackendError::new("not used"))
        }

        async fn tick_has_transactions(&self, _tick: u32) -> Result<bool, BackendError> {
            self.tick_checks.fetch_add(1, Ordering::Relaxed);
            self.tick_results
                .lock()
                .expect("tick results lock")
                .pop_front()
                .unwrap_or(Ok(true))
        }

        async fn query_contract_function(
            &self,
            _request: ContractFunctionRequest,
        ) -> Result<Vec<u8>, BackendError> {
            Err(BackendError::new("not used"))
        }

        async fn broadcast_transaction(&self, tx_bytes: Vec<u8>) -> Result<String, BackendError> {
            self.broadcasts
                .lock()
                .expect("broadcast lock")
                .push(tx_bytes);
            if self
                .failures_left
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_sub(1)
                })
                .is_ok()
            {
                Err(BackendError::new("temporary failure"))
            } else {
                Ok("transaction-id".to_string())
            }
        }
    }

    fn engine(backend: Arc<MockBackend>) -> ProviderEngine {
        ProviderEngine::new(
            backend,
            QubicWallet::from_seed(&"a".repeat(55)).expect("valid wallet"),
            10_000,
            600,
            50,
            1,
            1,
        )
    }

    fn prepare_normal_call(engine: &mut ProviderEngine, index: usize) -> u32 {
        observe_absence(engine, index, 99);
        engine.start_if_ready(index, 100).expect("start chain");
        let first_target = engine.slots[index]
            .chain
            .as_ref()
            .expect("chain")
            .first_target;
        engine.slots[index].chain.as_mut().expect("chain").calls[0].state =
            BroadcastState::Accepted;
        engine
            .extend_chain(index, first_target.saturating_sub(SEND_LEAD_TICKS - 3))
            .expect("extend chain");
        first_target + STREAM_COUNT
    }

    fn empty_status(requested_tick: u32) -> StatusObservation {
        StatusObservation {
            epoch: 1,
            requested_tick,
            status: ProviderStatus { slots: Vec::new() },
        }
    }

    fn owned_status(
        engine: &ProviderEngine,
        index: usize,
        requested_tick: u32,
        last_update_tick: u32,
    ) -> StatusObservation {
        StatusObservation {
            epoch: 1,
            requested_tick,
            status: ProviderStatus {
                slots: vec![ProviderSlot {
                    key: engine.slots[index].key,
                    locked_collateral: engine.collateral,
                    contributed_to_entropy: false,
                    last_update_tick,
                }],
            },
        }
    }

    fn prepare_active_tail(engine: &mut ProviderEngine, index: usize) -> (u32, u32) {
        observe_absence(engine, index, 99);
        engine.start_if_ready(index, 100).expect("start chain");
        let first_target = engine.slots[index]
            .chain
            .as_ref()
            .expect("chain")
            .first_target;
        engine
            .extend_chain(index, first_target)
            .expect("extend signed tail");
        let observation = owned_status(engine, index, first_target + 1, first_target);
        engine.apply_status(index, &observation);
        assert_eq!(engine.slots[index].state, SlotState::Active);
        let last_target = engine.slots[index]
            .chain
            .as_ref()
            .expect("chain")
            .last_target;
        (first_target, last_target)
    }

    fn observe_absence(engine: &mut ProviderEngine, index: usize, requested_tick: u32) {
        engine.apply_status(index, &empty_status(requested_tick));
        assert!(matches!(
            engine.slots[index].state,
            SlotState::Waiting {
                absence_observed: true,
                ..
            }
        ));
    }

    proptest! {
        #[test]
        fn non_increasing_request_ticks_do_not_advance_status_suspicion(
            first in any::<u32>(),
            candidate in any::<u32>(),
        ) {
            let latest = first.max(candidate);
            let non_increasing = first.min(candidate);
            let suspicion = StatusSuspicion::new(StatusEvidence::Absent, latest);

            prop_assert_eq!(
                suspicion.observe(
                    StatusEvidence::Foreign {
                        last_update_tick: candidate,
                    },
                    non_increasing,
                ),
                suspicion
            );
        }
    }

    #[test]
    fn schedules_three_streams_six_ticks_ahead_after_observed_absence() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        for index in 0..3 {
            observe_absence(&mut engine, index, 99);
            engine.start_if_ready(index, 100).expect("start chain");
        }
        let targets = engine
            .slots
            .each_ref()
            .map(|slot| slot.chain.as_ref().expect("chain").first_target);
        assert_eq!(targets, [108, 106, 107]);
    }

    #[test]
    fn a_fresh_absence_restarts_after_the_old_tail() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        observe_absence(&mut engine, 0, 99);
        engine.start_if_ready(0, 100).expect("start chain");
        engine.extend_chain(0, 105).expect("extend chain");
        assert_eq!(
            engine.slots[0].chain.as_ref().expect("chain").last_target,
            111
        );

        engine.apply_status(0, &empty_status(109));
        engine.apply_status(0, &empty_status(110));
        engine.apply_status(0, &empty_status(111));
        assert!(matches!(
            engine.slots[0].state,
            SlotState::Restarting {
                frozen_tail: 111,
                absence_observed: true,
                ..
            }
        ));
        engine.finish_status_restart(0, 111);
        engine.start_if_ready(0, 111).expect("restart chain");
        assert_eq!(
            engine.slots[0]
                .chain
                .as_ref()
                .expect("new chain")
                .first_target,
            117
        );
    }

    #[test]
    fn active_absence_requires_three_fresh_observations() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, frozen_tail) = prepare_active_tail(&mut engine, 0);

        engine.apply_status(0, &empty_status(first_target + 2));
        engine.apply_status(0, &empty_status(first_target + 2));
        assert_eq!(engine.slots[0].state, SlotState::Active);
        assert_eq!(
            engine.slots[0]
                .chain
                .as_ref()
                .expect("chain")
                .status_suspicion,
            Some(StatusSuspicion {
                evidence: StatusEvidence::Absent,
                confirmations: 1,
                first_requested_tick: first_target + 2,
                last_requested_tick: first_target + 2,
            })
        );

        engine.apply_status(0, &empty_status(first_target + 3));
        assert_eq!(engine.slots[0].state, SlotState::Active);
        engine.apply_status(0, &empty_status(first_target + 4));
        assert!(matches!(
            engine.slots[0].state,
            SlotState::Restarting {
                frozen_tail: tail,
                absence_observed: true,
                ..
            } if tail == frozen_tail
        ));
    }

    #[test]
    fn chain_keeps_planning_until_third_status_confirmation() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, old_tail) = prepare_active_tail(&mut engine, 0);

        engine.apply_status(0, &empty_status(first_target + 2));
        engine.apply_status(0, &empty_status(first_target + 3));
        engine
            .extend_chain(0, old_tail)
            .expect("suspected chain remains plannable");

        assert_eq!(engine.slots[0].state, SlotState::Active);
        assert!(engine.slots[0].chain.as_ref().expect("chain").last_target > old_tail);
    }

    #[test]
    fn starting_foreign_status_waits_for_target_and_three_confirmations() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        observe_absence(&mut engine, 0, 99);
        engine.start_if_ready(0, 100).expect("start chain");
        let first_target = engine.slots[0].chain.as_ref().expect("chain").first_target;
        let stale_target = first_target - STREAM_COUNT;

        let before_execution = owned_status(&engine, 0, first_target, stale_target);
        engine.apply_status(0, &before_execution);
        assert_eq!(
            engine.slots[0]
                .chain
                .as_ref()
                .expect("chain")
                .status_suspicion,
            None
        );

        for requested_tick in first_target + 1..first_target + 3 {
            let foreign = owned_status(&engine, 0, requested_tick, stale_target);
            engine.apply_status(0, &foreign);
            assert!(matches!(engine.slots[0].state, SlotState::Starting { .. }));
        }
        let third = owned_status(&engine, 0, first_target + 3, stale_target);
        engine.apply_status(0, &third);
        assert!(matches!(
            engine.slots[0].state,
            SlotState::Restarting {
                absence_observed: false,
                ..
            }
        ));
    }

    #[test]
    fn contradictory_evidence_restarts_the_confirmation_series() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, _) = prepare_active_tail(&mut engine, 0);
        let stale_target = first_target - STREAM_COUNT;

        engine.apply_status(0, &empty_status(first_target + 2));
        engine.apply_status(0, &owned_status(&engine, 0, first_target + 3, stale_target));
        engine.apply_status(0, &empty_status(first_target + 4));

        assert_eq!(engine.slots[0].state, SlotState::Active);
        assert_eq!(
            engine.slots[0]
                .chain
                .as_ref()
                .expect("chain")
                .status_suspicion,
            Some(StatusSuspicion {
                evidence: StatusEvidence::Absent,
                confirmations: 1,
                first_requested_tick: first_target + 4,
                last_requested_tick: first_target + 4,
            })
        );
    }

    #[test]
    fn advancing_owned_status_clears_absence_suspicion() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, _) = prepare_active_tail(&mut engine, 0);

        engine.apply_status(0, &empty_status(first_target + 2));
        engine.apply_status(0, &empty_status(first_target + 3));
        let advanced = owned_status(&engine, 0, first_target + 4, first_target + STREAM_COUNT);
        engine.apply_status(0, &advanced);

        let chain = engine.slots[0].chain.as_ref().expect("chain");
        assert_eq!(engine.slots[0].state, SlotState::Active);
        assert_eq!(chain.confirmed_through, Some(first_target + STREAM_COUNT));
        assert_eq!(chain.status_suspicion, None);
    }

    #[test]
    fn lagging_local_status_requires_three_fresh_observations() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, frozen_tail) = prepare_active_tail(&mut engine, 0);

        let first_lag = owned_status(&engine, 0, first_target + 4, first_target);
        engine.apply_status(0, &first_lag);
        assert_eq!(engine.slots[0].state, SlotState::Active);
        assert_eq!(
            engine.slots[0]
                .chain
                .as_ref()
                .expect("chain")
                .status_suspicion,
            Some(StatusSuspicion {
                evidence: StatusEvidence::Lag {
                    missing_target: first_target + STREAM_COUNT,
                },
                confirmations: 1,
                first_requested_tick: first_target + 4,
                last_requested_tick: first_target + 4,
            })
        );

        engine.apply_status(0, &first_lag);
        assert_eq!(engine.slots[0].state, SlotState::Active);

        let second_lag = owned_status(&engine, 0, first_target + 5, first_target);
        engine.apply_status(0, &second_lag);
        assert_eq!(engine.slots[0].state, SlotState::Active);

        let third_lag = owned_status(&engine, 0, first_target + 6, first_target);
        engine.apply_status(0, &third_lag);
        assert!(matches!(
            engine.slots[0].state,
            SlotState::Restarting {
                frozen_tail: tail,
                absence_observed: false,
                ..
            } if tail == frozen_tail
        ));
    }

    #[test]
    fn advancing_status_clears_lag_and_stale_regression_is_ignored() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, _) = prepare_active_tail(&mut engine, 0);

        let lag = owned_status(&engine, 0, first_target + 4, first_target);
        engine.apply_status(0, &lag);
        let advanced = owned_status(&engine, 0, first_target + 5, first_target + STREAM_COUNT);
        engine.apply_status(0, &advanced);
        let stale = owned_status(&engine, 0, first_target + 6, first_target);
        engine.apply_status(0, &stale);

        let chain = engine.slots[0].chain.as_ref().expect("chain");
        assert_eq!(engine.slots[0].state, SlotState::Active);
        assert_eq!(chain.confirmed_through, Some(first_target + STREAM_COUNT));
        assert_eq!(chain.status_suspicion, None);
    }

    #[tokio::test]
    async fn frozen_status_tail_is_sent_before_fresh_first_commit() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = engine(Arc::clone(&backend));
        let (first_target, frozen_tail) = prepare_active_tail(&mut engine, 1);
        let first_lag = owned_status(&engine, 1, first_target + 4, first_target);
        engine.apply_status(1, &first_lag);
        let second_lag = owned_status(&engine, 1, first_target + 5, first_target);
        engine.apply_status(1, &second_lag);
        let third_lag = owned_status(&engine, 1, first_target + 6, first_target);
        engine.apply_status(1, &third_lag);

        let signed_call_count = engine.slots[1].chain.as_ref().expect("chain").calls.len();
        engine
            .extend_chain(1, frozen_tail)
            .expect("restart state ignores extension");
        assert_eq!(
            engine.slots[1].chain.as_ref().expect("chain").calls.len(),
            signed_call_count
        );

        for call in &mut engine.slots[1].chain.as_mut().expect("chain").calls {
            if call.target_tick < frozen_tail {
                call.state = BroadcastState::Accepted;
            }
        }
        engine.dispatch_calls(1, frozen_tail - 1, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        assert_eq!(backend.broadcasts.lock().expect("broadcast lock").len(), 1);

        let absence_tick = frozen_tail + 1;
        engine.apply_status(1, &empty_status(absence_tick));
        engine.finish_status_restart(1, absence_tick);
        assert!(matches!(
            engine.slots[1].state,
            SlotState::Waiting {
                absence_observed: true,
                ..
            }
        ));
        engine
            .start_if_ready(1, absence_tick)
            .expect("fresh first commit");
        let replacement = engine.slots[1].chain.as_ref().expect("new chain");
        assert!(replacement.first_target >= frozen_tail + SEND_LEAD_TICKS);
        assert_eq!(replacement.first_target % STREAM_COUNT, 1);
        assert_eq!(replacement.calls[0].kind, CallKind::FirstCommit);
        assert!(
            replacement
                .calls
                .iter()
                .all(|call| call.kind != CallKind::TerminalReveal)
        );
    }

    #[test]
    fn expiry_during_status_restart_preserves_the_later_signed_tail() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, frozen_tail) = prepare_active_tail(&mut engine, 0);
        let first_lag = owned_status(&engine, 0, first_target + 4, first_target);
        engine.apply_status(0, &first_lag);
        let second_lag = owned_status(&engine, 0, first_target + 5, first_target);
        engine.apply_status(0, &second_lag);
        let third_lag = owned_status(&engine, 0, first_target + 6, first_target);
        engine.apply_status(0, &third_lag);

        engine.expire_calls(0, first_target + STREAM_COUNT);

        assert!(matches!(
            engine.slots[0].state,
            SlotState::Restarting {
                frozen_tail: tail,
                ..
            } if tail == frozen_tail
        ));
        let chain = engine.slots[0].chain.as_ref().expect("chain");
        assert!(chain.calls.iter().any(|call| {
            call.target_tick == frozen_tail && call.state == BroadcastState::Ready
        }));
        assert_eq!(engine.send_stats.failed, 1);
    }

    #[test]
    fn drain_during_status_restart_never_adds_terminal_reveal() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, frozen_tail) = prepare_active_tail(&mut engine, 2);
        let first_lag = owned_status(&engine, 2, first_target + 4, first_target);
        engine.apply_status(2, &first_lag);
        let second_lag = owned_status(&engine, 2, first_target + 5, first_target);
        engine.apply_status(2, &second_lag);
        let third_lag = owned_status(&engine, 2, first_target + 6, first_target);
        engine.apply_status(2, &third_lag);

        engine
            .enter_drain(2, DrainReason::Shutdown)
            .expect("record shutdown drain");
        assert!(
            engine.slots[2]
                .chain
                .as_ref()
                .expect("chain")
                .calls
                .iter()
                .all(|call| call.kind != CallKind::TerminalReveal)
        );
        engine.finish_status_restart(2, frozen_tail);
        assert_eq!(
            engine.slots[2].state,
            SlotState::Drained(DrainOutcome::NothingToDrain)
        );
    }

    #[test]
    fn missed_target_requires_status_requested_after_the_break() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        observe_absence(&mut engine, 0, 99);
        engine.start_if_ready(0, 100).expect("start chain");
        let target = engine.slots[0].chain.as_ref().expect("chain").first_target;

        engine.expire_calls(0, target);
        assert_eq!(engine.send_stats, SendStats::default());
        engine.apply_status(0, &empty_status(target));
        assert_eq!(
            engine.slots[0].state,
            SlotState::Waiting {
                absence_after_tick: target + 1,
                absence_observed: false,
            }
        );
        engine.apply_status(0, &empty_status(target + 1));
        assert!(matches!(
            engine.slots[0].state,
            SlotState::Waiting {
                absence_observed: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn retry_reuses_identical_signed_bytes() {
        let backend = Arc::new(MockBackend::default());
        backend.failures_left.store(1, Ordering::Relaxed);
        let mut engine = engine(Arc::clone(&backend));
        observe_absence(&mut engine, 1, 99);
        engine.start_if_ready(1, 100).expect("start chain");

        engine.dispatch_calls(1, 100, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        engine.dispatch_calls(1, 100, 1_000);
        harvest_until_idle(&mut engine, 1).await;

        let broadcasts = backend.broadcasts.lock().expect("broadcast lock");
        assert_eq!(broadcasts.len(), 2);
        assert_eq!(broadcasts[0], broadcasts[1]);
    }

    #[tokio::test]
    async fn retry_counts_only_the_final_normal_delivery() {
        let backend = Arc::new(MockBackend::default());
        backend.failures_left.store(1, Ordering::Relaxed);
        let mut engine = engine(Arc::clone(&backend));
        let target = prepare_normal_call(&mut engine, 1);

        engine.dispatch_calls(1, target - SEND_LEAD_TICKS, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        assert_eq!(engine.send_stats, SendStats::default());

        engine.dispatch_calls(1, target - SEND_LEAD_TICKS, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        assert_eq!(
            engine.send_stats,
            SendStats {
                ok: 1,
                failed: 0,
                empty: 0,
            }
        );
    }

    #[test]
    fn expired_normal_target_counts_one_final_failure() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let target = prepare_normal_call(&mut engine, 1);

        engine.expire_calls(1, target);

        assert_eq!(
            engine.send_stats,
            SendStats {
                ok: 0,
                failed: 1,
                empty: 0,
            }
        );
    }

    #[tokio::test]
    async fn empty_target_tick_reclassifies_financial_failure() {
        let backend = Arc::new(MockBackend::default());
        backend
            .tick_results
            .lock()
            .expect("tick results lock")
            .push_back(Ok(false));
        let mut engine = engine(Arc::clone(&backend));
        let target = prepare_normal_call(&mut engine, 1);

        engine.expire_calls(1, target);
        assert_eq!(engine.send_stats.failed, 1);
        engine.ensure_tick_check(target + 1);
        harvest_tick_check_until_idle(&mut engine).await;

        assert_eq!(
            engine.send_stats,
            SendStats {
                ok: 0,
                failed: 0,
                empty: 1,
            }
        );
    }

    #[tokio::test]
    async fn confirmed_absence_reclassifies_only_the_first_missing_reveal() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = engine(Arc::clone(&backend));
        let (first_target, _) = prepare_active_tail(&mut engine, 0);
        engine.dispatch_calls(0, first_target, 1_000);
        harvest_until_idle(&mut engine, 0).await;
        assert_eq!(engine.send_stats.ok, 2);

        engine.apply_status(0, &empty_status(first_target + 4));
        engine.apply_status(0, &empty_status(first_target + 5));
        engine.apply_status(0, &empty_status(first_target + 6));

        assert!(matches!(
            engine.slots[0].state,
            SlotState::Restarting { .. }
        ));
        assert_eq!(
            engine.send_stats,
            SendStats {
                ok: 1,
                failed: 1,
                empty: 0,
            }
        );
        engine.apply_status(0, &empty_status(first_target + 7));
        assert_eq!(engine.send_stats.failed, 1);
    }

    #[test]
    fn confirmed_lag_counts_the_first_missing_reveal() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, _) = prepare_active_tail(&mut engine, 0);

        for requested_tick in first_target + 4..=first_target + 6 {
            let lag = owned_status(&engine, 0, requested_tick, first_target);
            engine.apply_status(0, &lag);
        }

        assert!(matches!(
            engine.slots[0].state,
            SlotState::Restarting { .. }
        ));
        assert_eq!(engine.send_stats.failed, 1);
    }

    #[test]
    fn foreign_status_restart_does_not_count_a_financial_failure() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, _) = prepare_active_tail(&mut engine, 1);

        for requested_tick in first_target + 4..=first_target + 6 {
            let foreign = owned_status(&engine, 1, requested_tick, first_target + 1);
            engine.apply_status(1, &foreign);
        }

        assert!(matches!(
            engine.slots[1].state,
            SlotState::Restarting { .. }
        ));
        assert_eq!(engine.send_stats, SendStats::default());
    }

    #[tokio::test]
    async fn accepted_call_is_never_rebroadcast_at_or_after_target() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = engine(Arc::clone(&backend));
        observe_absence(&mut engine, 1, 99);
        engine.start_if_ready(1, 100).expect("start chain");
        let target = engine.slots[1].chain.as_ref().expect("chain").first_target;

        engine.dispatch_calls(1, target - SEND_LEAD_TICKS, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        engine.expire_calls(1, target);
        engine.dispatch_calls(1, target, 1_000);
        engine.dispatch_calls(1, target.saturating_add(1), 1_000);

        assert_eq!(backend.broadcasts.lock().expect("broadcast lock").len(), 1);
        assert_eq!(
            engine.slots[1].chain.as_ref().expect("chain").calls[0].state,
            BroadcastState::Accepted
        );
        assert_eq!(engine.send_stats, SendStats::default());
    }

    #[tokio::test]
    async fn empty_target_tick_reclassifies_normal_success() {
        let backend = Arc::new(MockBackend::default());
        backend
            .tick_results
            .lock()
            .expect("tick results lock")
            .push_back(Ok(false));
        let mut engine = engine(Arc::clone(&backend));
        let target = prepare_normal_call(&mut engine, 1);

        engine.dispatch_calls(1, target - SEND_LEAD_TICKS, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        engine.ensure_tick_check(target + 1);
        harvest_tick_check_until_idle(&mut engine).await;

        assert_eq!(
            engine.send_stats,
            SendStats {
                ok: 0,
                failed: 0,
                empty: 1,
            }
        );
        assert_eq!(backend.tick_checks.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn target_tick_check_waits_for_the_configured_delay() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        engine.pending_tick_checks.insert(100);

        engine.ensure_tick_check(100);
        assert!(engine.tick_check.is_none());
        assert!(engine.pending_tick_checks.contains(&100));

        engine.ensure_tick_check(101);
        assert_eq!(
            engine.tick_check.as_ref().map(|check| check.target_tick),
            Some(100)
        );
    }

    #[tokio::test]
    async fn tick_check_error_retries_without_changing_counts() {
        let backend = Arc::new(MockBackend::default());
        backend
            .tick_results
            .lock()
            .expect("tick results lock")
            .extend([
                Err(BackendError::new("temporary tick-data failure")),
                Ok(true),
            ]);
        let mut engine = engine(Arc::clone(&backend));
        let target = prepare_normal_call(&mut engine, 1);

        engine.dispatch_calls(1, target - SEND_LEAD_TICKS, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        engine.ensure_tick_check(target + 1);
        harvest_tick_check_until_idle(&mut engine).await;
        assert_eq!(engine.send_stats.ok, 1);
        assert!(engine.pending_tick_checks.contains(&target));

        engine.next_tick_check_at = Instant::now();
        engine.ensure_tick_check(target + 1);
        harvest_tick_check_until_idle(&mut engine).await;
        assert_eq!(
            engine.send_stats,
            SendStats {
                ok: 1,
                failed: 0,
                empty: 0,
            }
        );
        assert_eq!(backend.tick_checks.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn terminal_waits_for_normal_backend_acceptance() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        observe_absence(&mut engine, 1, 99);
        engine.start_if_ready(1, 100).expect("start chain");
        engine
            .enter_drain(1, DrainReason::Shutdown)
            .expect("enter drain");

        engine.dispatch_calls(1, 103, 1_000);
        let calls = &engine.slots[1].chain.as_ref().expect("chain").calls;
        assert_eq!(calls[0].state, BroadcastState::Broadcasting);
        assert_eq!(calls[1].state, BroadcastState::Ready);
    }

    #[tokio::test]
    async fn shutdown_terminal_has_one_broadcast_attempt() {
        let backend = Arc::new(MockBackend::default());
        backend.failures_left.store(1, Ordering::Relaxed);
        let mut engine = engine(Arc::clone(&backend));
        observe_absence(&mut engine, 1, 99);
        engine.start_if_ready(1, 100).expect("start chain");
        engine
            .enter_drain(1, DrainReason::Shutdown)
            .expect("enter drain");
        {
            let first = &mut engine.slots[1].chain.as_mut().expect("chain").calls[0];
            first.state = BroadcastState::Accepted;
        }

        engine.dispatch_calls(1, 103, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        engine.finish_drain(1);

        assert_eq!(
            engine.slots[1].state,
            SlotState::Drained(DrainOutcome::Failed)
        );
        assert_eq!(backend.broadcasts.lock().expect("broadcast lock").len(), 1);
    }

    #[tokio::test]
    async fn shutdown_terminal_failure_counts_one_financial_failure() {
        let backend = Arc::new(MockBackend::default());
        backend.failures_left.store(1, Ordering::Relaxed);
        let mut engine = engine(Arc::clone(&backend));
        observe_absence(&mut engine, 1, 99);
        engine.start_if_ready(1, 100).expect("start chain");
        engine
            .enter_drain(1, DrainReason::Shutdown)
            .expect("enter drain");
        engine.slots[1].chain.as_mut().expect("chain").calls[0].state = BroadcastState::Accepted;

        engine.dispatch_calls(1, 103, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        engine.finish_drain(1);

        assert_eq!(engine.send_stats.failed, 1);
        assert_eq!(
            engine.slots[1].state,
            SlotState::Drained(DrainOutcome::Failed)
        );
    }

    #[tokio::test]
    async fn expired_attempted_terminal_counts_one_financial_failure() {
        let backend = Arc::new(MockBackend::default());
        backend.failures_left.store(1, Ordering::Relaxed);
        let mut engine = engine(Arc::clone(&backend));
        observe_absence(&mut engine, 1, 99);
        engine.start_if_ready(1, 100).expect("start chain");
        engine
            .enter_drain(1, DrainReason::Epoch)
            .expect("enter drain");
        engine.slots[1].chain.as_mut().expect("chain").calls[0].state = BroadcastState::Accepted;
        let terminal_target = engine.slots[1]
            .chain
            .as_ref()
            .expect("chain")
            .calls
            .back()
            .expect("terminal")
            .target_tick;

        engine.dispatch_calls(1, terminal_target - SEND_LEAD_TICKS, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        engine.expire_calls(1, terminal_target);

        assert_eq!(engine.send_stats.failed, 1);
        assert_eq!(
            engine.slots[1].state,
            SlotState::Drained(DrainOutcome::Failed)
        );
    }

    #[test]
    fn unattempted_terminal_does_not_double_count_failed_prerequisite() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let normal_target = prepare_normal_call(&mut engine, 1);
        engine
            .enter_drain(1, DrainReason::Epoch)
            .expect("enter drain");
        let terminal_target = engine.slots[1]
            .chain
            .as_ref()
            .expect("chain")
            .calls
            .back()
            .expect("terminal")
            .target_tick;

        engine.expire_calls(1, terminal_target);

        assert_eq!(engine.send_stats.failed, 1);
        assert_eq!(
            engine.send_outcomes.get(&normal_target),
            Some(&SendOutcome::Failed)
        );
        assert!(!engine.send_outcomes.contains_key(&terminal_target));
    }

    #[tokio::test]
    async fn pre_epoch_terminal_retries_identical_bytes() {
        let backend = Arc::new(MockBackend::default());
        backend.failures_left.store(1, Ordering::Relaxed);
        let mut engine = engine(Arc::clone(&backend));
        observe_absence(&mut engine, 1, 99);
        engine.start_if_ready(1, 100).expect("start chain");
        engine
            .enter_drain(1, DrainReason::Epoch)
            .expect("enter drain");
        {
            let first = &mut engine.slots[1].chain.as_mut().expect("chain").calls[0];
            first.state = BroadcastState::Accepted;
        }

        engine.dispatch_calls(1, 103, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        engine.dispatch_calls(1, 103, 1_000);
        harvest_until_idle(&mut engine, 1).await;
        engine.finish_drain(1);

        let broadcasts = backend.broadcasts.lock().expect("broadcast lock");
        assert_eq!(broadcasts.len(), 2);
        assert_eq!(broadcasts[0], broadcasts[1]);
        assert_eq!(
            engine.slots[1].state,
            SlotState::Drained(DrainOutcome::Accepted)
        );
        assert_eq!(engine.send_stats, SendStats::default());
    }

    #[test]
    fn terminal_reveal_is_zero_commit_after_frozen_tail() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        observe_absence(&mut engine, 2, 99);
        engine.start_if_ready(2, 100).expect("start chain");
        let old_tail = engine.slots[2].chain.as_ref().expect("chain").last_target;
        engine
            .enter_drain(2, DrainReason::Epoch)
            .expect("enter drain");
        let terminal = engine.slots[2]
            .chain
            .as_ref()
            .expect("chain")
            .calls
            .back()
            .expect("terminal");
        assert_eq!(terminal.kind, CallKind::TerminalReveal);
        assert_eq!(terminal.target_tick, old_tail + 3);
        assert_eq!(&terminal.tx_bytes[80 + 512..80 + 544], &[0; 32]);
    }

    #[test]
    fn epoch_warmup_and_wall_clock_drain_are_preserved() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let monday = Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap();
        engine.update_epoch_phase(
            &TickInfo {
                epoch: 9,
                tick: 149,
                initial_tick: 100,
                tick_duration_ms: 1_000,
            },
            monday,
        );
        assert!(matches!(
            engine.epoch_phase,
            Some(EpochPhase::Warmup { .. })
        ));
        engine.update_epoch_phase(
            &TickInfo {
                epoch: 9,
                tick: 150,
                initial_tick: 100,
                tick_duration_ms: 1_000,
            },
            monday,
        );
        assert_eq!(engine.epoch_phase, Some(EpochPhase::Active { epoch: 9 }));

        let boundary = Utc.with_ymd_and_hms(2026, 8, 5, 12, 0, 0).unwrap();
        assert!(is_epoch_stop_window(
            boundary - TimeDelta::seconds(600),
            600
        ));
        engine.update_epoch_phase(
            &TickInfo {
                epoch: 9,
                tick: 151,
                initial_tick: 100,
                tick_duration_ms: 1_000,
            },
            boundary - TimeDelta::seconds(600),
        );
        assert_eq!(engine.epoch_phase, Some(EpochPhase::Draining { epoch: 9 }));
    }

    #[test]
    fn epoch_change_discards_status_restart_state() {
        let mut engine = engine(Arc::new(MockBackend::default()));
        let (first_target, _) = prepare_active_tail(&mut engine, 0);
        let first_lag = owned_status(&engine, 0, first_target + 4, first_target);
        engine.apply_status(0, &first_lag);
        let second_lag = owned_status(&engine, 0, first_target + 5, first_target);
        engine.apply_status(0, &second_lag);
        let third_lag = owned_status(&engine, 0, first_target + 6, first_target);
        engine.apply_status(0, &third_lag);
        assert!(matches!(
            engine.slots[0].state,
            SlotState::Restarting { .. }
        ));

        let monday = Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap();
        engine.update_epoch_phase(
            &TickInfo {
                epoch: 10,
                tick: 200,
                initial_tick: 200,
                tick_duration_ms: 1_000,
            },
            monday,
        );

        assert!(engine.slots[0].chain.is_none());
        assert_eq!(
            engine.slots[0].state,
            SlotState::Waiting {
                absence_after_tick: 0,
                absence_observed: false,
            }
        );
    }

    #[test]
    fn target_arithmetic_never_wraps() {
        assert_eq!(next_stream_tick(12, 0), Some(12));
        assert_eq!(next_stream_tick(12, 1), Some(13));
        assert_eq!(next_stream_tick(u32::MAX, 0), Some(u32::MAX));
        assert_eq!(next_stream_tick(u32::MAX, 1), None);

        let mut engine = engine(Arc::new(MockBackend::default()));
        observe_absence(&mut engine, 0, 99);
        engine.start_if_ready(0, 100).expect("start chain");
        engine.slots[0].chain.as_mut().expect("chain").last_target = u32::MAX;
        engine.lose_chain(0, u32::MAX, true, "test overflow");
        assert_eq!(
            engine.slots[0].state,
            SlotState::Drained(DrainOutcome::Failed)
        );
    }

    async fn harvest_until_idle(engine: &mut ProviderEngine, index: usize) {
        for _ in 0..100 {
            tokio::task::yield_now().await;
            engine.harvest_broadcasts(index).await;
            if engine.slots[index].chain.as_ref().is_none_or(|chain| {
                chain
                    .calls
                    .iter()
                    .all(|call| call.state != BroadcastState::Broadcasting)
            }) {
                return;
            }
        }
        panic!("broadcast task did not finish");
    }

    async fn harvest_tick_check_until_idle(engine: &mut ProviderEngine) {
        for _ in 0..100 {
            tokio::task::yield_now().await;
            engine.harvest_tick_check().await;
            if engine.tick_check.is_none() {
                return;
            }
        }
        panic!("tick check task did not finish");
    }
}
