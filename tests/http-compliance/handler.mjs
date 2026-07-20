/**
 * Shared compliance-server route handler.
 *
 * This module exports a single `handleRequest(req)` function that implements
 * every route referenced by cases.json.  It is designed to be called from
 * both the Node and Deno iroh-http serve() handler so test logic is never
 * duplicated.
 *
 * Routes:
 *   /status/:code           — return the given status code
 *   /echo                   — echo the request body back
 *   /echo-method             — return the request method as body
 *   /echo-path[/*]           — return the full request path as body
 *   /echo-query              — return the query string (incl. leading ?)
 *   /echo-length             — return the request body byte length as a string
 *   /header/:name            — return the value of the named request header
 *   /header-exists/:name     — return "true"/"false" whether header exists
 *   /header-length/:name     — return the string length of the named header
 *   /header-count            — return the count of x-h-* request headers
 *   /set-header/:name/:value — set a response header with given name/value
 *   /set-headers/:count      — set N response headers (x-h-0 .. x-h-{N-1})
 *   /stream/:bytes           — stream N zero-filled bytes
 *   /typed-json              — return JSON with Content-Type: application/json
 *   /typed-text              — return text with Content-Type: text/plain
 *   /typed-html              — return html with Content-Type: text/html
 *   /typed-binary            — return binary with Content-Type: application/octet-stream
 *   /typed-utf8              — return text with Content-Type: text/plain; charset=utf-8
 *   /error-with-body         — return 500 with a body
 */

/**
 * @param {Request} req
 * @returns {Promise<Response>}
 */
export async function handleRequest(req) {
  const url = new URL(req.url, "http://localhost");
  const path = url.pathname;
  const query = url.search; // includes leading '?'
  const method = req.method;

  // --- /status/:code ---
  const statusMatch = path.match(/^\/status\/(\d+)$/);
  if (statusMatch) {
    const code = parseInt(statusMatch[1], 10);
    // 204 and 304 must not have a body
    if (code === 204 || code === 304) {
      return new Response(null, { status: code });
    }
    return new Response(`status ${code}`, { status: code });
  }

  // --- /echo ---
  if (path === "/echo") {
    const body = await req.text();
    return new Response(body, { status: 200 });
  }

  // --- /echo-method ---
  if (path === "/echo-method") {
    return new Response(method, { status: 200 });
  }

  // --- /hello ---
  // Static response body (not an echo). Directly exercises the #338 regression:
  // Android serve() dropped constant response bodies, so this returns empty
  // there while every other platform returns "hello".
  if (path === "/hello") {
    return new Response("hello", { status: 200 });
  }

  // --- /echo-path[/*] ---
  if (path === "/echo-path" || path.startsWith("/echo-path/")) {
    return new Response(path, { status: 200 });
  }

  // --- /echo-query ---
  if (path === "/echo-query") {
    return new Response(query, { status: 200 });
  }

  // --- /echo-length ---
  if (path === "/echo-length") {
    const buf = await req.arrayBuffer();
    return new Response(String(buf.byteLength), { status: 200 });
  }

  // --- /header/:name ---
  const headerMatch = path.match(/^\/header\/(.+)$/);
  if (headerMatch) {
    const name = decodeURIComponent(headerMatch[1]);
    const value = req.headers.get(name) ?? "";
    return new Response(value, { status: 200 });
  }

  // --- /header-exists/:name ---
  const headerExistsMatch = path.match(/^\/header-exists\/(.+)$/);
  if (headerExistsMatch) {
    const name = decodeURIComponent(headerExistsMatch[1]);
    const exists = req.headers.has(name);
    return new Response(String(exists), { status: 200 });
  }

  // --- /header-length/:name ---
  const headerLengthMatch = path.match(/^\/header-length\/(.+)$/);
  if (headerLengthMatch) {
    const name = decodeURIComponent(headerLengthMatch[1]);
    const value = req.headers.get(name) ?? "";
    return new Response(String(value.length), { status: 200 });
  }

  // --- /header-count ---
  if (path === "/header-count") {
    let count = 0;
    req.headers.forEach((_v, k) => {
      if (k.startsWith("x-h-")) count++;
    });
    return new Response(String(count), { status: 200 });
  }

  // --- /set-header/:name/:value ---
  const setHeaderMatch = path.match(/^\/set-header\/([^/]+)\/(.+)$/);
  if (setHeaderMatch) {
    const name = decodeURIComponent(setHeaderMatch[1]);
    const value = decodeURIComponent(setHeaderMatch[2]);
    return new Response("ok", {
      status: 200,
      headers: { [name]: value },
    });
  }

  // --- /set-headers/:count ---
  const setHeadersMatch = path.match(/^\/set-headers\/(\d+)$/);
  if (setHeadersMatch) {
    const count = parseInt(setHeadersMatch[1], 10);
    const headers = new Headers();
    for (let i = 0; i < count; i++) {
      headers.set(`x-h-${i}`, `v${i}`);
    }
    return new Response("ok", { status: 200, headers });
  }

  // --- /stream/:bytes ---
  const streamMatch = path.match(/^\/stream\/(\d+)$/);
  if (streamMatch) {
    const size = parseInt(streamMatch[1], 10);
    if (size === 0) {
      return new Response(new Uint8Array(0), { status: 200 });
    }
    // Stream in chunks to exercise streaming path
    const chunkSize = Math.min(size, 65536);
    const stream = new ReadableStream({
      start(controller) {
        let remaining = size;
        function push() {
          while (remaining > 0) {
            const n = Math.min(remaining, chunkSize);
            controller.enqueue(new Uint8Array(n)); // zero-filled
            remaining -= n;
          }
          controller.close();
        }
        push();
      },
    });
    return new Response(stream, { status: 200 });
  }

  // --- /typed-json ---
  if (path === "/typed-json") {
    return new Response('{"ok":true}', {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }

  // --- /typed-text ---
  if (path === "/typed-text") {
    return new Response("hello plain", {
      status: 200,
      headers: { "content-type": "text/plain" },
    });
  }

  // --- /typed-html ---
  if (path === "/typed-html") {
    return new Response("<h1>hello</h1>", {
      status: 200,
      headers: { "content-type": "text/html" },
    });
  }

  // --- /typed-binary ---
  if (path === "/typed-binary") {
    return new Response(new Uint8Array([0x00, 0x01, 0x02, 0x03]), {
      status: 200,
      headers: { "content-type": "application/octet-stream" },
    });
  }

  // --- /typed-utf8 ---
  if (path === "/typed-utf8") {
    return new Response("hello utf8", {
      status: 200,
      headers: { "content-type": "text/plain; charset=utf-8" },
    });
  }

  // --- /error-with-body ---
  if (path === "/error-with-body") {
    return new Response("something went wrong", { status: 500 });
  }

  // fallback
  return new Response("not found", { status: 404 });
}
