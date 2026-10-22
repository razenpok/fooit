# fooit sync server

A Cloudflare Worker that fooit syncs through. Each synced workspace, and each share's
list of workspaces, is a log kept in its own Durable Object (with SQLite storage): a
base and the batches of changes after it. fooit encrypts everything before sending
it, so the Worker only sees log ids, sizes and times. It runs on the Workers free plan.

## Deploy

```
cd worker
npm install
npx wrangler login
npx wrangler secret put OWNER_TOKEN    # any long random string, e.g. from `openssl rand -hex 32`
npx wrangler deploy
```

`deploy` prints the Worker's address, like `https://fooit-sync.you.workers.dev`. Put
it and the token in fooit's `config.toml` (see the main README), or in
`FOOIT_SYNC_URL` and `FOOIT_SYNC_TOKEN`.

The owner token is what lets a machine start new logs, so only you can fill the
server. Reading and changing a log needs the log's own token, which comes from the
secret in its share link.

## Run locally

```
echo 'OWNER_TOKEN="dev-token"' > .dev.vars
npx wrangler dev
```

## API

| request | |
| --- | --- |
| `POST /v1/logs/:id` | start a log: `{"auth", "base"}`, with `Authorization: Bearer OWNER_TOKEN` |
| `GET /v1/logs/:id?since=N` | the batches after N, or the base and all batches when N is missing or older than the base |
| `POST /v1/logs/:id/batches` | `{"expected", "data"}`: adds a batch if `expected` is still the newest sequence number, else 409 |
| `POST /v1/logs/:id/base` | `{"seq", "data"}`: replaces the base with the state at `seq` and drops the batches up to it |

All but the first need `X-Fooit-Auth`, whose SHA-256 must match the `auth` the log
was started with.
