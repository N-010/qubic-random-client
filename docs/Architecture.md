# Архитектура клиента Random

## Базовая
- **CLI/Runtime**: парсинг аргументов, запуск async-рантайма, graceful shutdown.
- **Tick Source**: отдельная корутина получает текущий тик и/или подписывается на broadcast, выдача `TickInfo`.
- **Pipeline Commit → Pending → Reveal**: очередь pending, раскрытие через `+3` тика, пул корутин для отправки Reveal/Commit.
- **Balance Watcher**: отдельная корутина периодически выводит баланс пользователя.
- **SC Transport (абстракция)**: отправка `RevealAndCommit` в контракт.
- **State/Storage**: in-memory.

## Техническая
### Структура модулей
- `src/main.rs` — CLI и запуск.
- `src/app/mod.rs` — сборка компонентов и жизненный цикл.
- `src/config.rs` — `Config`.
- `src/ticks/mod.rs` — `TickSource`.
- `src/balance/mod.rs` — корутина вывода баланса.
- `src/pipeline/mod.rs` — логика commit/reveal.
- `src/pipeline/state.rs` — pending + сериализация (опц.).
- `src/transport/mod.rs` — адаптер `scapi` через абстракцию.
- `src/protocol/mod.rs` — типы протокола `RevealAndCommit`.
- `src/entropy/mod.rs` — генерация bits + digest.

### Контракт и интервал
- Контракт: `DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAANMIG`.
- Индекс контракта Random: `3`.
- Интервал Reveal → Commit: 3 тика.
- Каждая транзакция: Reveal для pending + новый Commit.

### Ключевые типы/трейты
- `AppConfig`: `seed`, `runtime: Config`.
- `Seed`: locked in-memory buffer, zeroized on drop.
- `Config`: `senders`, `reveal_delay_ticks`, `commit_amount`, `commit_reveal_pipeline_count`, `runtime_threads`, `contract_id`, `endpoint`, `balance_interval_ms`.
- `TickInfo`: `{ epoch: u16, tick: u32, tick_duration_ms: u16 }`.
- `TickSource`: `async fn next_tick(&mut self) -> TickInfo`.
- `ScTransport`: `async fn send_reveal_and_commit(input, amount) -> Result<TxId>`.
- `PendingItem`: `{ commit_tick, revealed_bits, committed_digest, amount }`.

### Tick/Balance через SCAPI v0.2
Методы SCAPI v2:
- `src/rpc/get/tick_info.rs` — текущий тик.
- `src/rpc/get/balances.rs` — баланс пользователя.
Использовать их в отдельных корутинах: `Tick Source` и `Balance Watcher`.


## Notes (ASCII)
- TickInfo uses u32 for epoch/tick/tick_duration_ms in this client.
- QubicWallet derives identity/signature from seed (K12 + FourQ).
- Seed handling: seed is kept in a locked in-memory buffer (mlock on Unix, VirtualLock on Windows), not cloned, and zeroized on drop; failure to lock aborts startup.

- CLI: seed via stdin/TTY by default; --seed overrides; --endpoint used for RPC; SC interaction via SCAPI RequestDataBuilder.
- commit digest = K12(revealedBits), revealedBits generated via OS CSPRNG.

## Shutdown behavior (ASCII)
- On shutdown, if there is a pending commit waiting to be revealed, the pipeline returns a self-reveal job to the main task.
- The main task sends this reveal synchronously before exit (not via background senders) to avoid Ctrl-C races.
- This shutdown reveal uses amount=0 and sets committed_digest = K12(revealed_bits), so it does not create a new paid commit.
- The shutdown reveal tick is max(pending.reveal_send_at_tick, current_tick + reveal_delay_ticks) to avoid using an outdated tick.

## Tick window for slow RPC polling (ASCII)
- Problem: frequent tick polling can trigger RPC rate limits; lowering poll frequency risks missing the exact reveal tick.
- Solution: use a configurable "send window" before the target reveal tick, so a late tick still triggers the send.
- Config: `reveal_send_guard_ticks` defines the window size (e.g. 10 ticks). When `now_tick >= reveal_send_at_tick - guard`, the pipeline sends reveal+commit.
- The reveal/commit interval remains `reveal_delay_ticks` (default 3); only the allowable send time is widened.
- Requirement: pick `reveal_send_guard_ticks` >= max expected tick polling gap to ensure at least one tick lands in the window.
