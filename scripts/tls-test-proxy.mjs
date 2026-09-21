#!/usr/bin/env node
// Local TLS test proxy for the manual certificate-pinning checks
// (docs/testing.md, "TLS (self-signed)"): presents self-signed certificates on
// loopback and forwards every request unchanged to a real Jellyfin server, so
// a server with a publicly valid certificate can stand in for a self-signed
// one. Loopback only; logs method, path and status — never headers, queries
// or bodies (the Authorization header carries the token).
//
//   node scripts/tls-test-proxy.mjs https://your.server [certDir]
//
//   https://localhost:8921  starts with cert A   (switchable to B)
//   https://localhost:8923  starts with cert C   (switchable to D) — a second "server"
//   http://127.0.0.1:8922/use?port=8921&cert=B  switch a port's certificate and
//                                               drop its open connections
//   http://127.0.0.1:8922/status                current certificate per port
//
// Missing certificates are generated with OpenSSL (Git for Windows ships one)
// into certDir (default: a folder in the system temp dir), valid for 2 days.

import { execFileSync } from "node:child_process";
import { X509Certificate } from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import https from "node:https";
import os from "node:os";
import path from "node:path";

const [upstreamArg, certDirArg] = process.argv.slice(2);
if (!upstreamArg) {
  console.error("usage: node scripts/tls-test-proxy.mjs https://your.server [certDir]");
  process.exit(2);
}
const upstream = new URL(upstreamArg);
const certDir = certDirArg ?? path.join(os.tmpdir(), "jellysic-tls-test-certs");
fs.mkdirSync(certDir, { recursive: true });

function openssl() {
  const candidates = [
    "C:\\Program Files\\Git\\mingw64\\bin\\openssl.exe",
    "C:\\Program Files\\Git\\usr\\bin\\openssl.exe",
    "openssl",
  ];
  for (const candidate of candidates) {
    try {
      execFileSync(candidate, ["version"], { stdio: "ignore" });
      return candidate;
    } catch {
      // try the next one
    }
  }
  throw new Error("OpenSSL not found — install Git for Windows or put openssl on PATH");
}

const names = ["A", "B", "C", "D"];
let bin = null;
for (const name of names) {
  const key = path.join(certDir, `${name}.key`);
  const crt = path.join(certDir, `${name}.crt`);
  if (fs.existsSync(key) && fs.existsSync(crt)) continue;
  bin ??= openssl();
  execFileSync(bin, [
    "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes",
    "-keyout", key, "-out", crt, "-days", "2",
    "-subj", `/CN=localhost/O=jellysic test ${name}`,
    "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1",
  ], { stdio: "ignore" });
}

const load = (name) => ({
  key: fs.readFileSync(path.join(certDir, `${name}.key`)),
  cert: fs.readFileSync(path.join(certDir, `${name}.crt`)),
});
const fingerprint = (name) => new X509Certificate(load(name).cert).fingerprint256;

const client = upstream.protocol === "https:" ? https : http;
const log = (line) => console.log(`${new Date().toISOString().slice(11, 19)} ${line}`);
const ports = { 8921: { cert: "A" }, 8923: { cert: "C" } };

function forward(port) {
  return (req, res) => {
    const up = client.request(
      {
        protocol: upstream.protocol,
        hostname: upstream.hostname,
        port: upstream.port || (upstream.protocol === "https:" ? 443 : 80),
        method: req.method,
        path: req.url,
        headers: { ...req.headers, host: upstream.host },
      },
      (upRes) => {
        const pathname = req.url.split("?")[0];
        // Streams and cover images are noise; log them only when they fail.
        if (!/^\/(Audio|Items\/[^/]+\/Images)\//.test(pathname) || upRes.statusCode >= 400) {
          log(`:${port} ${req.method} ${pathname} -> ${upRes.statusCode}`);
        }
        res.writeHead(upRes.statusCode, upRes.headers);
        upRes.pipe(res);
      },
    );
    up.on("error", (e) => {
      log(`:${port} upstream error ${e.code ?? e.message}`);
      if (!res.headersSent) res.writeHead(502);
      res.end();
    });
    req.pipe(up);
  };
}

for (const [port, entry] of Object.entries(ports)) {
  entry.servers = [];
  entry.sockets = new Set();
  for (const host of ["127.0.0.1", "::1"]) {
    const server = https.createServer({ ...load(entry.cert), minVersion: "TLSv1.2" }, forward(port));
    server.on("secureConnection", (socket) => {
      entry.sockets.add(socket);
      socket.on("close", () => entry.sockets.delete(socket));
    });
    server.on("tlsClientError", (e) => log(`:${port} TLS handshake failed (${e.code ?? e.message})`));
    server.listen(+port, host);
    entry.servers.push(server);
  }
  log(`https://localhost:${port} -> ${upstream.origin}, cert ${entry.cert}`);
}
for (const name of names) log(`cert ${name}: ${fingerprint(name)}`);

http
  .createServer((req, res) => {
    const url = new URL(req.url, "http://control");
    if (url.pathname === "/use") {
      const port = url.searchParams.get("port");
      const entry = ports[port];
      const name = url.searchParams.get("cert");
      if (!entry || !names.includes(name ?? "")) {
        res.writeHead(400).end("bad request");
        return;
      }
      for (const server of entry.servers) server.setSecureContext({ ...load(name), minVersion: "TLSv1.2" });
      const dropped = entry.sockets.size;
      for (const socket of entry.sockets) socket.destroy();
      entry.cert = name;
      log(`port ${port} now presents cert ${name} (dropped ${dropped} open connections)`);
      res.end(`ok ${name}`);
    } else {
      res.end(JSON.stringify(Object.fromEntries(Object.entries(ports).map(([p, e]) => [p, e.cert]))));
    }
  })
  .listen(8922, "127.0.0.1");
