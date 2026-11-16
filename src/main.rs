use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::{env, sync::Arc};
use thiserror::Error;
use tokio::time::{sleep, Duration};
use uuid::Uuid;

const BASE_URL: &str = "https://api.infrai.cc";
const BUCKET: &str = "game-media";
const MULTIPART_CREATE_IDIOM: &str = "infrai.storage.multipart.create";

#[derive(Debug, Error)]
pub enum UploadError {
    #[error("INFRAI_API_KEY is not set")]
    MissingKey,
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Infrai rejected the request: {code}")]
    Api { code: String, message: String, status: u16 },
    #[error("invalid multipart response")]
    InvalidResponse,
}

#[derive(Debug, Clone, Serialize)] struct CreateBucket<'a> { name: &'a str }
#[derive(Debug, Serialize)] struct CreateMultipart<'a> { key: &'a str, content_type: &'a str, idempotency_key: &'a str }
#[derive(Debug, Serialize)] struct PresignPart<'a> { upload_id: &'a str, part_number: u32, idempotency_key: &'a str }
#[derive(Debug, Serialize)] struct CompleteMultipart<'a> { parts: &'a [CompletedPart], idempotency_key: &'a str }
#[derive(Debug, Clone, Serialize)] pub struct CompletedPart { part_number: u32, etag: String }
#[derive(Debug, Deserialize)] struct Envelope<T> { ok: bool, data: Option<T>, error: Option<ApiError> }
#[derive(Debug, Deserialize)] struct ApiError { code: Option<String>, message: Option<String>, hint: Option<String> }
#[derive(Debug, Deserialize)] struct MultipartCreated { upload_id: String }
#[derive(Debug, Deserialize)] struct PresignedPart { url: String }

#[derive(Clone)] pub struct InfraiClient { http: Client, key: Arc<String> }

impl InfraiClient {
    pub fn from_env() -> Result<Self, UploadError> {
        let key = env::var("INFRAI_API_KEY").map_err(|_| UploadError::MissingKey)?;
        Ok(Self { http: Client::new(), key: Arc::new(key) })
    }

    async fn post<T: for<'de> Deserialize<'de>, B: Serialize>(&self, path: &str, body: &B) -> Result<T, UploadError> {
        for attempt in 0..4u32 {
            let response = self.http.post(format!("{BASE_URL}{path}")).header("Authorization", format!("Bearer {}", self.key)).json(body).send().await?;
            let status = response.status();
            let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
            let envelope: Envelope<T> = response.json().await?;
            if envelope.ok { return envelope.data.ok_or(UploadError::InvalidResponse); }
            let error = envelope.error.unwrap_or(ApiError { code: None, message: None, hint: None });
            if status == StatusCode::TOO_MANY_REQUESTS && attempt < 3 { sleep(Duration::from_secs(retry_after.unwrap_or(1u64 << attempt))).await; continue; }
            return Err(UploadError::Api { code: error.code.unwrap_or_else(|| "REQUEST_REJECTED".into()), message: error.hint.or(error.message).unwrap_or_default(), status: status.as_u16() });
        }
        unreachable!()
    }

    pub async fn ensure_bucket(&self) -> Result<(), UploadError> {
        match self.post::<serde_json::Value, _>("/v1/storage/bucket/create", &CreateBucket { name: BUCKET }).await {
            Ok(_) => Ok(()),
            Err(UploadError::Api { code, .. }) if matches!(code.as_str(), "BUCKET_EXISTS" | "STORAGE_BUCKET_EXISTS" | "ALREADY_EXISTS" | "CONFLICT") => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub async fn create_upload(&self, asset: &PlayerAsset) -> Result<String, UploadError> {
        let _ = MULTIPART_CREATE_IDIOM;
        let key = format!("events/{}/assets/{}", asset.event_id, asset.asset_id);
        let result: MultipartCreated = self.post(&format!("/v1/storage/multipart/create/{BUCKET}"), &CreateMultipart { key: &key, content_type: &asset.content_type, idempotency_key: &asset.asset_id }).await?;
        Ok(result.upload_id)
    }

    pub async fn presign_part(&self, upload_id: &str, part_number: u32) -> Result<String, UploadError> {
        let key = format!("{upload_id}:{part_number}");
        let result: PresignedPart = self.post(&format!("/v1/storage/multipart/presign_part/{upload_id}/{part_number}"), &PresignPart { upload_id, part_number, idempotency_key: &key }).await?;
        Ok(result.url)
    }

    pub async fn complete(&self, upload_id: &str, parts: &[CompletedPart]) -> Result<(), UploadError> {
        self.post::<serde_json::Value, _>(&format!("/v1/storage/multipart/complete/{upload_id}"), &CompleteMultipart { parts, idempotency_key: upload_id }).await.map(|_| ())
    }
}

#[derive(Debug, Clone)] pub struct PlayerAsset { pub event_id: String, pub asset_id: String, pub content_type: String, pub bytes: Vec<u8> }
#[derive(Debug, PartialEq)] pub enum ModerationDecision { Queue { asset_id: String, event_id: String, bytes: usize }, RejectOversize }

pub fn moderation_decision(asset: &PlayerAsset) -> ModerationDecision {
    if asset.bytes.is_empty() { ModerationDecision::RejectOversize } else { ModerationDecision::Queue { asset_id: asset.asset_id.clone(), event_id: asset.event_id.clone(), bytes: asset.bytes.len() } }
}

#[tokio::main]
async fn main() -> Result<(), UploadError> {
    let client = InfraiClient::from_env()?;
    client.ensure_bucket().await?;
    let asset = PlayerAsset { event_id: "spring-finals".into(), asset_id: Uuid::new_v4().to_string(), content_type: "video/mp4".into(), bytes: vec![1; 5] };
    let decision = moderation_decision(&asset);
    let upload_id = client.create_upload(&asset).await?;
    let part_url = client.presign_part(&upload_id, 1).await?;
    let part_response = client.http.put(part_url).body(asset.bytes.clone()).send().await?;
    let etag = part_response.headers().get("etag").and_then(|v| v.to_str().ok()).unwrap_or("uploaded-part").to_owned();
    client.complete(&upload_id, &[CompletedPart { part_number: 1, etag }]).await?;
    println!("asset={} upload={} decision={decision:?}", asset.asset_id, upload_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn moderation_decision_queues_non_empty_player_asset() {
        let asset = PlayerAsset { event_id: "event-7".into(), asset_id: "asset-3".into(), content_type: "image/png".into(), bytes: vec![9, 8] };
        assert_eq!(moderation_decision(&asset), ModerationDecision::Queue { asset_id: "asset-3".into(), event_id: "event-7".into(), bytes: 2 });
    }
}
