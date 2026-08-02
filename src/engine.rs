use std::collections::VecDeque;
use std::fmt;
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
const PLAN_HORIZON_TICKS: u32 = 9;
const MAX_QUEUED_CALLS: usize = 4;
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
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BroadcastState {
    Planned,
    Broadcasting,
    Retry,
    Accepted,
    Expired,
}

struct PlannedCall {
    generation: u64,
    target_tick: u32,
    tx_bytes: Vec<u8>,
    revealed_preimage: Box<[u8; 512]>,
    next_preimage: Option<Box<[u8; 512]>>,
    kind: CallKind,
    broadcast_state: BroadcastState,
    ever_attempted: bool,
    broadcast: Option<NetworkTask<String>>,
}

impl fmt::Debug for PlannedCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlannedCall")
            .field("generation", &self.generation)
            .field("target_tick", &self.target_tick)
            .field("kind", &self.kind)
            .field("broadcast_state", &self.broadcast_state)
            .field("ever_attempted", &self.ever_attempted)
            .field("tx_bytes", &"REDACTED")
            .field("revealed_preimage", &"REDACTED")
            .field("next_preimage", &"REDACTED")
            .finish()
    }
}

struct Generation {
    id: u64,
    first_target: u32,
    last_normal_target: u32,
    confirmed_through: Option<u32>,
    outstanding_preimage: Box<[u8; 512]>,
}

impl fmt::Debug for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Generation")
            .field("id", &self.id)
            .field("first_target", &self.first_target)
            .field("last_normal_target", &self.last_normal_target)
            .field("confirmed_through", &self.confirmed_through)
            .field("outstanding_preimage", &"REDACTED")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Unknown,
    Unmanaged,
    Vacant,
    Predicting,
    Reconciling,
    Stopping { terminal_target: u32 },
    ShutdownComplete(DrainOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainOutcome {
    Accepted,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EpochPhase {
    Active,
    Draining { epoch: u32 },
    Warmup { epoch: u32 },
}

struct ManagedSlot {
    key: SlotKey,
    state: SlotState,
    generation: Option<Generation>,
    calls: VecDeque<PlannedCall>,
    next_generation_id: u64,
    enrollment_reserved: bool,
    reconcile_after_tick: Option<u32>,
}

impl fmt::Debug for ManagedSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManagedSlot")
            .field("key", &self.key)
            .field("state", &self.state)
            .field("generation", &self.generation)
            .field("calls", &self.calls)
            .field("next_generation_id", &self.next_generation_id)
            .field("enrollment_reserved", &self.enrollment_reserved)
            .field("reconcile_after_tick", &self.reconcile_after_tick)
            .finish()
    }
}

struct PendingStatusQuery {
    requested_tick: u32,
    task: NetworkTask<ProviderStatus>,
}

struct StatusObservation {
    requested_tick: u32,
    status: ProviderStatus,
}

struct BalanceObservation {
    balance: u64,
    observed_at: Instant,
}

pub struct ProviderEngine {
    backend: Arc<dyn NetworkBackend>,
    wallet: QubicWallet,
    identity: String,
    collateral: u64,
    slots: [ManagedSlot; 3],
    status_query: Option<PendingStatusQuery>,
    balance_query: Option<NetworkTask<BalanceObservation>>,
    balance_snapshot: Option<(u64, Instant)>,
    epoch_phase: EpochPhase,
    observed_epoch: Option<u32>,
    epoch_stop_lead_time_secs: u64,
    epoch_resume_delay_ticks: u32,
}

impl ProviderEngine {
    pub fn new(
        backend: Arc<dyn NetworkBackend>,
        wallet: QubicWallet,
        collateral: u64,
        epoch_stop_lead_time_secs: u64,
        epoch_resume_delay_ticks: u32,
    ) -> Self {
        let tier = collateral_tier(collateral);
        Self {
            backend,
            wallet,
            identity: wallet.get_identity(),
            collateral,
            slots: std::array::from_fn(|stream| ManagedSlot {
                key: SlotKey {
                    stream: stream as u8,
                    collateral_tier: tier,
                },
                state: SlotState::Unknown,
                generation: None,
                calls: VecDeque::new(),
                next_generation_id: 0,
                enrollment_reserved: false,
                reconcile_after_tick: None,
            }),
            status_query: None,
            balance_query: None,
            balance_snapshot: None,
            epoch_phase: EpochPhase::Active,
            observed_epoch: None,
            epoch_stop_lead_time_secs,
            epoch_resume_delay_ticks,
        }
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
                    return Err("one or more final reveal chains expired before submission".into());
                }
                console::log_info("All managed final reveal chains were submitted");
                return Ok(());
            }
            if shutdown_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err("shutdown timed out before final reveals could be submitted".into());
            }

            sleep(POLL_INTERVAL).await;
            let stop_requested = shutting_down || cancellation.is_cancelled();
            if let Err(err) = self.cycle(stop_requested, &cancellation).await {
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
                console::log_info("Shutdown requested; no new provider slots will be opened");
            }
        }
    }

    async fn cycle(
        &mut self,
        shutting_down: bool,
        cancellation: &CancellationToken,
    ) -> AppResult<()> {
        let tick = self
            .backend_call("get tick info", self.backend.tick_info())
            .await?;
        console::set_tick_value(tick.epoch, tick.tick);
        self.update_epoch_phase(&tick, Utc::now());
        let shutting_down = shutting_down || cancellation.is_cancelled();
        let draining_epoch = matches!(self.epoch_phase, EpochPhase::Draining { .. });

        let status = self.harvest_status_query().await;
        self.ensure_status_query(tick.tick);

        for index in 0..self.slots.len() {
            self.harvest_broadcasts(index).await;
            let stop_requested = shutting_down || cancellation.is_cancelled() || draining_epoch;
            if let Err(err) = self.advance_slot(index, &tick, status.as_ref(), stop_requested) {
                console::log_warn(format!(
                    "Stream {} predictive scheduling failed: {err}",
                    self.slots[index].key.stream
                ));
            }
        }

        if !shutting_down
            && !cancellation.is_cancelled()
            && matches!(self.epoch_phase, EpochPhase::Active)
        {
            self.enroll_vacant_slots(&tick, status.as_ref(), cancellation)
                .await?;
            for index in 0..self.slots.len() {
                if cancellation.is_cancelled() {
                    self.begin_shutdown(index, tick.tick)?;
                }
                self.dispatch_calls(index, tick.tick, tick.tick_duration_ms);
            }
        }
        Ok(())
    }

    fn update_epoch_phase(&mut self, tick: &TickInfo, now: chrono::DateTime<Utc>) {
        let epoch_changed = self.observed_epoch.is_some_and(|epoch| epoch != tick.epoch);
        if epoch_changed {
            self.reset_for_epoch();
            console::log_info(format!(
                "Epoch {} started; provider enrollment is paused for {} ticks",
                tick.epoch, self.epoch_resume_delay_ticks
            ));
        }

        if self.observed_epoch != Some(tick.epoch) {
            self.observed_epoch = Some(tick.epoch);
            self.epoch_phase = EpochPhase::Warmup { epoch: tick.epoch };
        }

        if matches!(self.epoch_phase, EpochPhase::Warmup { epoch } if epoch == tick.epoch)
            && tick.tick.saturating_sub(tick.initial_tick) >= self.epoch_resume_delay_ticks
        {
            self.epoch_phase = EpochPhase::Active;
        }

        if matches!(self.epoch_phase, EpochPhase::Active)
            && is_epoch_stop_window(now, self.epoch_stop_lead_time_secs)
        {
            self.epoch_phase = EpochPhase::Draining { epoch: tick.epoch };
            console::log_info(format!(
                "Epoch {} is approaching its boundary; draining managed slots",
                tick.epoch
            ));
        }
    }

    fn reset_for_epoch(&mut self) {
        if let Some(query) = self.status_query.take() {
            query.task.abort();
        }
        if let Some(task) = self.balance_query.take() {
            task.abort();
        }
        self.balance_snapshot = None;
        for slot in &mut self.slots {
            for call in &mut slot.calls {
                if let Some(task) = call.broadcast.take() {
                    task.abort();
                }
            }
            slot.calls.clear();
            slot.generation = None;
            slot.enrollment_reserved = false;
            slot.reconcile_after_tick = None;
            slot.state = SlotState::Unknown;
        }
    }

    async fn harvest_status_query(&mut self) -> Option<StatusObservation> {
        let is_finished = self
            .status_query
            .as_ref()
            .is_some_and(|query| query.task.is_finished());
        if !is_finished {
            return None;
        }

        let query = self.status_query.take()?;
        match join_network_task(query.task).await {
            Ok(status) => Some(StatusObservation {
                requested_tick: query.requested_tick,
                status,
            }),
            Err(err) => {
                console::log_warn(format!(
                    "Provider status is temporarily unavailable; prediction continues: {err}"
                ));
                None
            }
        }
    }

    fn ensure_status_query(&mut self, requested_tick: u32) {
        if self.status_query.is_some() {
            return;
        }
        let backend = Arc::clone(&self.backend);
        let public_key = self.wallet.public_key.0.to_vec();
        self.status_query = Some(PendingStatusQuery {
            requested_tick,
            task: spawn_network_task("query provider status", async move {
                let output = backend
                    .query_contract_function(ContractFunctionRequest {
                        contract_index: RANDOM_CONTRACT_INDEX,
                        input_type: GET_PROVIDER_STATUS_FUNCTION,
                        input: public_key,
                    })
                    .await
                    .map_err(|err| err.to_string())?;
                ProviderStatus::decode(&output).map_err(|err| err.to_string())
            }),
        });
    }

    fn ensure_balance_query(&mut self) {
        if self.balance_query.is_some() {
            return;
        }
        let backend = Arc::clone(&self.backend);
        let identity = self.identity.clone();
        self.balance_query = Some(spawn_network_task("get balance", async move {
            let balance = backend
                .balance(&identity)
                .await
                .map_err(|err| err.to_string())?;
            Ok(BalanceObservation {
                balance,
                observed_at: Instant::now(),
            })
        }));
    }

    async fn enroll_vacant_slots(
        &mut self,
        tick: &TickInfo,
        observation: Option<&StatusObservation>,
        cancellation: &CancellationToken,
    ) -> AppResult<()> {
        if let Some(result) = take_finished_task(&mut self.balance_query).await {
            match result {
                Ok(observation) => {
                    console::set_balance_line(format!(
                        "QUBIC: {}",
                        console::format_amount(observation.balance)
                    ));
                    self.balance_snapshot = Some((observation.balance, observation.observed_at));
                }
                Err(err) => {
                    self.balance_snapshot = None;
                    console::log_warn(format!(
                        "Balance is temporarily unavailable; new provider slots will not be opened: {err}"
                    ));
                }
            }
        }

        let Some(observation) = observation else {
            return Ok(());
        };
        let vacant_slots = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                (slot.state == SlotState::Vacant && observation.status.slot(slot.key).is_none())
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        if vacant_slots.is_empty() {
            self.balance_snapshot = None;
            if let Some(task) = self.balance_query.take() {
                task.abort();
            }
            return Ok(());
        }

        if self
            .balance_snapshot
            .is_some_and(|(_, observed_at)| observed_at.elapsed() > BACKEND_TIMEOUT)
        {
            self.balance_snapshot = None;
        }

        let Some((balance, _)) = self.balance_snapshot.take() else {
            self.ensure_balance_query();
            return Ok(());
        };
        let reserved = self
            .slots
            .iter()
            .filter(|slot| slot.enrollment_reserved)
            .fold(0_u64, |total, _| total.saturating_add(self.collateral));
        let mut available_balance = balance.saturating_sub(reserved);
        for index in vacant_slots {
            if cancellation.is_cancelled() {
                break;
            }
            if let Err(err) = self.open_vacant_slot(index, tick.tick, &mut available_balance) {
                console::log_warn(format!(
                    "Stream {} enrollment failed: {err}",
                    self.slots[index].key.stream
                ));
            }
        }
        Ok(())
    }

    fn advance_slot(
        &mut self,
        index: usize,
        tick: &TickInfo,
        observation: Option<&StatusObservation>,
        shutting_down: bool,
    ) -> AppResult<()> {
        self.reconcile_presence(index, observation);

        if matches!(
            self.slots[index].state,
            SlotState::Predicting | SlotState::Reconciling
        ) && let Some(observation) = observation
        {
            self.reconcile_prediction(index, tick.tick, observation)?;
        }
        if self.slots[index].state == SlotState::Predicting {
            self.extend_normal_chain(index, tick.tick)?;
        }

        if shutting_down {
            self.begin_shutdown(index, tick.tick)?;
        }
        if self.slots[index].state != SlotState::Reconciling {
            self.dispatch_calls(index, tick.tick, tick.tick_duration_ms);
        }
        self.update_shutdown_slot(index);
        self.prune_completed_calls(index, tick.tick);
        Ok(())
    }

    fn reconcile_presence(&mut self, index: usize, observation: Option<&StatusObservation>) {
        let Some(observation) = observation else {
            return;
        };
        let key = self.slots[index].key;
        let occupied = observation.status.slot(key).is_some();
        match self.slots[index].state {
            SlotState::Unknown if occupied => {
                console::log_warn(format!(
                    "Stream {} tier {} is already occupied; its preimage is unavailable after restart",
                    key.stream, key.collateral_tier
                ));
                self.slots[index].state = SlotState::Unmanaged;
            }
            SlotState::Unknown => self.slots[index].state = SlotState::Vacant,
            SlotState::Unmanaged if !occupied => {
                console::log_info(format!(
                    "Stream {} tier {} became vacant",
                    key.stream, key.collateral_tier
                ));
                self.slots[index].state = SlotState::Vacant;
            }
            SlotState::Vacant if occupied => {
                self.slots[index].state = SlotState::Unmanaged;
            }
            SlotState::Unmanaged
            | SlotState::Vacant
            | SlotState::Predicting
            | SlotState::Reconciling
            | SlotState::Stopping { .. }
            | SlotState::ShutdownComplete(_) => {}
        }
    }

    fn reconcile_prediction(
        &mut self,
        index: usize,
        current_tick: u32,
        observation: &StatusObservation,
    ) -> AppResult<()> {
        let key = self.slots[index].key;
        if self.slots[index].state == SlotState::Reconciling
            && self.slots[index]
                .reconcile_after_tick
                .is_some_and(|tick| observation.requested_tick < tick)
        {
            return Ok(());
        }
        let observed_target = observation
            .status
            .slot(key)
            .map(|slot| slot.last_update_tick);

        if let Some(target_tick) = observed_target
            && self.slots[index]
                .generation
                .as_ref()
                .and_then(|generation| generation.confirmed_through)
                .is_some_and(|confirmed| target_tick < confirmed)
        {
            console::log_warn(format!(
                "Stream {} returned a non-monotonic status tick {target_tick}; waiting for a newer observation",
                key.stream
            ));
            return Ok(());
        }

        if let Some(target_tick) = observed_target {
            let matches_current = self.is_generation_normal_target(index, target_tick);
            if matches_current {
                self.confirm_through(index, target_tick);
            } else {
                let current_first = self.slots[index]
                    .generation
                    .as_ref()
                    .map(|generation| generation.first_target);
                if current_first.is_some_and(|first| target_tick >= first)
                    && !self.is_locally_signed_target(index, target_tick)
                {
                    console::log_warn(format!(
                        "Stream {} was updated at unknown tick {target_tick}; its current preimage is unmanaged",
                        key.stream
                    ));
                    self.mark_unmanaged(index);
                    return Ok(());
                }
            }
        }

        let deadline_reached = self.next_unconfirmed_target(index).is_some_and(|target| {
            observation.requested_tick >= target.saturating_add(STREAM_COUNT)
        });
        if deadline_reached || self.slots[index].state == SlotState::Reconciling {
            let missing_target = self.next_unconfirmed_target(index).unwrap_or_default();
            if let Some(target_tick) = observed_target
                && self.is_generation_normal_target(index, target_tick)
            {
                console::log_warn(format!(
                    "Stream {} stopped advancing before predicted tick {missing_target}; resuming from confirmed tick {target_tick}",
                    key.stream
                ));
                self.resume_from_confirmed(index, target_tick, current_tick)?;
                return Ok(());
            }
            if self.slots[index]
                .generation
                .as_ref()
                .is_some_and(|generation| missing_target > generation.first_target)
            {
                console::record_reveal_result(false);
            }
            if observed_target.is_none() {
                console::log_warn(format!(
                    "Stream {} is absent after the chain break; a fresh zero-reveal commit may be enrolled",
                    key.stream
                ));
                self.mark_vacant(index);
            } else {
                console::log_warn(format!(
                    "Stream {} remains occupied after the chain break, but its outstanding preimage is unknown; waiting for contract eviction",
                    key.stream
                ));
                self.mark_unmanaged(index);
            }
        }
        Ok(())
    }

    fn resume_from_confirmed(
        &mut self,
        index: usize,
        target_tick: u32,
        current_tick: u32,
    ) -> AppResult<()> {
        let next_preimage = self.slots[index]
            .calls
            .iter()
            .find(|call| {
                call.target_tick == target_tick
                    && matches!(call.kind, CallKind::FirstCommit | CallKind::RevealAndCommit)
            })
            .and_then(|call| call.next_preimage.as_deref())
            .copied()
            .ok_or("confirmed call preimage is unavailable")?;

        for call in self.slots[index]
            .calls
            .iter_mut()
            .filter(|call| call.target_tick > target_tick)
        {
            if let Some(task) = call.broadcast.take() {
                task.abort();
            }
            call.broadcast_state = BroadcastState::Expired;
        }
        self.slots[index]
            .calls
            .retain(|call| call.target_tick <= target_tick);
        let key = self.slots[index].key;
        let resume_target =
            next_stream_tick(current_tick.saturating_add(SEND_LEAD_TICKS), key.stream);
        let following_preimage = random_reveal();
        let input = RevealAndCommitInput {
            reveal: next_preimage,
            commit: commit_digest(&following_preimage),
        };
        let tx_bytes = self.build_transaction(input, resume_target, key)?;
        let generation = self.slots[index]
            .generation
            .as_mut()
            .ok_or("predictive generation disappeared")?;
        let generation_id = generation.id;
        generation.last_normal_target = resume_target;
        generation.confirmed_through = Some(target_tick);
        *generation.outstanding_preimage = following_preimage;
        self.slots[index].calls.push_back(PlannedCall {
            generation: generation_id,
            target_tick: resume_target,
            tx_bytes,
            revealed_preimage: Box::new(next_preimage),
            next_preimage: Some(Box::new(following_preimage)),
            kind: CallKind::RevealAndCommit,
            broadcast_state: BroadcastState::Planned,
            ever_attempted: false,
            broadcast: None,
        });
        self.slots[index].enrollment_reserved = false;
        self.slots[index].reconcile_after_tick = None;
        self.slots[index].state = SlotState::Predicting;
        self.extend_normal_chain(index, current_tick)
    }

    fn is_generation_normal_target(&self, index: usize, target_tick: u32) -> bool {
        let Some(generation_id) = self.slots[index]
            .generation
            .as_ref()
            .map(|generation| generation.id)
        else {
            return false;
        };
        self.slots[index].calls.iter().any(|call| {
            call.generation == generation_id
                && call.target_tick == target_tick
                && matches!(call.kind, CallKind::FirstCommit | CallKind::RevealAndCommit)
        })
    }

    fn next_unconfirmed_target(&self, index: usize) -> Option<u32> {
        let generation = self.slots[index].generation.as_ref()?;
        let confirmed = generation.confirmed_through;
        self.slots[index]
            .calls
            .iter()
            .filter(|call| {
                call.generation == generation.id
                    && matches!(call.kind, CallKind::FirstCommit | CallKind::RevealAndCommit)
                    && confirmed.is_none_or(|tick| call.target_tick > tick)
            })
            .map(|call| call.target_tick)
            .min()
    }

    fn confirm_through(&mut self, index: usize, target_tick: u32) {
        let Some((generation_id, previous)) = self.slots[index]
            .generation
            .as_ref()
            .map(|generation| (generation.id, generation.confirmed_through))
        else {
            return;
        };
        if previous.is_some_and(|tick| tick >= target_tick) {
            return;
        }

        let confirmed_reveals = self.slots[index]
            .calls
            .iter()
            .filter(|call| {
                call.generation == generation_id
                    && call.kind == CallKind::RevealAndCommit
                    && call.target_tick <= target_tick
                    && previous.is_none_or(|tick| call.target_tick > tick)
            })
            .count();
        for _ in 0..confirmed_reveals {
            console::record_reveal_result(true);
        }
        for call in self.slots[index].calls.iter_mut().filter(|call| {
            call.generation == generation_id
                && call.target_tick <= target_tick
                && matches!(call.kind, CallKind::FirstCommit | CallKind::RevealAndCommit)
        }) {
            if let Some(task) = call.broadcast.take() {
                task.abort();
            }
            call.broadcast_state = BroadcastState::Accepted;
        }
        if let Some(generation) = self.slots[index].generation.as_mut() {
            generation.confirmed_through = Some(target_tick);
        }
        self.slots[index].enrollment_reserved = false;
    }

    fn is_locally_signed_target(&self, index: usize, target_tick: u32) -> bool {
        let slot = &self.slots[index];
        slot.calls
            .iter()
            .any(|call| call.target_tick == target_tick)
    }

    fn start_generation(&mut self, index: usize, target_tick: u32) -> AppResult<()> {
        let key = self.slots[index].key;
        let generation_id = self.slots[index].next_generation_id;
        let next_generation_id = generation_id
            .checked_add(1)
            .ok_or("generation counter exhausted")?;
        let preimage = random_reveal();
        let input = RevealAndCommitInput {
            reveal: [0; 512],
            commit: commit_digest(&preimage),
        };
        let tx_bytes = self.build_transaction(input, target_tick, key)?;

        self.slots[index].next_generation_id = next_generation_id;
        self.slots[index].generation = Some(Generation {
            id: generation_id,
            first_target: target_tick,
            last_normal_target: target_tick,
            confirmed_through: None,
            outstanding_preimage: Box::new(preimage),
        });
        self.slots[index].calls.push_back(PlannedCall {
            generation: generation_id,
            target_tick,
            tx_bytes,
            revealed_preimage: Box::new([0; 512]),
            next_preimage: Some(Box::new(preimage)),
            kind: CallKind::FirstCommit,
            broadcast_state: BroadcastState::Planned,
            ever_attempted: false,
            broadcast: None,
        });
        self.slots[index].state = SlotState::Predicting;
        Ok(())
    }

    fn extend_normal_chain(&mut self, index: usize, current_tick: u32) -> AppResult<()> {
        let horizon = current_tick.saturating_add(PLAN_HORIZON_TICKS);
        loop {
            let queued = self.slots[index]
                .calls
                .iter()
                .filter(|call| {
                    call.target_tick > current_tick
                        && matches!(
                            call.broadcast_state,
                            BroadcastState::Planned
                                | BroadcastState::Broadcasting
                                | BroadcastState::Retry
                        )
                })
                .count();
            if queued >= MAX_QUEUED_CALLS {
                return Ok(());
            }

            let next_target = self.slots[index]
                .generation
                .as_ref()
                .and_then(|generation| generation.last_normal_target.checked_add(STREAM_COUNT))
                .ok_or("predictive target tick overflowed")?;
            if next_target <= current_tick {
                self.enter_reconciling(index, current_tick);
                return Ok(());
            }

            let Some((generation_id, target_tick, revealed_preimage, key)) = self.slots[index]
                .generation
                .as_ref()
                .and_then(|generation| {
                    let target_tick = generation.last_normal_target.checked_add(STREAM_COUNT)?;
                    (target_tick <= horizon).then(|| {
                        (
                            generation.id,
                            target_tick,
                            *generation.outstanding_preimage,
                            self.slots[index].key,
                        )
                    })
                })
            else {
                return Ok(());
            };

            let next_preimage = random_reveal();
            let input = RevealAndCommitInput {
                reveal: revealed_preimage,
                commit: commit_digest(&next_preimage),
            };
            let tx_bytes = self.build_transaction(input, target_tick, key)?;
            self.slots[index].calls.push_back(PlannedCall {
                generation: generation_id,
                target_tick,
                tx_bytes,
                revealed_preimage: Box::new(revealed_preimage),
                next_preimage: Some(Box::new(next_preimage)),
                kind: CallKind::RevealAndCommit,
                broadcast_state: BroadcastState::Planned,
                ever_attempted: false,
                broadcast: None,
            });
            let generation = self.slots[index]
                .generation
                .as_mut()
                .ok_or("predictive generation disappeared")?;
            generation.last_normal_target = target_tick;
            *generation.outstanding_preimage = next_preimage;
        }
    }

    fn enter_reconciling(&mut self, index: usize, current_tick: u32) {
        if self.slots[index].state == SlotState::Reconciling {
            return;
        }
        for call in &mut self.slots[index].calls {
            if call.broadcast_state != BroadcastState::Accepted {
                if let Some(task) = call.broadcast.take() {
                    task.abort();
                }
                call.broadcast_state = BroadcastState::Expired;
            }
        }
        self.slots[index].state = SlotState::Reconciling;
        self.slots[index].reconcile_after_tick = Some(current_tick);
        console::log_warn(format!(
            "Stream {} chain timing became discontinuous; broadcasts are paused until contract status is reconciled",
            self.slots[index].key.stream
        ));
    }

    fn begin_shutdown(&mut self, index: usize, current_tick: u32) -> AppResult<()> {
        match self.slots[index].state {
            SlotState::Unknown | SlotState::Vacant | SlotState::Unmanaged => return Ok(()),
            SlotState::Stopping { .. } | SlotState::ShutdownComplete(_) => return Ok(()),
            SlotState::Predicting => {}
            SlotState::Reconciling => {
                console::log_warn(format!(
                    "Stream {} cannot be drained because its outstanding preimage is uncertain",
                    self.slots[index].key.stream
                ));
                self.mark_unmanaged(index);
                return Ok(());
            }
        }

        self.extend_normal_chain(index, current_tick)?;
        if self.slots[index].state == SlotState::Reconciling {
            self.mark_unmanaged(index);
            return Ok(());
        }
        let generation = self.slots[index]
            .generation
            .as_ref()
            .ok_or("managed slot has no predictive generation")?;
        let terminal_target = generation
            .last_normal_target
            .checked_add(STREAM_COUNT)
            .ok_or("shutdown leave tick overflowed")?;
        let generation_id = generation.id;
        let reveal = *generation.outstanding_preimage;
        let input = RevealAndCommitInput {
            reveal,
            commit: [0; 32],
        };
        let tx_bytes = self.build_transaction(input, terminal_target, self.slots[index].key)?;
        self.slots[index].calls.push_back(PlannedCall {
            generation: generation_id,
            target_tick: terminal_target,
            tx_bytes,
            revealed_preimage: Box::new(reveal),
            next_preimage: None,
            kind: CallKind::Leave,
            broadcast_state: BroadcastState::Planned,
            ever_attempted: false,
            broadcast: None,
        });
        self.slots[index].state = SlotState::Stopping { terminal_target };
        console::log_info(format!(
            "Shutdown leave for stream {} is scheduled at tick {terminal_target}",
            self.slots[index].key.stream
        ));
        Ok(())
    }

    fn mark_unmanaged(&mut self, index: usize) {
        for call in &mut self.slots[index].calls {
            if let Some(task) = call.broadcast.take() {
                task.abort();
            }
            call.broadcast_state = BroadcastState::Expired;
        }
        self.slots[index].generation = None;
        self.slots[index].enrollment_reserved = false;
        self.slots[index].reconcile_after_tick = None;
        self.slots[index].state = SlotState::Unmanaged;
    }

    fn mark_vacant(&mut self, index: usize) {
        for call in &mut self.slots[index].calls {
            if let Some(task) = call.broadcast.take() {
                task.abort();
            }
        }
        self.slots[index].calls.clear();
        self.slots[index].generation = None;
        self.slots[index].enrollment_reserved = false;
        self.slots[index].reconcile_after_tick = None;
        self.slots[index].state = SlotState::Vacant;
    }

    fn open_vacant_slot(
        &mut self,
        index: usize,
        current_tick: u32,
        available_balance: &mut u64,
    ) -> AppResult<()> {
        if self.slots[index].state != SlotState::Vacant {
            return Ok(());
        }

        let key = self.slots[index].key;
        let safety_floor = self.collateral.saturating_mul(2);
        if *available_balance < safety_floor {
            console::log_warn(format!(
                "Waiting to open stream {}: {} QU available, {} required including reveal reserve",
                key.stream, *available_balance, safety_floor
            ));
            return Ok(());
        }

        let target_tick =
            next_stream_tick(current_tick.saturating_add(SEND_LEAD_TICKS), key.stream);
        self.start_generation(index, target_tick)?;
        self.slots[index].enrollment_reserved = true;
        *available_balance = available_balance.saturating_sub(self.collateral);
        Ok(())
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

    fn dispatch_calls(&mut self, index: usize, current_tick: u32, tick_duration_ms: u32) {
        let horizon = current_tick.saturating_add(SEND_LEAD_TICKS);

        for call in &mut self.slots[index].calls {
            if call.broadcast_state == BroadcastState::Accepted {
                continue;
            }
            if current_tick >= call.target_tick {
                if let Some(task) = call.broadcast.take() {
                    task.abort();
                }
                call.broadcast_state = BroadcastState::Expired;
                continue;
            }

            let should_start = match call.broadcast_state {
                BroadcastState::Planned => call.target_tick <= horizon,
                BroadcastState::Retry => true,
                BroadcastState::Broadcasting
                | BroadcastState::Accepted
                | BroadcastState::Expired => false,
            };
            if !should_start {
                continue;
            }

            let backend = Arc::clone(&self.backend);
            debug_assert_eq!(
                &call.tx_bytes[80..80 + 512],
                call.revealed_preimage.as_slice()
            );
            if let Some(next_preimage) = &call.next_preimage {
                debug_assert_eq!(
                    &call.tx_bytes[80 + 512..80 + 544],
                    commit_digest(next_preimage.as_ref()).as_slice()
                );
            } else {
                debug_assert_eq!(&call.tx_bytes[80 + 512..80 + 544], &[0; 32]);
            }
            let tx_bytes = call.tx_bytes.clone();
            let lead_ticks = call.target_tick - current_tick;
            let attempt_timeout = broadcast_attempt_timeout(lead_ticks, tick_duration_ms);
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
            call.broadcast_state = BroadcastState::Broadcasting;
            call.ever_attempted = true;
            console::log_info(format!(
                "Broadcast for stream {} tick {} started",
                self.slots[index].key.stream, call.target_tick
            ));
        }
    }

    async fn harvest_broadcasts(&mut self, index: usize) {
        let key = self.slots[index].key;
        for call in &mut self.slots[index].calls {
            let is_finished = call.broadcast.as_ref().is_some_and(JoinHandle::is_finished);
            if !is_finished {
                continue;
            }
            let Some(task) = call.broadcast.take() else {
                continue;
            };
            match join_network_task(task).await {
                Ok(tx_id) => {
                    call.broadcast_state = BroadcastState::Accepted;
                    console::log_info(format!(
                        "Backend accepted transaction {} for stream {} tick {}",
                        console::shorten_id(&tx_id),
                        key.stream,
                        call.target_tick
                    ));
                }
                Err(err) => {
                    call.broadcast_state = BroadcastState::Retry;
                    console::log_warn(format!(
                        "Transaction for stream {} tick {} is pending identical-byte retry: {err}",
                        key.stream, call.target_tick
                    ));
                }
            }
        }
        self.update_shutdown_slot(index);
    }

    fn update_shutdown_slot(&mut self, index: usize) {
        let SlotState::Stopping { terminal_target } = self.slots[index].state else {
            return;
        };
        let Some(generation_id) = self.slots[index]
            .generation
            .as_ref()
            .map(|generation| generation.id)
        else {
            return;
        };
        let prerequisites = self.slots[index]
            .calls
            .iter()
            .filter(|call| call.generation == generation_id && call.target_tick <= terminal_target)
            .collect::<Vec<_>>();
        let outcome = if prerequisites
            .iter()
            .any(|call| call.broadcast_state == BroadcastState::Expired)
        {
            Some(DrainOutcome::Expired)
        } else if !prerequisites.is_empty()
            && prerequisites
                .iter()
                .all(|call| call.broadcast_state == BroadcastState::Accepted)
        {
            Some(DrainOutcome::Accepted)
        } else {
            None
        };
        if let Some(outcome) = outcome {
            self.complete_shutdown_slot(index, outcome);
        }
    }

    fn complete_shutdown_slot(&mut self, index: usize, outcome: DrainOutcome) {
        for call in &mut self.slots[index].calls {
            if let Some(task) = call.broadcast.take() {
                task.abort();
            }
        }
        self.slots[index].calls.clear();
        self.slots[index].generation = None;
        self.slots[index].enrollment_reserved = false;
        self.slots[index].reconcile_after_tick = None;
        self.slots[index].state = SlotState::ShutdownComplete(outcome);
    }

    fn prune_completed_calls(&mut self, index: usize, current_tick: u32) {
        let terminal = match self.slots[index].state {
            SlotState::Stopping { terminal_target } => Some(terminal_target),
            SlotState::Unknown
            | SlotState::Unmanaged
            | SlotState::Vacant
            | SlotState::Predicting
            | SlotState::Reconciling
            | SlotState::ShutdownComplete(_) => None,
        };
        if terminal.is_some() || self.slots[index].state == SlotState::Reconciling {
            return;
        }
        let confirmed = self.slots[index]
            .generation
            .as_ref()
            .and_then(|generation| generation.confirmed_through);
        let generation_id = self.slots[index]
            .generation
            .as_ref()
            .map(|generation| generation.id);
        let recent_unconfirmed_cutoff = self.slots[index]
            .calls
            .iter()
            .rev()
            .filter(|call| {
                generation_id == Some(call.generation)
                    && call.target_tick < current_tick
                    && matches!(call.kind, CallKind::FirstCommit | CallKind::RevealAndCommit)
                    && confirmed.is_none_or(|tick| call.target_tick > tick)
            })
            .nth(MAX_QUEUED_CALLS.saturating_sub(1))
            .map(|call| call.target_tick);
        self.slots[index].calls.retain(|call| {
            call.target_tick >= current_tick
                || confirmed == Some(call.target_tick)
                || (generation_id == Some(call.generation)
                    && matches!(call.kind, CallKind::FirstCommit | CallKind::RevealAndCommit)
                    && confirmed.is_none_or(|tick| call.target_tick > tick))
                    && recent_unconfirmed_cutoff.is_none_or(|cutoff| call.target_tick >= cutoff)
                || call.broadcast.is_some()
        });
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
        self.slots.iter().all(|slot| {
            matches!(
                slot.state,
                SlotState::Unknown
                    | SlotState::Vacant
                    | SlotState::Unmanaged
                    | SlotState::ShutdownComplete(_)
            )
        })
    }

    fn shutdown_failed(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.state == SlotState::ShutdownComplete(DrainOutcome::Expired))
    }
}

impl Drop for ProviderEngine {
    fn drop(&mut self) {
        if let Some(query) = self.status_query.take() {
            query.task.abort();
        }
        if let Some(task) = self.balance_query.take() {
            task.abort();
        }
        for slot in &mut self.slots {
            for call in &mut slot.calls {
                if let Some(task) = call.broadcast.take() {
                    task.abort();
                }
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

async fn take_finished_task<T>(task: &mut Option<NetworkTask<T>>) -> Option<Result<T, String>> {
    if !task.as_ref().is_some_and(JoinHandle::is_finished) {
        return None;
    }
    Some(join_network_task(task.take()?).await)
}

fn collateral_tier(collateral: u64) -> u8 {
    collateral.ilog10() as u8
}

fn next_stream_tick(at_or_after: u32, stream: u8) -> u32 {
    let stream = u32::from(stream);
    at_or_after.saturating_add((stream + STREAM_COUNT - at_or_after % STREAM_COUNT) % STREAM_COUNT)
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

fn random_reveal() -> [u8; 512] {
    let mut reveal = [0; 512];
    fill_secure_bits(&mut reveal);
    reveal
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::*;
    use crate::contract::{PROVIDER_STATUS_SIZE, ProviderSlot};

    #[test]
    fn schedules_each_stream_at_or_after_requested_tick() {
        assert_eq!(next_stream_tick(12, 0), 12);
        assert_eq!(next_stream_tick(12, 1), 13);
        assert_eq!(next_stream_tick(12, 2), 14);
        assert_eq!(next_stream_tick(u32::MAX, 0), u32::MAX);
    }

    #[test]
    fn collateral_maps_to_contract_tier() {
        assert_eq!(collateral_tier(1), 0);
        assert_eq!(collateral_tier(1_000_000_000), 9);
    }

    #[tokio::test]
    async fn reveal_is_broadcast_before_commit_executes_with_six_tick_lead() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(Arc::clone(&backend));
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();

        engine.advance_slot(0, &tick(3), None, false).unwrap();
        wait_for_broadcasts(&backend, 1).await;
        engine.harvest_broadcasts(0).await;
        engine.advance_slot(0, &tick(6), None, false).unwrap();
        wait_for_broadcasts(&backend, 2).await;

        let broadcasts = backend.broadcasts.lock().unwrap();
        let reveal_broadcast_tick = 6;
        assert_eq!(transaction_tick(&broadcasts[0]), 9);
        assert_eq!(transaction_tick(&broadcasts[1]), 12);
        assert!(reveal_broadcast_tick < transaction_tick(&broadcasts[0]));
        assert_eq!(
            transaction_tick(&broadcasts[1]) - reveal_broadcast_tick,
            SEND_LEAD_TICKS
        );
        assert_ne!(&broadcasts[1][80..80 + 512], &[0; 512]);
        assert_ne!(&broadcasts[1][80 + 512..80 + 544], &[0; 32]);
    }

    #[tokio::test]
    async fn status_latency_does_not_stop_predictive_planning() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();

        for current_tick in [3, 6, 9, 12, 15] {
            engine
                .advance_slot(0, &tick(current_tick), None, false)
                .unwrap();
        }

        let generation = engine.slots[0].generation.as_ref().unwrap();
        assert_eq!(generation.last_normal_target, 24);
        assert_eq!(engine.slots[0].state, SlotState::Predicting);
    }

    #[tokio::test]
    async fn status_transport_error_does_not_start_reconciliation() {
        let backend = Arc::new(StatusErrorBackend::default());
        let network_backend: Arc<dyn NetworkBackend> = backend;
        let mut engine = ProviderEngine::new(
            network_backend,
            QubicWallet::from_seed(&"a".repeat(55)).unwrap(),
            1,
            600,
            50,
        );
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.advance_slot(0, &tick(3), None, false).unwrap();

        let cancellation = CancellationToken::new();
        engine.cycle(false, &cancellation).await.unwrap();
        tokio::task::yield_now().await;
        engine.cycle(false, &cancellation).await.unwrap();

        assert_eq!(engine.slots[0].state, SlotState::Predicting);
    }

    #[tokio::test]
    async fn confirmed_absence_after_deadline_allows_fresh_enrollment() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        let empty = ProviderStatus { slots: Vec::new() };

        engine
            .advance_slot(0, &tick(11), Some(&observation(11, empty.clone())), false)
            .unwrap();
        assert_eq!(engine.slots[0].state, SlotState::Predicting);

        engine
            .advance_slot(0, &tick(12), Some(&observation(12, empty)), false)
            .unwrap();
        assert_eq!(engine.slots[0].state, SlotState::Vacant);
        assert!(
            !engine.slots[0]
                .calls
                .iter()
                .any(|call| call.kind == CallKind::Leave)
        );

        let mut available_balance = 2;
        engine
            .open_vacant_slot(0, 12, &mut available_balance)
            .unwrap();
        let first_commit = engine.slots[0].calls.back().unwrap();
        assert_eq!(first_commit.kind, CallKind::FirstCommit);
        assert_eq!(&first_commit.tx_bytes[80..80 + 512], &[0; 512]);
        assert_ne!(&first_commit.tx_bytes[80 + 512..80 + 544], &[0; 32]);
    }

    #[tokio::test]
    async fn lagging_local_status_uses_deadline_of_next_unconfirmed_target() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.advance_slot(0, &tick(3), None, false).unwrap();
        let stale = status_for(engine.slots[0].key, 9);

        engine
            .advance_slot(0, &tick(14), Some(&observation(14, stale.clone())), false)
            .unwrap();
        assert_eq!(engine.slots[0].state, SlotState::Predicting);
        engine
            .advance_slot(0, &tick(15), Some(&observation(15, stale)), false)
            .unwrap();

        assert_eq!(engine.slots[0].state, SlotState::Predicting);
        let resumed = engine.slots[0]
            .calls
            .iter()
            .find(|call| call.target_tick == 21)
            .unwrap();
        assert_eq!(resumed.kind, CallKind::RevealAndCommit);
        assert_ne!(&resumed.tx_bytes[80..80 + 512], &[0; 512]);
        assert_ne!(&resumed.tx_bytes[80 + 512..80 + 544], &[0; 32]);
        assert!(
            !engine.slots[0]
                .calls
                .iter()
                .any(|call| call.kind == CallKind::Leave)
        );
    }

    #[test]
    fn timing_gap_pauses_broadcasts_until_status_reconciliation() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.extend_normal_chain(0, 30).unwrap();

        assert_eq!(engine.slots[0].state, SlotState::Reconciling);
        assert!(
            engine.slots[0]
                .calls
                .iter()
                .all(|call| call.broadcast_state != BroadcastState::Broadcasting)
        );
        assert!(
            !engine.slots[0]
                .calls
                .iter()
                .any(|call| call.kind == CallKind::Leave)
        );

        let empty = ProviderStatus { slots: Vec::new() };
        engine
            .advance_slot(0, &tick(31), Some(&observation(29, empty.clone())), false)
            .unwrap();
        assert_eq!(engine.slots[0].state, SlotState::Reconciling);
        engine
            .advance_slot(0, &tick(31), Some(&observation(30, empty)), false)
            .unwrap();
        assert_eq!(engine.slots[0].state, SlotState::Vacant);
    }

    #[test]
    fn shutdown_never_sends_a_leave_after_a_timing_gap() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();

        engine.begin_shutdown(0, 30).unwrap();

        assert_eq!(engine.slots[0].state, SlotState::Unmanaged);
        assert!(
            !engine.slots[0]
                .calls
                .iter()
                .any(|call| call.kind == CallKind::Leave)
        );
    }

    #[tokio::test]
    async fn non_monotonic_status_cannot_roll_back_confirmed_preimage() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.advance_slot(0, &tick(3), None, false).unwrap();
        engine.advance_slot(0, &tick(6), None, false).unwrap();
        let key = engine.slots[0].key;

        engine
            .advance_slot(
                0,
                &tick(15),
                Some(&observation(15, status_for(key, 15))),
                false,
            )
            .unwrap();
        engine
            .advance_slot(
                0,
                &tick(18),
                Some(&observation(18, status_for(key, 12))),
                false,
            )
            .unwrap();

        assert_eq!(
            engine.slots[0]
                .generation
                .as_ref()
                .unwrap()
                .confirmed_through,
            Some(15)
        );
    }

    #[test]
    fn contract_confirmation_overrides_local_broadcast_expiry() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.slots[0].calls[0].broadcast_state = BroadcastState::Expired;

        engine.confirm_through(0, 9);

        assert_eq!(
            engine.slots[0].calls[0].broadcast_state,
            BroadcastState::Accepted
        );
    }

    #[test]
    fn status_outage_keeps_only_bounded_reconciliation_history() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();

        for current_tick in (3..=60).step_by(STREAM_COUNT as usize) {
            engine.extend_normal_chain(0, current_tick).unwrap();
            for call in &mut engine.slots[0].calls {
                if call.target_tick <= current_tick {
                    call.broadcast_state = BroadcastState::Accepted;
                }
            }
            engine.prune_completed_calls(0, current_tick);
        }

        let past_calls = engine.slots[0]
            .calls
            .iter()
            .filter(|call| call.target_tick < 60)
            .count();
        assert!(past_calls <= MAX_QUEUED_CALLS);
    }

    #[tokio::test]
    async fn planned_call_is_still_attempted_after_poll_skips_six_tick_boundary() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(Arc::clone(&backend));
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();

        engine.dispatch_calls(0, 4, 1_000);
        wait_for_broadcasts(&backend, 1).await;

        assert!(engine.slots[0].calls[0].ever_attempted);
        assert_eq!(transaction_tick(&backend.broadcasts.lock().unwrap()[0]), 9);
    }

    #[test]
    fn shutdown_waits_for_every_prerequisite_call() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.begin_shutdown(0, 3).unwrap();
        let terminal_target = match engine.slots[0].state {
            SlotState::Stopping { terminal_target } => terminal_target,
            state => panic!("unexpected state: {state:?}"),
        };

        engine.slots[0]
            .calls
            .iter_mut()
            .find(|call| call.target_tick == terminal_target)
            .unwrap()
            .broadcast_state = BroadcastState::Accepted;
        engine.update_shutdown_slot(0);
        assert!(matches!(engine.slots[0].state, SlotState::Stopping { .. }));

        for call in &mut engine.slots[0].calls {
            call.broadcast_state = BroadcastState::Accepted;
        }
        engine.update_shutdown_slot(0);
        assert_eq!(
            engine.slots[0].state,
            SlotState::ShutdownComplete(DrainOutcome::Accepted)
        );
    }

    #[test]
    fn epoch_change_discards_old_work_and_enforces_warmup() {
        use chrono::TimeZone as _;

        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        let outside_stop_window = Utc.with_ymd_and_hms(2026, 7, 30, 12, 0, 0).unwrap();
        engine.update_epoch_phase(
            &TickInfo {
                epoch: 7,
                tick: 100,
                initial_tick: 0,
                tick_duration_ms: 1_000,
            },
            outside_stop_window,
        );
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 108).unwrap();

        engine.update_epoch_phase(
            &TickInfo {
                epoch: 8,
                tick: 1_000,
                initial_tick: 1_000,
                tick_duration_ms: 1_000,
            },
            outside_stop_window,
        );
        assert_eq!(engine.epoch_phase, EpochPhase::Warmup { epoch: 8 });
        assert_eq!(engine.slots[0].state, SlotState::Unknown);
        assert!(engine.slots[0].calls.is_empty());

        engine.update_epoch_phase(
            &TickInfo {
                epoch: 8,
                tick: 1_050,
                initial_tick: 1_000,
                tick_duration_ms: 1_000,
            },
            outside_stop_window,
        );
        assert_eq!(engine.epoch_phase, EpochPhase::Active);
    }

    #[test]
    fn epoch_stop_window_is_utc_wednesday_boundary() {
        use chrono::TimeZone as _;

        assert!(!is_epoch_stop_window(
            Utc.with_ymd_and_hms(2026, 7, 29, 11, 49, 59).unwrap(),
            600
        ));
        assert!(is_epoch_stop_window(
            Utc.with_ymd_and_hms(2026, 7, 29, 11, 50, 0).unwrap(),
            600
        ));
    }

    #[tokio::test]
    async fn later_local_status_confirms_every_previous_call() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.advance_slot(0, &tick(9), None, false).unwrap();
        let status = status_for(engine.slots[0].key, 15);

        engine
            .advance_slot(0, &tick(15), Some(&observation(15, status)), false)
            .unwrap();

        assert_eq!(
            engine.slots[0]
                .generation
                .as_ref()
                .unwrap()
                .confirmed_through,
            Some(15)
        );
        assert_eq!(engine.slots[0].state, SlotState::Predicting);
    }

    #[test]
    fn unknown_newer_status_makes_slot_unmanaged() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        let status = status_for(engine.slots[0].key, 10);

        engine
            .advance_slot(0, &tick(10), Some(&observation(10, status)), false)
            .unwrap();

        assert_eq!(engine.slots[0].state, SlotState::Unmanaged);
        assert!(engine.slots[0].generation.is_none());
    }

    #[tokio::test]
    async fn retry_reuses_identical_bytes_and_other_stream_progresses() {
        let backend = Arc::new(MockBackend {
            failures: AtomicUsize::new(1),
            ..MockBackend::default()
        });
        let mut engine = test_engine(Arc::clone(&backend));
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.dispatch_calls(0, 3, 1_000);
        wait_for_broadcasts(&backend, 1).await;
        engine.slots[1].state = SlotState::Vacant;
        engine.start_generation(1, 10).unwrap();
        engine.dispatch_calls(1, 4, 1_000);
        wait_for_broadcasts(&backend, 2).await;
        engine.harvest_broadcasts(0).await;
        engine.harvest_broadcasts(1).await;
        engine.dispatch_calls(0, 4, 1_000);
        wait_for_broadcasts(&backend, 3).await;

        let broadcasts = backend.broadcasts.lock().unwrap();
        assert_eq!(broadcasts[0], broadcasts[2]);
        assert_ne!(broadcasts[0], broadcasts[1]);
        assert_eq!(
            engine.slots[1].calls[0].broadcast_state,
            BroadcastState::Accepted
        );
    }

    #[tokio::test]
    async fn stalled_broadcast_does_not_block_another_stream() {
        let backend = Arc::new(FirstBroadcastBlocksBackend::default());
        let network_backend: Arc<dyn NetworkBackend> = backend.clone();
        let mut engine = ProviderEngine::new(
            network_backend,
            QubicWallet::from_seed(&"a".repeat(55)).unwrap(),
            1,
            600,
            50,
        );
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.dispatch_calls(0, 3, 1_000);
        wait_for_counter(&backend.broadcast_calls, 1).await;
        engine.slots[1].state = SlotState::Vacant;
        engine.start_generation(1, 10).unwrap();
        engine.dispatch_calls(1, 4, 1_000);
        wait_for_counter(&backend.broadcast_calls, 2).await;
        engine.harvest_broadcasts(1).await;

        assert!(engine.slots[0].calls[0].broadcast.is_some());
        assert_eq!(
            engine.slots[1].calls[0].broadcast_state,
            BroadcastState::Accepted
        );

        backend.release_first.notify_one();
        finish_all_broadcasts(&mut engine, 0).await;
    }

    #[tokio::test]
    async fn shutdown_adds_terminal_leave_without_restart() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.advance_slot(0, &tick(6), None, true).unwrap();

        let terminal_target = match engine.slots[0].state {
            SlotState::Stopping { terminal_target } => terminal_target,
            state => panic!("unexpected state: {state:?}"),
        };
        let terminal = engine.slots[0]
            .calls
            .iter()
            .find(|call| call.target_tick == terminal_target)
            .unwrap();
        assert_eq!(terminal.kind, CallKind::Leave);
        assert_eq!(&terminal.tx_bytes[80 + 512..80 + 544], &[0; 32]);
        assert!(
            !engine.slots[0]
                .calls
                .iter()
                .any(|call| call.target_tick > terminal_target)
        );
    }

    #[tokio::test]
    async fn delayed_status_started_before_deadline_cannot_trigger_recovery() {
        let backend = Arc::new(MockBackend::default());
        let mut engine = test_engine(backend);
        engine.slots[0].state = SlotState::Vacant;
        engine.start_generation(0, 9).unwrap();
        engine.advance_slot(0, &tick(3), None, false).unwrap();
        engine.advance_slot(0, &tick(6), None, false).unwrap();
        engine.advance_slot(0, &tick(9), None, false).unwrap();
        let empty = ProviderStatus { slots: Vec::new() };

        engine
            .advance_slot(0, &tick(15), Some(&observation(8, empty)), false)
            .unwrap();

        assert_eq!(engine.slots[0].state, SlotState::Predicting);
    }

    fn tick(tick: u32) -> TickInfo {
        TickInfo {
            tick,
            ..TickInfo::default()
        }
    }

    fn observation(requested_tick: u32, status: ProviderStatus) -> StatusObservation {
        StatusObservation {
            requested_tick,
            status,
        }
    }

    fn status_for(key: SlotKey, last_update_tick: u32) -> ProviderStatus {
        ProviderStatus {
            slots: vec![ProviderSlot {
                key,
                locked_collateral: 1,
                contributed_to_entropy: true,
                last_update_tick,
            }],
        }
    }

    fn transaction_tick(bytes: &[u8]) -> u32 {
        u32::from_le_bytes(bytes[72..76].try_into().unwrap())
    }

    fn test_engine(backend: Arc<MockBackend>) -> ProviderEngine {
        ProviderEngine::new(
            backend,
            QubicWallet::from_seed(&"a".repeat(55)).unwrap(),
            1,
            600,
            50,
        )
    }

    async fn wait_for_broadcasts(backend: &MockBackend, expected: usize) {
        for _ in 0..50 {
            if backend.broadcasts.lock().unwrap().len() >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("expected {expected} broadcasts");
    }

    async fn wait_for_counter(counter: &AtomicUsize, expected: usize) {
        for _ in 0..50 {
            if counter.load(Ordering::Relaxed) >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("expected {expected} calls");
    }

    async fn finish_all_broadcasts(engine: &mut ProviderEngine, index: usize) {
        for _ in 0..50 {
            tokio::task::yield_now().await;
            engine.harvest_broadcasts(index).await;
            if engine.slots[index]
                .calls
                .iter()
                .all(|call| call.broadcast.is_none())
            {
                return;
            }
        }
        panic!("broadcast tasks did not finish");
    }

    #[derive(Default)]
    struct MockBackend {
        broadcasts: Mutex<Vec<Vec<u8>>>,
        failures: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl NetworkBackend for MockBackend {
        async fn tick_info(&self) -> Result<TickInfo, BackendError> {
            panic!("not called")
        }

        async fn balance(&self, _identity: &str) -> Result<u64, BackendError> {
            panic!("not called")
        }

        async fn query_contract_function(
            &self,
            _request: ContractFunctionRequest,
        ) -> Result<Vec<u8>, BackendError> {
            panic!("not called")
        }

        async fn broadcast_transaction(&self, tx_bytes: Vec<u8>) -> Result<String, BackendError> {
            self.broadcasts.lock().unwrap().push(tx_bytes);
            if self
                .failures
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_sub(1)
                })
                .is_ok()
            {
                return Err(BackendError::new("ambiguous test failure"));
            }
            Ok("test-transaction".to_string())
        }
    }

    #[derive(Default)]
    struct StatusErrorBackend {
        status_calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl NetworkBackend for StatusErrorBackend {
        async fn tick_info(&self) -> Result<TickInfo, BackendError> {
            Ok(tick(12))
        }

        async fn balance(&self, _identity: &str) -> Result<u64, BackendError> {
            Ok(0)
        }

        async fn query_contract_function(
            &self,
            _request: ContractFunctionRequest,
        ) -> Result<Vec<u8>, BackendError> {
            self.status_calls.fetch_add(1, Ordering::Relaxed);
            Err(BackendError::new("status unavailable"))
        }

        async fn broadcast_transaction(&self, _tx_bytes: Vec<u8>) -> Result<String, BackendError> {
            Ok("test-transaction".to_string())
        }
    }

    #[derive(Default)]
    struct FirstBroadcastBlocksBackend {
        broadcast_calls: AtomicUsize,
        release_first: Notify,
    }

    #[async_trait::async_trait]
    impl NetworkBackend for FirstBroadcastBlocksBackend {
        async fn tick_info(&self) -> Result<TickInfo, BackendError> {
            panic!("not called")
        }

        async fn balance(&self, _identity: &str) -> Result<u64, BackendError> {
            panic!("not called")
        }

        async fn query_contract_function(
            &self,
            _request: ContractFunctionRequest,
        ) -> Result<Vec<u8>, BackendError> {
            panic!("not called")
        }

        async fn broadcast_transaction(&self, _tx_bytes: Vec<u8>) -> Result<String, BackendError> {
            let call = self.broadcast_calls.fetch_add(1, Ordering::Relaxed);
            if call == 0 {
                self.release_first.notified().await;
            }
            Ok(format!("test-transaction-{call}"))
        }
    }

    #[allow(dead_code)]
    fn empty_status_bytes() -> Vec<u8> {
        vec![0; PROVIDER_STATUS_SIZE]
    }
}
