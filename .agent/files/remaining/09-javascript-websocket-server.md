# Packet C2: package-owned JavaScript HTTP/WebSocket server

## Goal

Expose CDP from Node without the native Obscura CLI, a native addon or a native
WebSocket dependency. The transport uses Node built-ins and JavaScript code in
the npm package.

## Dependencies

- C1 command/event/action envelope is frozen.

## Small tasks

### C2.1. HTTP discovery endpoints

Implement bounded responses for:

- `/json/version`
- `/json/list` and `/json`
- target create/close/activate endpoints if required by clients

Bind loopback by default. Port `0` must report the actual selected port in all
returned WebSocket URLs. Host/header-derived public URLs must be validated or
configured explicitly.

### C2.2. RFC 6455 upgrade

- Validate method, Upgrade/Connection headers, version 13 and a valid key.
- Compute `Sec-WebSocket-Accept` with Node crypto.
- Reject invalid paths/targets before switching protocols.
- Bound header bytes and handshake time.

### C2.3. Frame parser/writer

- Require masked client frames and unmasked server frames.
- Support text, continuation, ping, pong and close.
- Reject binary CDP requests unless intentionally supported.
- Validate RSV/opcode/UTF-8/control-frame/fragmentation rules.
- Enforce per-frame, per-message and queued-output limits before allocation.
- Handle partial/multiple frames in arbitrary TCP chunks.

### C2.4. Backpressure and connection lifecycle

- Respect `socket.write()` backpressure.
- Bound queued events per connection.
- Serialize response/event writes without reordering.
- Ping/idle deadline and graceful close handshake.
- Abrupt disconnect cancels C1 connection actions and frees targets/streams.
- Server shutdown stops accepting, closes clients, then terminates Workers.

### C2.5. Security and robustness tests

- Bad upgrade headers/key/version/path.
- Unmasked, oversize, fragmented and invalid UTF-8 frames.
- Ping/pong interleaving and close codes.
- Slowloris handshake and output-backpressure client.
- Multiple clients and explicit flattened sessions.
- Fuzz frame boundaries using deterministic generated chunks.

### C2.6. Real CDP smoke

Connect a minimal standards-compliant WebSocket client and complete:

- Browser version
- target discovery/create/attach
- Runtime evaluation
- Page navigation
- screenshot and PDF
- target/connection close

## Acceptance criteria

- `npm install` compiles nothing.
- The server contains no `ws`, native addon or native Obscura executable
  requirement unless the project explicitly chooses a pure-JS dependency
  later. The initial plan assumes Node built-ins only.
- Port 0 and graceful shutdown are deterministic.

## Copyable LLM prompt

```text
Implement one C2 task from
.agent/files/remaining/09-javascript-websocket-server.md. Use Node 22 built-ins
only and follow RFC 6455 strictly. The server transports C1 data; it must not
duplicate browser/CDP state. State exact byte/queue/deadline limits before
editing. Add fragmented-input and cleanup tests. Do not touch docs/README/
.agent/index.js or commit unless told.
```
