# Telegram Book Downloader

Локальное нативное приложение на Rust для скачивания книг из Telegram-каналов
личного аккаунта. Целевые платформы: **macOS Apple Silicon** и **Windows 10/11 x64**.
GPUI рисует интерфейс непосредственно через платформенный backend:
WebView, Tauri, Electron, JavaScript, Docker и сервер приложению не нужны.

## Текущий статус

Это **базовая инфраструктура, ещё не функциональный Telegram MVP**.
Приложение запускает нативное окно, создаёт локальную SQLite и обменивается
типизированными командами/событиями между GPUI и Core на Tokio.
Для запуска **не нужны Telegram API credentials**; соединения с Telegram нет.

| Область | Что работает |
| --- | --- |
| Workspace | Пять crate, общие зависимости и линты, `Cargo.lock`, закреплённый stable Rust |
| UI | Тёмная оболочка в стиле Delta, сворачиваемый sidebar, Вход, Каналы, Загрузки, Библиотека, Настройки |
| Core ↔ GPUI | Ограниченные асинхронные каналы, обновление через `Entity::update` и `Context::notify` |
| Storage | SQLx, встроенная миграция, SQLite WAL, сохранение/чтение истории, атомарная дедупликация |
| Downloader | FIFO-очередь, фильтр шести форматов, отклонение защищённого контента |
| Telegram | Grammers 0.11 transport adapter с управлением временем жизни задачи; валидация и redacted `Debug` credentials |
| Проверки | Модульные тесты, disk-backed SQLite test, тест протокола после перезапуска, GPUI entity tests |
| CI | Нативные сборки, fmt, Clippy и тесты на macOS ARM64 и Windows x64 |

Экран загрузок читает **настоящую SQLite**, а не демонстрационные данные.
Кнопка «Обновить» отправляет команду Core и асинхронно получает историю.
Библиотека показывает записи со статусом `Completed`.
Настройки пока доступны только для просмотра; их значения не редактируются.

### Следующая функциональная итерация

- Авторизация по номеру, коду и 2FA; восстановление подключения.
- Безопасный persistent session adapter: macOS Keychain / Windows Credential
  Manager или другое явно согласованное защищённое хранение.
- Получение каналов, сканирование сообщений, выбор одного вложения.
- Запись файла с проверкой имени/пути, прогресс, обновление статуса в SQLite.
- Выбор пользовательской директории, обработка flood wait и сетевых ошибок.

**Пока не реализованы:** вход в Telegram, сохранение Telegram-сессии,
получение каналов и фактическая передача файлов. MemorySession используется только
в offline-тесте транспортного адаптера, не как хранилище пользовательской сессии.
Автоматическое отслеживание каналов, восстановление очереди, переключение тем,
установщики и подпись бинарников отложены по ТЗ.

## Зависимости и воспроизводимость

- Rust **1.98.1 stable**, `rustfmt`, `clippy` — в `rust-toolchain.toml`.
- `gpui` и `gpui_platform` из официального Zed, **одна ревизия**
  [`504abf602a09edc3c7d5a4f4f60958aee335274f`](https://github.com/zed-industries/zed/tree/504abf602a09edc3c7d5a4f4f60958aee335274f).
  Версия Rust совпадает с toolchain этой ревизии.
- На macOS включён `gpui_platform/font-kit`: без него GPUI не отрисовывает текст.
- Grammers **0.11.0**, Tokio **1.54.1**, SQLx **0.9.0**.
- Прямые внешние зависимости закреплены через `=version`; Git-зависимости GPUI
  закреплены через `rev`, транзитивные зависимости — через `Cargo.lock`.

Первая сборка загружает зависимости из crates.io и официальных Git-репозиториев
и может занять несколько минут. Интернет требуется для сборки, но запущенный
инфраструктурный каркас работает без сети.
Используйте `--locked`, не удаляйте `Cargo.lock`.

## Запуск на macOS Apple Silicon

1. Установите Git, [Rust через rustup](https://rustup.rs/) и **полный Xcode**.
   Одних Command Line Tools недостаточно для Metal backend GPUI.
2. Откройте Xcode и завершите установку компонентов, включая Metal toolchain.
3. Проверьте активный Xcode и Metal:

   ```sh
   xcode-select -p
   xcrun --find metal
   ```

   Если выбран только каталог Command Line Tools, переключитесь на установленный
   Xcode: `sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer`.
   На новых версиях Xcode недостающий Metal toolchain можно установить через
   `xcodebuild -downloadComponent MetalToolchain`.

4. В корне репозитория:

   ```sh
   rustup show
   cargo run -p tgbooks-app --locked
   ```

Окно открывается на главном потоке, фоновые операции выполняются на Tokio.
Закрытие последнего окна или `Cmd+Q` завершает приложение и закрывает SQLite.

## Запуск на Windows 10/11 x64

1. Установите Git for Windows и [Rust через rustup](https://rustup.rs/).
   Используйте toolchain `x86_64-pc-windows-msvc`, не GNU.
2. Установите Visual Studio 2022 Build Tools с workload
   **Desktop development with C++**, MSVC и Windows 10/11 SDK.
3. Откройте PowerShell в корне репозитория:

   ```powershell
   rustup show
   cargo run -p tgbooks-app --locked
   ```

GPUI использует Win32, Direct3D и DirectWrite. Нужен графический сеанс и
совместимый GPU/драйвер. CI проверяет компиляцию и тесты, **не заменяя ручной запуск
на реальной Windows 10/11**.

## Управление

| Действие | macOS | Windows |
| --- | --- | --- |
| Переключение разделов | `Cmd+1` … `Cmd+5` | `Ctrl+1` … `Ctrl+5` |
| Показать / скрыть sidebar | `Cmd+B` | `Ctrl+B` |
| Обновить локальную историю | `Cmd+R` | `Ctrl+R` |
| Перемещение фокуса | `Tab`, `Shift+Tab` | `Tab`, `Shift+Tab` |
| Активировать кнопку | `Enter`, `Space` | `Enter`, `Space` |
| Завершить приложение | `Cmd+Q` | `Ctrl+Q` или закрытие окна |

Навигационные кнопки имеют роли и доступные имена, выбранное состояние,
hover/pressed-состояния и видимый фокус. Верхняя полоса — часть GPUI-интерфейса:
отдельный системный titlebar скрыт. На macOS сохранены настоящие traffic lights;
на Windows отрисовываются кнопки свернуть / развернуть / закрыть.
Свободная область верхней полосы позволяет перетаскивать окно;
двойной щелчок использует платформенное поведение.
Полная проверка VoiceOver/Narrator остаётся отдельной ручной проверкой.

## Локальные данные и безопасность

Путь определяется `directories::ProjectDirs` с идентификаторами
`org / tgbooks / TelegramBookDownloader`:

- macOS: `~/Library/Application Support/org.tgbooks.TelegramBookDownloader/`.
- Windows: `%LOCALAPPDATA%\tgbooks\TelegramBookDownloader\data\`.

Точный путь виден в разделе «Настройки». Внутри находятся `downloads.sqlite3`
(включая служебные WAL/SHM-файлы SQLite) и директория `books`.
На Unix новые директории создаются с mode `0700`; права уже существующих
директорий не меняются. На Windows используются унаследованные системные ACL.
**История не зашифрована и не предназначена для хранения секретов.**

Для разработки можно явно направить все данные в игнорируемый `target`:

```sh
TGBOOKS_DATA_DIR="$PWD/target/dev-data" cargo run -p tgbooks-app --locked
```

```powershell
$env:TGBOOKS_DATA_DIR = Join-Path (Get-Location).Path "target\dev-data"
cargo run -p tgbooks-app --locked
```

`TGBOOKS_DATA_DIR` должен быть абсолютным путём. Файлы данных не создаются
относительно текущей директории неявно.

- Не добавляйте API ID/hash, номер телефона, коды, пароли и сессии в Git,
  исходники, снимки тестов или логи.
- `.env`, session-файлы, SQLite и логи исключены через `.gitignore`.
  Само приложение **не загружает `.env`** и пока не читает credentials.
- Тип `ApiCredentials` проверяет формат и скрывает оба поля в `Debug`.
- Логируются только собственные targets `tgbooks`, `tgbooks::*`, `tgbooks_*`
  на уровне INFO и выше.
  `RUST_LOG` намеренно не включает подробные логи зависимостей/MTProto.
  SQL statement logging выключен.
- Внешние ошибки не передаются в UI в сыром виде: используются типизированные
  безопасные категории ошибок.
- Никакого обхода protected content: очередь отклоняет такие кандидаты.
  При подключении реального Telegram backend этот флаг нужно получать из API
  и проверять до передачи файла.

## Архитектура

```text
crates/
  app/          composition root, Tokio lifecycle, GPUI views и platform UI choices
  core/         модели, команды/события, DownloadHistory port, application service
  telegram/     Grammers transport; не экспортируется в GPUI views
  downloader/   правила допуска и FIFO-очередь, без Telegram/network
  storage/      SQLx, миграции, системные директории
```

Направление зависимостей: `app → core/storage`, `storage → core`,
`downloader → core`; `telegram` изолирует Grammers. В этой итерации Telegram и
Downloader намеренно ещё не подключены к пользовательскому workflow.
Core не зависит от GPUI, SQLx, Grammers или platform-specific API.

```text
GPUI view
  → CoreCommand через bounded async-channel (без блокировки UI)
  → application service на Tokio
  → DownloadHistory port → SQLx adapter
  → CoreEvent через bounded async-channel
  → GPUI Context::spawn → Entity::update → Context::notify
```

Миграции встроены в бинарник через `sqlx::migrate!`; `storage/build.rs` отслеживает
добавление SQL-файлов на stable Rust. `.gitattributes` фиксирует LF, чтобы хеши
миграций совпадали на обеих ОС. SQLx CLI, внешняя БД и `DATABASE_URL` не нужны.
Идентичность записи — `(channel_id, message_id, document_id)`,
а не имя файла; уникальность обеспечивается SQLite primary key.
Скачивание одного документа из разных сообщений пока считается разными записями.

## Проверки

Из корня репозитория:

```sh
cargo fmt --all -- --check
cargo build --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

GPUI tests включаются feature `gpui-tests`; стандартный `cargo test --workspace --locked`
запускает обычные unit/integration tests без этой дополнительной инфраструктуры.

Для визуальной проверки на macOS можно отрисовать все экраны через настоящий
Metal renderer с системным шрифтом, не запрашивая захват рабочего стола:

```sh
cargo run -p tgbooks-app --features gpui-tests --locked -- --render-ui-preview
```

PNG сохраняются в `target/ui-previews` (или `TGBOOKS_PREVIEW_DIR`).
Это opt-in инструмент разработки: он использует пустую тестовую историю,
не открывает SQLite и не подключается к Telegram. Нативные кнопки macOS
в PNG не попадают: они добавляются AppKit поверх GPUI.
Windows-превью пока не поддерживается renderer'ом закреплённого GPUI.

Тесты не требуют Telegram-аккаунта, credentials или сервера:

- Core: расширения, сериализация, корректность прогресса, протокол команд,
  безопасные ошибки и завершение сервиса.
- Downloader: FIFO, дубликаты в очереди и в работе, идентичные message ID в
  разных каналах, отклонение protected content.
- Storage: реальные временные SQLite-файлы, миграции, повторное открытие,
  Unicode-пути, дедупликация, проверка диапазонов чисел.
- App: история через UI-протокол после перезапуска хранилища; ошибки путей.
- GPUI: состояние entity, навигация и получение асинхронных событий без Tokio
  на UI-потоке.
- Telegram: формат и redaction credentials, создание/удаление транспорта
  без сетевых запросов.

Очистка backend зарегистрирована в `on_app_quit`, а не только после возврата
event loop: на macOS AppKit может завершить процесс, не возвращая управление
в `main`. У закреплённого GPUI бюджет quit hooks — 200 мс; при подключении
долгих загрузок потребуется отменять их до закрытия приложения.

Для работы только над неграфическими crate (в том числе на Linux):

```sh
cargo test -p tgbooks-core -p tgbooks-downloader -p tgbooks-storage -p tgbooks-telegram --locked
```

Полная Linux UI-сборка не входит в целевые платформы и не настроена.
GitHub Actions запускает полный набор проверок на `macos-15` (ARM64) и
`windows-2022` (x64). Workflow ничего не публикует и не подписывает.

## Официальные источники

Перед реализацией использованы:

- [GPUI README на закреплённой ревизии](https://github.com/zed-industries/zed/blob/504abf602a09edc3c7d5a4f4f60958aee335274f/crates/gpui/README.md),
  примеры `hello_world`, `tab_stop`, `a11y`, `on_window_close_quit`,
  исходники `Context`, `KeyBinding`, `TestAppContext`.
- [Grammers client 0.11](https://docs.rs/grammers-client/0.11.0/grammers_client/),
  [SenderPool](https://docs.rs/grammers-client/0.11.0/grammers_client/struct.SenderPool.html)
  и официальный [пример dialogs](https://github.com/Lonami/grammers/blob/master/grammers-client/examples/dialogs.rs).
- [Tokio Runtime](https://docs.rs/tokio/1.54.1/tokio/runtime/struct.Runtime.html).
- [SQLx 0.9 migrations](https://docs.rs/sqlx/0.9.0/sqlx/macro.migrate.html) и
  [SQLite options](https://docs.rs/sqlx/0.9.0/sqlx/sqlite/struct.SqliteConnectOptions.html).

Лицензия проекта: [MIT](LICENSE). Зависимости сохраняют собственные лицензии;
перед распространением установщиков потребуется собрать third-party notices.
