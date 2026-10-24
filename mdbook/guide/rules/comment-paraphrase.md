# `slop/comment-paraphrase`

**Built-in level:** `warning`

## What it catches

A comment that restates the statement below it. Detected *structurally*, not by a phrase list: the comment's non-stopword tokens are compared against the identifiers of the code that follows (camelCase and snake_case are split into tokens, and a small RU→EN glossary lets a Russian comment restate an English identifier). The verdict is per comment block (adjacent same-column lines) and produces one finding at the block's first line. It fires only when at least two identifier tokens are shared *and* they make up at least 30% of the comment's words; comment blocks over two lines, `//!` inner docs, and any block carrying a *why* marker ("because", "otherwise", "so", "иначе", …) or a referent (URL, decision-doc id, issue ref, path) are never flagged. A few carve-outs cancel the why exemption — a "why" about deploy mechanics, another service's internals or permission to exist ("deliberately", "by design") is still judged on overlap.

## Legitimate exception

> A comment whose overlap with the identifiers below explains WHY the code is shaped that way (a constraint the code cannot express), not WHAT the code does. The verdict covers the whole adjacent comment block: a cause, consequence ('because', 'so') or referent (URL, ADR-064:, path) on ANY line exempts it, and blocks over 2 lines or with shared identifiers under 30% of their words are never flagged. 'Why' comments are the point of commenting.

## Do instead

Delete the restatement — the code already says it. Fires once per 1-2 line comment block whose words are mostly the identifiers of the statement below. Keep the comment only if it states a constraint the code cannot (why this shape, what breaks otherwise); rewrite it in those terms.

## Example

Bad:

```rust
// retry after header parse
fn parse_retry_after_header() {}
```

```console
$ agent-slop check example.rs
example.rs:1:1 slop/comment-paraphrase — restates the code below (retry, after, header, parse)
```

Good:

```rust
// The proxy drops the header, so we parse it from the body instead.
fn parse_retry_after_header() {}
```

The good version produces no finding for this rule.

## Match configuration

- `min_shared_identifiers` = `2`
- `min_comment_words` = `3`
- `min_shared_ratio` = `0.3`
- `max_block_lines` = `2`

<details>
<summary><code>stopwords</code> (37 entries)</summary>

```text
the
a
an
this
that
of
to
for
and
or
is
are
was
in
on
it
its
with
as
by
be
we
get
set
return
returns
check
checks
if
then
value
values
list
map
string
number
int
```

</details>

<details>
<summary><code>ru_glossary</code> (82 entries)</summary>

```text
порт
port
сервис
service
сервер
server
сеть
network
ключ
key
путь
path
файл
file
каталог
directory
пользователь
user
пароль
password
токен
token
список
list
очередь
queue
таймер
timer
время
time
дата
date
имя
name
группа
group
хост
host
версия
version
запрос
request
ответ
response
прокси
proxy
домен
domain
база
database
таблица
table
модель
model
режим
mode
уровень
level
лимит
limit
включ
enable
выключ
disable
проверка
check
кэш
cache
журнал
log
поток
thread
задача
task
ссылка
url
секция
section
значение
value
доступ
access
```

</details>

<details>
<summary><code>why_markers</code> (25 entries)</summary>

```text
because
otherwise
since
due to
so that
so
without
unless
breaks
fails
else
instead of
requires
would
иначе
из-за
потому что
так как
чтобы
без
ломает*
падает
упад*
требует
вместо
```

</details>

<details>
<summary><code>why_exemption_cancels</code> (27 entries)</summary>

```text
comes from vault
from the deploy
deploy repo
helm chart
avoids the port
port clash
chains to the ready
env_prefix
reads flat env
as a last resort
deliberately
by design
intentional
allowed here
rpc boundary
architecture:
приходит из vault
из vault
цепляется к готовым
избегает порта
конфликта портов
читает плоские
как api ходит
намеренно
так задумано
импорт допустим
граница rpc
```

</details>

## Configure

```toml
[slop.rules]
comment-paraphrase = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/comment-paraphrase -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
