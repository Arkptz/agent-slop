# `slop/docstring-restates-name`

**Built-in level:** `warning`

## What it catches

A docstring (Python `"""` string statement) or colon-headed definition slot whose substantive words are all already the name of the declaration it documents: `def get_proxy_user_name` documented as "Get the proxy user name". It shares its token engine with `slop/comment-paraphrase` (stopwords, RU→EN glossary, English plural folding). A docstring must be strictly longer than two substantive words to be judged, and a definition-slot head (the clause before the first colon) needs at least three substantive words — below that, a heading equal to the name is idiomatic (Go doc comments open with the name). Dashed banners (`──`, `--`, `==`) are layout, not prose, and `#` headings without a colon are skipped.

## Legitimate exception

> A docstring that restates the name AND adds one non-obvious fact (a constraint, a unit, a side effect, an error case) — 'Return the pathname of the KOS root' earns its line by saying where it reads from. Public-API doc comments conventionally begin with the name (Go); the rule fires on name-only content, not on the convention.

## Do instead

Either delete the line or earn it: one non-obvious fact the name cannot carry — a unit, a side effect, an error case, a constraint with a source. If the only content is the name, the name was the documentation.

## Example

Bad:

```python
def get_proxy_user_name():
    """Get the proxy user name."""
    return 1
```

```console
$ agent-slop check example.py
example.py:2:5 slop/docstring-restates-name — restates the name below (proxy, user, name) — add a fact or delete
```

Good:

```python
def get_proxy_user_name():
    """Return the proxy user name as stored in the session cookie."""
    return 1
```

The good version produces no finding for this rule.

## Match configuration

- `min_shared_identifiers` = `2`
- `min_comment_words` = `2`
- `min_head_words` = `3`

<details>
<summary><code>stopwords</code> (113 entries)</summary>

```text
the
a
an
this
that
these
those
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
you
via
from
into
onto
over
at
get
set
return
returns
returned
check
checks
if
then
else
when
while
new
create
creates
handle
handles
used
uses
using
use
given
one
value
values
list
map
string
number
int
type
py
ts
rs
nix
api
cli
данный
данная
данного
для
и
или
не
на
в
из
с
со
к
по
за
от
до
это
этот
эта
эти
как
что
чтобы
при
после
перед
над
под
без
новый
новая
новое
возвращает
возвращает
создаёт
создает
получает
получить
обрабатывает
используется
используется
для
работы
работа
```

</details>

<details>
<summary><code>ru_glossary</code> (100 entries)</summary>

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
клиент
client
провайдер
provider
эффект
effect
адаптер
adapter
пресет
preset
генерация
generation
изображение
image
видео
video
аудио
audio
```

</details>

## Configure

```toml
[slop.rules]
docstring-restates-name = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/docstring-restates-name -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
