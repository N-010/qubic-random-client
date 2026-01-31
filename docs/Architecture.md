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
- `Config`: `threads`, `workers`, `seed`, `reveal_delay_ticks`, `deposit_amount`, `contract_id`, `endpoint`, `data_dir`, `persist_pending`.
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
