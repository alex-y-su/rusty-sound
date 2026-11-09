# Discovery notes (unofficial api-v2, prototype only)

Status: personal prototype. Uses undocumented endpoints, which the SoundCloud terms do not permit. Do not distribute.

## client_id
- Public, embedded in the web app's JS bundles (`client_id:"…"`), not a secret.
- Found by fetching https://soundcloud.com and grepping the `a-v2.sndcdn.com/assets/*.js` files linked from it.
- Rotates. Extract at startup and re-extract on HTTP 401.

## Endpoints verified (200 OK)
- Search: `GET https://api-v2.soundcloud.com/search/tracks?q=…&limit=…&offset=…&client_id=…`
  - Response: `collection[]`, `total_results`, `next_href`, `query_urn`.
  - Each track has `id`, `title`, `duration` (ms), `permalink_url`, `streamable`, `media.transcodings[]`.
- Resolve: `GET https://api-v2.soundcloud.com/resolve?url=<URL-encoded soundcloud.com URL>&client_id=…`
  - Must URL-encode the target. An unencoded URL returned 404.
- Stream: `GET <transcoding.url>?client_id=…` returns `{"url": "<signed playback URL>"}`.
  - Sample track returned only HLS transcodings (`aac_160k`), so HLS is the path to implement first.
  - Signed URLs expire. Re-request before each playback.

## Not verified yet
- Progressive transcodings (no sample track had one).
- Track ids from search, and expiry time of signed URLs.
- Pagination via `next_href`.
- Stability of client_id across several days.
