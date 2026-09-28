// Iron Oxide service worker.
//
// Served from /sw.js so its scope is the whole app ("/").
//
// Strategy:
//   * Anything that is not a same-origin GET goes straight to the network and is
//     never cached. That covers every server function / API call (POST, and GET
//     under /api/), auth callbacks and the dev-server hot-reload socket.
//   * Page navigations are network-first. When offline, the app shell cached at
//     install time is served instead. Navigation responses are never written to
//     the cache, so no per-user HTML ends up in it.
//   * Static files under /assets/ (hashed by `dx`, so their content never changes
//     for a given URL) and the files precached below are cache-first.
//
// The page registers this worker as /sw.js?build=<id>, where the id is derived from
// the hashed asset URLs (see src/pwa.rs). Each deploy that changes the wasm, JS or
// CSS therefore installs a fresh worker with its own cache, and the activate step
// deletes the previous build's cache. Bump CACHE_VERSION when only this file or an
// unhashed file in public/ (icons, manifest) changes.

const CACHE_VERSION = "v1";
const BUILD_ID = new URLSearchParams(self.location.search).get("build") || "unknown";
const CACHE_NAME = `iron-oxide-${CACHE_VERSION}-${BUILD_ID}`;
const SHELL_URL = "/";

// Unhashed files from public/ that the app shell needs.
const PRECACHE_URLS = [
  "/manifest.webmanifest",
  "/icons/icon-192.png",
  "/icons/icon-512.png",
  "/icons/icon-maskable-512.png",
  "/icons/apple-touch-icon.png",
  "/icons/icon.svg",
  "/icons/favicon.svg",
  "/favicon.ico",
];

// Paths that must always hit the network, even for GET.
const NETWORK_ONLY_PREFIXES = ["/api/", "/auth/", "/_dioxus"];

// Paths whose URLs are content-hashed by `dx bundle --release`: the wasm, its JS glue and every
// `asset!()` file end up under /assets/ with a hash in the name, so they are safe to serve
// cache-first. (Debug builds serve an unhashed /wasm/ folder instead; the app does not register
// this worker in debug builds.)
const HASHED_PREFIXES = ["/assets/"];

// Whether a response may be stored in the cache. Only a complete, direct, same-origin file
// qualifies:
//   * status 200 exactly: `ok` also covers 206 Partial Content, which `cache.put` rejects;
//   * not redirected: an auth middleware may redirect to a login page;
//   * never HTML: the Dioxus server answers unknown paths, /assets/ included, with the SSR page
//     (200 text/html). During a deploy, a request for a new asset can reach an old server. Caching
//     that page under a JS or wasm URL would break the app until the next deploy, and the page may
//     contain the user's data.
function isCacheable(response) {
  const contentType = response.headers.get("Content-Type") || "";
  return (
    response.status === 200 &&
    !response.redirected &&
    response.type === "basic" &&
    !contentType.toLowerCase().startsWith("text/html")
  );
}

// Fetches `path` and stores it, or throws so that the install fails and is retried on the next
// page load. Replaces `cache.addAll`, which accepts any `ok` response.
async function fetchAndCache(cache, path) {
  const response = await fetch(path, { cache: "no-cache" });
  if (!isCacheable(response)) {
    throw new Error(`not caching ${path}: ${response.status} ${response.headers.get("Content-Type")}`);
  }
  await cache.put(path, response.clone());
  return response;
}

// Fetch the anonymous app shell and precache it together with the hashed
// wasm/js/css it references, so a first offline launch works.
async function precache() {
  const cache = await caches.open(CACHE_NAME);
  await Promise.all(PRECACHE_URLS.map((path) => fetchAndCache(cache, path)));

  // credentials: "omit" makes sure the cached shell never contains a user's data.
  const shellResponse = await fetch(new Request(SHELL_URL, { credentials: "omit", cache: "no-store" }));
  if (shellResponse.status !== 200) {
    throw new Error(`app shell fetch failed: ${shellResponse.status}`);
  }
  const html = await shellResponse.clone().text();
  // Browsers refuse a redirected response as the answer to a navigation, so a shell that redirects
  // (e.g. to a login page) is not stored: offline navigations then fail normally instead.
  if (!shellResponse.redirected) {
    await cache.put(SHELL_URL, shellResponse);
  }

  const assetUrls = hashedAssetPaths(html, /(?:src|href)="([^"]+)"/g);

  // The wasm binary is not referenced by the HTML: the JS glue loads it. Look it up there.
  const wasmUrls = new Set();
  for (const path of [...assetUrls].filter((p) => p.endsWith(".js"))) {
    const js = await (await fetchAndCache(cache, path)).text();
    for (const wasm of hashedAssetPaths(js, /["']([^"']+\.wasm)["']/g)) {
      wasmUrls.add(wasm);
    }
  }
  const remaining = [...assetUrls, ...wasmUrls].filter((p) => !p.endsWith(".js"));
  await Promise.all(remaining.map((path) => fetchAndCache(cache, path)));
}

// Same-origin paths under HASHED_PREFIXES captured by `pattern` (group 1) in `text`.
function hashedAssetPaths(text, pattern) {
  const paths = new Set();
  for (const match of text.matchAll(pattern)) {
    // new URL() also normalises the "/./assets/…" form that dx emits.
    const url = new URL(match[1], self.location.origin);
    if (url.origin === self.location.origin && HASHED_PREFIXES.some((p) => url.pathname.startsWith(p))) {
      paths.add(url.pathname);
    }
  }
  return paths;
}

self.addEventListener("install", (event) => {
  event.waitUntil(precache().then(() => self.skipWaiting()));
});

// Drop every cache that belongs to an older version, then take control of open pages.
self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys.filter((key) => key.startsWith("iron-oxide-") && key !== CACHE_NAME).map((key) => caches.delete(key)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

async function cacheFirst(request) {
  const cached = await caches.match(request);
  if (cached) {
    return cached;
  }
  const response = await fetch(request);
  if (isCacheable(response)) {
    const cache = await caches.open(CACHE_NAME);
    await cache.put(request, response.clone());
  }
  // Anything else is passed through untouched, never cached.
  return response;
}

async function networkFirstNavigation(request) {
  try {
    return await fetch(request);
  } catch (error) {
    const shell = await caches.match(SHELL_URL);
    if (shell) {
      return shell;
    }
    throw error;
  }
}

self.addEventListener("fetch", (event) => {
  const { request } = event;
  const url = new URL(request.url);

  // Non-GET (server functions, form posts) and cross-origin requests: let the browser handle them.
  if (request.method !== "GET" || url.origin !== self.location.origin) {
    return;
  }
  if (NETWORK_ONLY_PREFIXES.some((p) => url.pathname.startsWith(p))) {
    return;
  }
  // Range requests (media elements always send them) expect a 206 slice. Leave them to the browser:
  // a cached full file is not a valid answer, and a 206 cannot be cached anyway.
  if (request.headers.has("Range")) {
    return;
  }
  if (request.mode === "navigate") {
    event.respondWith(networkFirstNavigation(request));
    return;
  }
  if (HASHED_PREFIXES.some((p) => url.pathname.startsWith(p)) || PRECACHE_URLS.includes(url.pathname)) {
    event.respondWith(cacheFirst(request));
  }
  // Everything else: default network behaviour, no caching.
});
