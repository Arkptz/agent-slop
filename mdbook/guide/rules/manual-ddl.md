# `slop/manual-ddl`

**Built-in level:** `warning`

## What it catches

A schema change left as a TODO or "run manually" note in the source. The note is invisible to the deploy, so the column or table it describes exists only where someone remembered to run it, and the comment outlives the migration by months. It fires when the comment block contains both a human-step cue (a `TODO`/`FIXME`/`XXX`/`HACK` marker, or "manually", "manual migration", "вручную") *and* a DDL statement (`alter table`, `create table`, `drop column`, `create index`, …). The cue and the statement may sit on different lines of one paragraph.

## Legitimate exception

> A comment explaining SQL a tool or library runs itself ('the migrator does CREATE TABLE IF NOT EXISTS first'), not a step someone has to perform.

## Do instead

Put the schema change where the deploy runs it — a migration file, or the schema generator when the table is new — and describe the rollout in the PR or runbook, not in the model.

## Example

Bad:

```python
# TODO: manually run ALTER TABLE users ADD COLUMN active;
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/manual-ddl — 'ALTER TABLE' left for a human — the deploy never runs it
```

Good:

```python
# The migrator runs CREATE TABLE IF NOT EXISTS before this query.
x = 1
```

The good version produces no finding for this rule.

## Match configuration

<details>
<summary><code>markers</code> (4 entries)</summary>

```text
TODO
FIXME
XXX
HACK
```

</details>

<details>
<summary><code>phrases</code> (6 entries)</summary>

```text
вручную
ручная миграция
ручной миграции
manual migration
manually
run by hand
```

</details>

<details>
<summary><code>ddl</code> (9 entries)</summary>

```text
alter table
rename table
create table
drop table
add column
drop column
modify column
create index
drop index
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
manual-ddl = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/manual-ddl -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
