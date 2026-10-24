# `slop/config-deploy-narration`

**Built-in level:** `warning`

## What it catches

A comment narrating deployment or environment mechanics the service cannot see: where a secret comes from ("comes from vault"), how compose resolves an env file (`docker-compose.yml`, "compose file for"), which unit creates the shared database, why this port number ("port clash"), "helm chart", "kustomize overlay", and Russian ("приходит из vault", "репозиторий деплоя"). The deployed artefact has no say in any of it; the reader who needs it reads the deploy repo, not the source.

## Legitimate exception

> A comment that ties ONE named value to its deploy consequence inside the file that owns the value ('API_PORT is baked into the image, so the compose mapping must match'): the mechanics are the contract of this key.

## Do instead

Deployment mechanics belong next to the deployment (compose/helm comments live in the deploy repo) or in a runbook with a referent. In source, keep only what the running code itself enforces.

## Example

Bad:

```python
# The API key comes from vault.
x = 1
```

```console
$ agent-slop check example.py
example.py:1:1 slop/config-deploy-narration — deploy narration 'comes from vault' — mechanics this file cannot enforce
```

Good:

```python
# API_PORT is baked into the image, so the compose mapping must match.
x = 1
```

The good version produces no finding for this rule.

## Match configuration

- `word_boundaries` = `true`

<details>
<summary><code>phrases</code> (50 entries)</summary>

```text
comes from vault
vault side
from the deploy
deploy repo
the deploy pipeline
helm chart
helmfile
kustomize overlay
service dns
chain to the ready
chains to the ready
avoids the port
port clash
port conflict
env_prefix
reads flat env
flat env names
separate from the deploy
write-only for
exports the file
docker compose -f
docker-compose -f
docker-compose.yml
docker-compose.yaml
compose file for
compose file at
ключ приходит
приходит из vault
из vault
отдельно от выката
отдельно от деплоя
репозиторий деплоя
деплой репозиторий
цепляется к готовым
цепляется к готовому
подключается к готовым
избегает порта
конфликта портов
столкновения портов
читает плоские
плоские имена
ходит по dns
по service dns
как api ходит
как api заходит
общий compose
готовую базу
готовые сервисы
создаёт только
создает только
```

</details>

A trailing `*` on a phrase makes it a stem (Russian inflections); matching is always case-insensitive.

## Configure

```toml
[slop.rules]
config-deploy-narration = "off"   # error | warning | hint | off
```

## Suppress

```text
// agent-slop: allow slop/config-deploy-narration -- <reason with a referent: URL, ADR-064, owner/repo#1, path/to/it, date:2026-09-21>
```

Use the comment marker of the file's language (`#`, `//`, `--`, `;`, `/*`). See [Suppressions](../suppressions.md).
