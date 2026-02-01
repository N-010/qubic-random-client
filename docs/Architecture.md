# Архитектура клиента Random

## Базовая
- **CLI/Runtime**: парсинг аргументов, запуск async-рантайма, graceful shutdown.
- **Tick Source**: отдельная корутина получает текущий тик и/или подписывается на broadcast, выдача `TickInfo`.
- **Pipeline Commit → Pending → Reveal**: очередь pending, раскрытие через `+3` тика, пул корутин для отправки Reveal/Commit.
- **Balance Watcher**: отдельная корутина периодически выводит баланс пользователя.
- **SC Transport (абстракция)**: отправка `RevealAndCommit` в контракт.
- **State/Storage**: in-memory + опциональная персистенция pending.

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
- `Config`: `workers`, `seed`, `reveal_delay_ticks`, `tx_tick_offset`, `commit_amount`, `pipeline_sleep_ms`, `contract_id`, `endpoint`, `data_dir`, `persist_pending`.
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

- CLI: --seed required; --endpoint used for RPC; SC interaction via SCAPI RequestDataBuilder.
- commit digest = K12(revealedBits), revealedBits generated via OS CSPRNG.

## Shutdown behavior (ASCII)
- On shutdown, if there is a pending commit waiting to be revealed, the pipeline returns a self-reveal job to the main task.
- The main task sends this reveal synchronously before exit (not via background workers) to avoid Ctrl-C races.
- This shutdown reveal uses amount=0 and sets committed_digest = K12(revealed_bits), so it does not create a new paid commit.
- The shutdown reveal tick is max(pending.reveal_send_at_tick, current_tick + tx_tick_offset) to avoid using an outdated tick.
