# callix

Ядро полиглотного анализа кода: разбирает Python, TypeScript, Go и Rust в общий
граф-IR (узлы, рёбра, спаны) и отдаёт его Python-коду. На Rust написано всё —
парсинг, модели графа, анализ, извлечение межъязыковых границ и резолв
символов, включая сам тайп-чекер [ty](https://github.com/astral-sh/ty),
слинкованный прямо в модуль. Python-часть пакета — только фасад с
реэкспортами. Ставится одним `pip install`: для Python и TypeScript
внешних бинарей не нужно вообще, резолверам Go и Rust нужен тулчейн
своего языка (см. соответствующие разделы).

Это переработка [graphlens](https://github.com/Neko1313/graphlens) с
переносом парсинга на Rust. Публичный API совместим: те же 14 `NodeKind`,
12 `RelationKind`, те же детерминированные ID (`sha256[:16]`) и тот же
формат сериализации — граф от graphlens читается callix'ом и наоборот.
Структурная часть графа совпадает с graphlens побайтово; единственное
намеренное расхождение — ключ неразрешённых `EXTERNAL_SYMBOL`, см.
[Отличия от graphlens](#отличия-от-graphlens).

## Установка

```bash
pip install callix
```

Требуется Python ≥ 3.13. Тайп-чекеры Python и TypeScript вместе со
стабами typeshed и `lib.d.ts` уже внутри wheel. Анализ Go требует
установленного Go, анализ Rust — `rust-analyzer` и Cargo.

## Использование

```python
from callix import NodeKind, PythonAdapter

graph = PythonAdapter().analyze("path/to/project")

print(len(graph.nodes), len(graph.relations))
for node in graph.nodes_by_kind(NodeKind.FUNCTION):
    print(node.qualified_name, graph.callers(node.id))

graph.to_json(indent=2)          # сериализация
old.diff(new)                    # структурный diff
```

Резолвер символов подключён по умолчанию, поэтому межфайловые связи
(CALLS / REFERENCES / HAS_TYPE / INHERITS_FROM) строятся сразу:

```python
PythonAdapter()                          # встроенный ty
PythonAdapter(resolve=False)             # без резолва, только структура
PythonAdapter(resolver=my_resolver)      # свой резолвер
PythonAdapter(dep_parsers=[my_parser])   # свои парсеры манифестов
```

Без резолва граф остаётся структурным, а `graph.metadata["resolver_status"]`
становится `unavailable`.

Свой резолвер — любой объект с `prepare(root, files)`, `resolve_all(queries)`
и `status()`; свой парсер манифеста — объект с `can_parse(root)` и
`parse(root)`. Оба вызываются из Rust через обычный Python-протокол.

## Межъязыковые границы

`analyze()` попутно находит порты на стыке сервисов и заводит для них
узлы `BOUNDARY` с рёбрами `EXPOSES` / `CONSUMES` от объемлющей функции.
Распознаются маршруты FastAPI / Flask / Starlette и клиенты
requests / httpx (`http`), servicer-классы и стабы (`grpc`), активности
Temporal (`temporal`), publish / subscribe (`queue`).

ID узла выводится только из механизма и нормализованного ключа, поэтому
Python-сервер с `@app.get("/users/{id}")` и клиент на другом языке с
`fetch("/users/1")` дают **один и тот же узел** и склеиваются при слиянии
графов — оба ключа нормализуются в `GET /users/{}`.

## Разработка

Задачи описаны в `taskfile.dist.yaml` ([Task](https://taskfile.dev)):

```bash
task                             # сборка → генерация .pyi → example/test.py
task example -- example/foo.py   # другой скрипт
task build DEBUG=1               # debug-сборка, только под отладчик
task --list                      # остальное: lint, check, test, stubs:check
```

Первая сборка долгая: дерево ty/ruff — около 200 крейтов. Дальше идёт
инкрементально. Нужен Rust ≥ 1.95.

Сборка релизная не случайно: в debug встроенный ty медленнее примерно в
15 раз (резолв graphlens — 12.5s против 0.8s), и цикл разработки
превращается в ожидание.

Форматирование местами ручное, поэтому `cargo fmt` в задачи не вынесен:
он переписал бы код под свой стиль. `task lint` показывает подсказки
clippy, но не валит сборку из-за них.

Стабы `.pyi` генерируются автоматически (`pyo3-stub-gen`) в
`python/callix/_core/` и коммитятся — CI падает, если сгенерированное
разошлось с закоммиченным. Руками их править не нужно.

## Про встроенный ty

Резолвер вызывает ty как библиотеку (`ty_ide::goto_definition`), а не
поднимает `ty server` подпроцессом. Это убирает JSON-RPC и сериализацию:
на graphlens резолв 33k позиций занимает 0.85s вместо 4.92s, весь
`analyze()` — 1.14s вместо 5.40s.

У такого решения есть цена. Всё перечисленное — осознанный размен, а не
баги к исправлению.

**Пин на коммит ruff.** Крейты `ty_ide` и `ty_project` помечены
`publish = false` и на crates.io не выкладываются, поэтому подключены как
git-зависимость с `rev`, закреплённым на коммит, соответствующий тегу ty
(сейчас — ty 0.0.52). Обновление ty — ручная операция: найти новый коммит
(`gh api repos/astral-sh/ty/contents/ruff?ref=<тег>`), поменять `rev` во
всех записях `Cargo.toml`, пересобрать и сверить результат. API этих
крейтов внутренний и меняется без предупреждения.

**Хрупкая сборка.** Вне workspace ruff фичи не наследуются, поэтому
`get-size2` закреплён ровно на `=0.10.0` с ручным списком фич: в 0.10.3
крейт перешёл на `compact_str` 0.10, а ruff собран с 0.9, и без пина
`ruff_python_ast` не компилируется вовсе.

**Размер модуля.** `_core.abi3.so` — около 21 MB вместо 2.5 MB без ty:
внутри тайп-чекер и стабы typeshed.

**Время сборки.** Холодная сборка занимает десятки минут. В CI это
закрыто кешем cargo, локально — инкрементальными пересборками.

**Редкие расхождения с `ty server`.** Встроенный резолвер изредка
указывает не на ту цель, что LSP: `goto_definition` возвращает файл во
встроенном typeshed там, где `ty server` отдаёт путь в проекте, и
occurrence уходит в `EXTERNAL_SYMBOL` вместо привязки к узлу графа.
Причина — ty server помечает файлы открытыми (`is_open_file`), а
поднятая напрямую `ProjectDatabase` этого не делает. Масштаб: на
apache/superset разошлись 22 узла из 225 706 (0.01%) при полном
совпадении рёбер (556 571) и доли разрешённого (352 634 из 418 944).

## TypeScript

TypeScript поддержан наравне с Python — тем же API:

```python
from callix import TypeScriptAdapter

graph = TypeScriptAdapter().analyze("path/to/ts-project")
TypeScriptAdapter(resolve=False).analyze(root)      # только структура
```

Разбираются `.ts`, `.tsx`, `.mts`, `.cts` (файлы деклараций `.d.ts`
пропускаются), монорепозитории с вложенными `package.json` и
`tsconfig.json`, алиасы путей из `compilerOptions.paths`, зависимости из
четырёх секций `package.json` и встроенные модули Node.

Резолвер символов работает поверх
[typescript-go](https://github.com/microsoft/typescript-go) — официального
порта компилятора TypeScript на Go. Он слинкован в модуль статически,
вместе с `lib.d.ts`, поэтому ни `tsc`, ни `tsgo`, ни Node в системе не
нужны. Его можно использовать и отдельно:

```python
from callix import TsResolver

resolver = TsResolver()
resolver.prepare("path/to/ts-project")     # найдёт tsconfig.json сам
resolver.definition_at("src/main.ts", 4, 15)
```

Первый запрос строит программу и занимает десятки миллисекунд, дальше
идут десятки микросекунд.

Мост живёт в `go/bridge.go` и собирается `build.rs` в c-archive. Весь
typescript-go лежит в `internal/`, куда Go запрещает импорт из чужих
модулей, — поэтому модуль моста называется
`github.com/microsoft/typescript-go/callixbridge` и подключает клон
через `replace`. Правило Go проверяет путь импорта, а не расположение на
диске, так что форк ts-go не нужен: обновление — смена `TS_GO_REV` в
`build.rs`.

### Что нужно для сборки

Go ≥ 1.26 и сеть на первую сборку — `build.rs` клонирует typescript-go в
`.ts-go/` (~370 МБ, переживает `cargo clean`). Всё это нужно только тому,
кто собирает: в wheel уже лежит статически слинкованный код.

## Go

Go поддержан тем же API:

```python
from callix import GoAdapter

graph = GoAdapter().analyze("path/to/go-module")
GoAdapter(resolve=False).analyze(root)      # только структура
```

Разбираются модули по `go.mod` (включая монорепозитории с вложенными
модулями), зависимости из блочных и однострочных `require`, встраивание
структур и интерфейсов, методы с получателями. Границы: маршруты
gin/chi/echo, клиенты `net/http`, gRPC-стабы `New<Service>Client`,
Temporal и очереди.

Резолвер работает поверх `golang.org/x/tools/go/packages` и `go/types`,
слинкованных в тот же Go-мост. Проверено против `gopls`: ответы
совпадают до колонки.

**Этому резолверу нужен установленный Go.** В отличие от Python и
TypeScript, стандартную библиотеку Go вкомпилировать нельзя — это
исходники в GOROOT, и `packages.Load` вызывает `go list`. Для Go-проекта
это не проблема: Go там и так стоит, а graphlens требует вдобавок
отдельно поставленный `gopls`.

## Rust

Rust поддержан тем же API:

```python
from callix import RustAdapter

graph = RustAdapter().analyze("path/to/workspace")
RustAdapter(resolve=False).analyze(root)    # только структура
```

Разбираются крейты по `Cargo.toml` (включая воркспейсы с несколькими
участниками), зависимости из `dependencies` / `dev-dependencies` /
`build-dependencies`, встроенные модули `mod foo { ... }`, `impl` и
`impl Trait for Type`. Границы: маршруты axum `.route()`, атрибуты
actix/rocket `#[get("/x")]`, клиенты reqwest, gRPC-стабы tonic и очереди.

Резолвер работает не по LSP, а поверх пакетного индекса
`rust-analyzer scip`: интерактивный сервер держит состояние анализа всего
воркспейса в памяти и на больших проектах разрастается до десятков
гигабайт, тогда как индекс пишется один раз и читается статически.
Декодер SCIP — свой, без protobuf-рантайма: из occurrence нужны только
символ, роли и начало диапазона.

**Этому резолверу нужен установленный `rust-analyzer` (и Cargo).** Как и
у Go, вкомпилировать язык целиком нельзя: типизация опирается на исходники
стандартной библиотеки и на реальную сборку воркспейса. Для Rust-проекта
это не проблема — тулчейн там и так стоит. Если `rust-analyzer` пинится
через `rust-toolchain.toml` и в этом тулчейне компонента нет, callix
откатывается к тулчейну по умолчанию, а не молча отдаёт пустой резолв.

## Отличия от graphlens

**Классификация stdlib в Go.** graphlens разворачивает симлинки только у
пути цели, а GOROOT берёт как есть из `go env GOROOT`. На Debian-подобных
установках `/usr/lib/go-X/src` — симлинк на `/usr/share/go-X/src`, поэтому
вся стандартная библиотека попадает в `unknown`. callix сверяет исходный и
развёрнутый путь с обеих сторон.

**Ключ неразрешённых `EXTERNAL_SYMBOL` включает файл.** Когда резолвер
находит определение, но не может назвать его (`full_name` пуст), узлу
нужен искусственный ключ. В graphlens это `{role}@{line}:{col}` — без
пути к файлу, поэтому места с одинаковыми координатами в разных файлах
схлопываются в один узел. На apache/superset так слипались 98% внешних
узлов: 75806 из 77041. callix добавляет в ключ путь файла относительно
корня проекта:

```
graphlens:  read@399:60
callix:     read@superset/commands/importers/v1/utils.py:399:60
```

Путь именно относительный — иначе ID перестали бы совпадать между
машинами и сломали бы инкрементальные обновления. Правило одинаково для
всех четырёх языков. Следствие: ID таких узлов у callix и graphlens
разные, и на superset узлов становится 268 тысяч вместо 226 тысяч. Рёбра,
структурные узлы и доля разрешённого не меняются.

## Точки расширения

В Rust уехало всё, включая разбор манифестов и оркестрацию `analyze()`.
Python-код при этом никуда не делся из картины: и резолвер, и парсеры
зависимостей можно подменить своими объектами — Rust вызывает их через
обычный протокол. Это же относится к `callix.TyResolver`, который просто
оборачивает встроенный ty и может быть заменён на что угодно с тем же
интерфейсом.

## Лицензия

MIT
