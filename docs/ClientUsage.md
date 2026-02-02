# Как работать с клиентом Random

Документ описывает запуск и практическую эксплуатацию клиента Random SC.

## Быстрый старт

1) Сборка:

```bash
cargo build
```

2) Запуск (seed обязателен; по умолчанию stdin/TTY):

```bash
cargo run -- --seed <55-символьный seed из a-z>
```

Или просто запустите и введите seed (без отображения):

```bash
cargo run
```

## Требования к seed

- Длина: ровно 55 символов.
- Допустимые символы: только `a-z` (строчные латинские).
- Seed хранится в памяти в заблокированном буфере и затирается при выходе.

## Основные параметры CLI

Клиент — бинарь `random-client` (см. `Cargo.toml`). Если `--seed` не указан, seed читается из stdin/TTY по умолчанию.

```text
--seed <STRING>                Seed (55 символов a-z); если не указан, читается из stdin/TTY
--senders <N>                  Параллельные отправители Reveal/Commit: больше = быстрее отправка и выше нагрузка; 0 = авто (по ядрам)
--reveal-delay-ticks <N>       Сколько тиков ждать между commit и reveal (по умолчанию 3)
--reveal-send-guard-ticks <N>  На сколько тиков раньше планового reveal можно отправлять (по умолчанию 5)
--commit-amount <N>            Сумма депозита/ставки в каждой транзакции; влияет на риск и награду
--commit-reveal-pipeline-count <N> Количество параллельных pipeline commit/reveal (цепочек commit→reveal)
--runtime-threads <N>          Потоки Tokio для выполнения задач: больше = выше параллельность и нагрузка; 0 = авто (по числу логических ядер)
--heap-dump                    Снять heap-профиль jemalloc при старте
--heap-stats                   Печатать статистику аллокатора при завершении (Ctrl+C)
--heap-dump-interval-secs <N>  Интервал периодических heap-дампов в секундах (0 = отключено)
--tick-poll-interval-ms <N>    Как часто опрашивать текущий тик (мс)
--endpoint <URL>               RPC endpoint для запросов и отправки транзакций
--balance-interval-ms <N>      Интервал запроса баланса (мс)
```

## Heap-профилирование (jemalloc)

- Сборка с jemalloc: `cargo build --features jemalloc`.
- Включите профилирование до старта: `JEMALLOC_CONF=prof:true,prof_active:true,lg_prof_interval:30`.
- Снять дамп при старте: `--heap-dump`, либо периодически: `--heap-dump-interval-secs`.
- Печатать статистику аллокатора при завершении: `--heap-stats`.
- Для пути дампов используйте `JEMALLOC_CONF=prof_prefix:/path/prefix`.
- Профилирование jemalloc не поддерживается на MSVC.

## Статистика аллокатора на Windows (mimalloc)

- Сборка с mimalloc: `cargo build --features mimalloc`.
- Используйте `--heap-stats`, чтобы печатать статистику при завершении (например, Ctrl+C).
- `MIMALLOC_SHOW_STATS=1` также печатает статистику при завершении.
- Флаги `--heap-dump` требуют jemalloc и недоступны на MSVC.

## Как работает pipeline

- Логика строится вокруг процедуры `RANDOM::RevealAndCommit()`.
- Сначала отправляется commit (публикация digest), затем через `+3` тика — reveal + новый commit.
- `revealedBits` раскрывают энтропию предыдущего commit, `committedDigest` — digest для следующего reveal.
- Каждая транзакция отправляется с `commit_amount` (reveal-only не используется).
- Если доступный баланс ниже `commit_amount`, pipeline приостанавливается.
- Планирование транзакции делается на будущий тик: `current_tick + reveal_delay_ticks`.
- Для защиты от не-раскрытия используется депозит: при commit депозит удерживается контрактом, при своевременном reveal возвращается; иначе — сгорает.

## Работа с RPC

- Отправка транзакций происходит через RPC endpoint, указанный в `--endpoint`.
- Запросы тика/баланса идут по SCAPI v0.2 (см. `docs/Architecture.md`).

## Остановка клиента

- При завершении, если есть pending commit, клиент синхронно отправляет reveal перед выходом.
- Reveal на выходе использует amount=0 и не делает новый commit.

## Типичные ошибки

- `seed from stdin is empty`: stdin/TTY оказался пустым.
- `seed must be 55 characters`: неверная длина.
- `seed must contain only a-z characters`: недопустимые символы.
- Ошибки `VirtualLock/mlock`: система не дала закрепить память для seed.

## Примеры

Запуск с пользовательским endpoint и депозитом:

```bash
cargo run -- \
  --seed <seed> \
  --endpoint https://rpc.qubic.org/live/v1/ \
  --commit-amount 25000
```

Запуск с авто-отправителями:

```bash
cargo run -- --senders 0
```
