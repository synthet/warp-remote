// Caches the app shell so the PWA opens instantly and offline. Session data and the API are
// always fetched live: a stale terminal list would be worse than none.
const CACHE = "synth-remote-v1";
const SHELL = [
  "/",
  "/assets/app.js",
  "/assets/app.css",
  "/assets/icon.svg",
  "/assets/vendor/xterm.js",
  "/assets/vendor/xterm.css",
  "/assets/vendor/addon-fit.js",
];

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(SHELL)));
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys().then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))),
  );
  self.clients.claim();
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (event.request.method !== "GET" || url.origin !== location.origin) return;
  if (url.pathname.startsWith("/api/") || url.pathname.startsWith("/sessions/")) return;
  const shellPath = url.pathname.startsWith("/session/") ? "/" : url.pathname;
  // Network first, so upgrades land immediately; fall back to the cached shell offline.
  event.respondWith(
    fetch(event.request)
      .then((response) => {
        if (response.ok && SHELL.includes(shellPath)) {
          const copy = response.clone();
          caches.open(CACHE).then((cache) => cache.put(shellPath, copy));
        }
        return response;
      })
      .catch(() => caches.match(shellPath)),
  );
});
