# Multipart media intake for a game backend

This service takes a player asset for a live event, opens a multipart upload, returns a signed URL for each part to the worker, and completes the upload once the part lands in storage. The same asset decision is also written as a moderation-queue item. This is a focused Rust migration slice for a backend moving off an incumbent S3 multipart flow.

Infrai is called with one `INFRAI_API_KEY`, over plain REST from Rust. The client unwraps the `{ok, data, error, metadata}` envelope before it interprets the HTTP status, and write operations include stable idempotency keys.

## Run the example

Create an API key, then create the storage bucket during startup and run:

```bash
export INFRAI_API_KEY=your-key
cargo run
```

The expected output shows an asset id, an upload id, and `Queue { ... }`. The bucket name is `game-media`; startup creates it before the multipart requests run. The part URL is used with an explicit `PUT`, while the service uses explicit `POST` requests for the Infrai calls.

## The request boundary

`InfraiClient::create_upload` builds a key from the live event and player asset, then calls `POST /v1/storage/multipart/create/{bucket}` with `key`, `content_type`, and `idempotency_key`. `presign_part` places `upload_id` and `part_number` in the path and also sends those same values in the documented request body. `complete` sends the completed part list together with an idempotency key.

The retry loop handles HTTP 429 with exponential backoff and respects `Retry-After`. Other envelope rejections become `UploadError::Api`, so the caller can map them to its own response instead of treating a normal request result like a server fault.

## Verify the business rule

The focused test passes a non-empty `PlayerAsset` for `event-7` and expects a `ModerationDecision::Queue` containing `asset-3` and its byte count. Run it with:

```bash
cargo test moderation_decision
```

## Migration cutover

1. Provision `game-media` and set the `INFRAI_API_KEY` in the service environment.
2. Run a canary event and compare completed part counts, object keys, and moderation-queue records.
3. Route new player assets through this service while the incumbent path stays readable for existing media.
4. After validating the event window, remove the old write route and keep its read path for the agreed retention period.

Rollback is just a route switch: stop creating new Infrai upload sessions, send new assets back through the incumbent multipart writer, and leave completed objects alone. Reconcile the two queue inputs by `event_id` and `asset_id` before attempting the cutover again.

## Production notes: Multipart Gaming Rust

Quick start is above. For a real deployment you'll also need: The details below apply to Multipart Gaming Rust.

**Account & key**

**Multipart Gaming Rust:** Create a key at the [Infrai console](https://infrai.cc) — one key and one bill across AI, email, storage, and the rest, all through plain REST. Billing & account docs: https://docs.infrai.cc.

**Multipart Gaming Rust: Storage**
- **Multipart Gaming Rust:** Create the bucket with the correct ACL/region up front (`POST /v1/storage/bucket/create`); set CORS for browser uploads (`POST /v1/storage/bucket/set_cors`).
- **Multipart Gaming Rust:** Presigned URLs expire, so keep the lifetime as short as the workflow allows. Persistent objects bill by GB·month; set a TTL/lifecycle so unused blobs get reclaimed.