// fooit's sync server. Every log (a synced workspace, or a share's list of workspaces) is a
// Durable Object holding a base and the batches pushed after it. Everything in them is
// encrypted by fooit; the server only checks a token derived from the log's secret, and
// keeps the order of batches.
//
//   POST /v1/logs/:id            start a log            Authorization: Bearer OWNER_TOKEN
//                                {"auth": sha256 of the log's token, "base": base64}
//   GET  /v1/logs/:id?since=N    the batches after N, or the base and all batches after it
//   POST /v1/logs/:id/batches    {"expected": head, "data": base64}, 409 when behind
//   POST /v1/logs/:id/base       {"seq": n, "data": base64}, replacing batches up to n
//
// All but the first need X-Fooit-Auth: the log's token.

import { DurableObject } from "cloudflare:workers";

const MAX_DATA = 8 * 1024 * 1024; // base64 characters in one request
const CHUNK = 1024 * 1024; // a row of Durable Object storage holds at most 2 MB

function json(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function error(status, message) {
  return json({ error: message }, status);
}

async function sha256(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function sameText(a, b) {
  if (typeof a !== "string" || typeof b !== "string" || a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

async function body(request) {
  try {
    const value = await request.json();
    return value && typeof value === "object" ? value : null;
  } catch {
    return null;
  }
}

const isData = (v) => typeof v === "string" && v.length <= MAX_DATA && /^[A-Za-z0-9+/=]*$/.test(v);
const isSeq = (v) => Number.isSafeInteger(v) && v >= 0;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/" && request.method === "GET") {
      return new Response(
        "This is a fooit sync server.\nTo add a share from a link to it, run: fooit share import '<link>'\n",
        { headers: { "content-type": "text/plain; charset=utf-8" } },
      );
    }
    const match = url.pathname.match(/^\/v1\/logs\/([0-9a-f]{32})(\/batches|\/base)?$/);
    if (!match) return error(404, "not found");
    const [, id, action = ""] = match;

    if (action === "" && request.method === "POST") {
      const token = (request.headers.get("authorization") || "").replace(/^Bearer\s+/i, "");
      if (!env.OWNER_TOKEN || !sameText(token, env.OWNER_TOKEN)) return error(401, "bad owner token");
    } else if (!request.headers.get("x-fooit-auth")) {
      return error(403, "no token");
    }

    const log = env.LOGS.get(env.LOGS.idFromName(id));
    return log.fetch(request);
  },
};

export class Log extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.sql = ctx.storage.sql;
    this.sql.exec(`
      CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS base (part INTEGER PRIMARY KEY, data TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS batches (seq INTEGER PRIMARY KEY, data TEXT NOT NULL, time INTEGER NOT NULL);
    `);
  }

  meta(key) {
    const row = this.sql.exec("SELECT value FROM meta WHERE key = ?", key).toArray()[0];
    return row ? row.value : null;
  }

  setMeta(key, value) {
    this.sql.exec("INSERT OR REPLACE INTO meta (key, value) VALUES (?, ?)", key, String(value));
  }

  baseSeq() {
    return Number(this.meta("base_seq") || 0);
  }

  head() {
    const row = this.sql.exec("SELECT max(seq) AS seq FROM batches").toArray()[0];
    return row && row.seq !== null ? Number(row.seq) : this.baseSeq();
  }

  readBase() {
    return this.sql.exec("SELECT data FROM base ORDER BY part").toArray().map((r) => r.data).join("");
  }

  writeBase(data) {
    this.sql.exec("DELETE FROM base");
    for (let i = 0; i * CHUNK < data.length || i === 0; i++) {
      this.sql.exec("INSERT INTO base (part, data) VALUES (?, ?)", i, data.slice(i * CHUNK, (i + 1) * CHUNK));
    }
  }

  async fetch(request) {
    const url = new URL(request.url);
    const action = url.pathname.endsWith("/batches") ? "batches" : url.pathname.endsWith("/base") ? "base" : "";
    const authHash = this.meta("auth");

    if (action === "" && request.method === "POST") {
      if (authHash) return error(409, "log exists");
      const b = await body(request);
      if (!b || !/^[0-9a-f]{64}$/.test(b.auth) || !isData(b.base)) return error(400, "bad request");
      this.ctx.storage.transactionSync(() => {
        this.setMeta("auth", b.auth);
        this.setMeta("base_seq", 0);
        this.writeBase(b.base);
      });
      return json({ head: 0 }, 201);
    }

    if (!authHash) return error(404, "no such log");
    if (!sameText(await sha256(request.headers.get("x-fooit-auth") || ""), authHash)) {
      return error(403, "bad token");
    }

    if (action === "" && request.method === "GET") {
      const baseSeq = this.baseSeq();
      const sinceText = url.searchParams.get("since");
      const since = sinceText === null ? null : Number(sinceText);
      const full = since === null || !isSeq(since) || since < baseSeq;
      const after = full ? baseSeq : since;
      const batches = this.sql
        .exec("SELECT seq, data, time FROM batches WHERE seq > ? ORDER BY seq", after)
        .toArray()
        .map((r) => ({ seq: Number(r.seq), data: r.data, time: Number(r.time) }));
      const oldest = this.sql.exec("SELECT min(time) AS time FROM batches").toArray()[0];
      return json({
        head: this.head(),
        base_seq: baseSeq,
        base: full ? this.readBase() : null,
        batches,
        oldest_age: oldest && oldest.time !== null ? Math.max(0, Date.now() - Number(oldest.time)) : null,
      });
    }

    if (action === "batches" && request.method === "POST") {
      const b = await body(request);
      if (!b || !isSeq(b.expected) || !isData(b.data)) return error(400, "bad request");
      // no await between reading the head and adding to it, so no other request comes between
      const head = this.head();
      if (b.expected !== head) return json({ head }, 409);
      this.sql.exec("INSERT INTO batches (seq, data, time) VALUES (?, ?, ?)", head + 1, b.data, Date.now());
      return json({ head: head + 1 });
    }

    if (action === "base" && request.method === "POST") {
      const b = await body(request);
      if (!b || !isSeq(b.seq) || !isData(b.data)) return error(400, "bad request");
      if (b.seq <= this.baseSeq() || b.seq > this.head()) return json({ base_seq: this.baseSeq() }, 409);
      this.ctx.storage.transactionSync(() => {
        this.writeBase(b.data);
        this.setMeta("base_seq", b.seq);
        this.sql.exec("DELETE FROM batches WHERE seq <= ?", b.seq);
      });
      return json({ base_seq: b.seq });
    }

    return error(405, "method not allowed");
  }
}
