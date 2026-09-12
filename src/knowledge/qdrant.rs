//! Qdrant vector-store adapter.
//!
//! Qdrant owns vector indexes and persistence. This module only translates the
//! [`KnowledgeStore`](crate::knowledge::KnowledgeStore) vector operations into
//! Qdrant's HTTP API; graph operations stay in [`super::falkor`].

use std::fmt::Write;
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use crate::knowledge::{ChunkFilter, KnowledgeError, ModelId, ScoredHit, VectorSpace};

const CHUNK_PREFIX: &str = "ohara_chunks_";
const ENTITY_COLLECTION: &str = "ohara_entity_names";

#[derive(Debug, Deserialize)]
struct CollectionList {
    collections: Vec<CollectionInfo>,
}

#[derive(Debug, Deserialize)]
struct CollectionInfo {
    name: String,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    result: Vec<SearchPoint>,
}

#[derive(Debug, Deserialize)]
struct SearchPoint {
    id: serde_json::Value,
    score: f32,
    payload: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Clone)]
pub(super) struct QdrantStore {
    client: Client,
    base_url: Url,
    dim: usize,
    timeout: Duration,
    read_only: bool,
}

impl QdrantStore {
    pub(super) fn new(
        base_url: Url,
        dim: usize,
        timeout: Duration,
        read_only: bool,
    ) -> Result<Self, KnowledgeError> {
        let client = Client::builder()
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .map_err(|error| KnowledgeError::Backend(format!("create Qdrant client: {error}")))?;
        Ok(Self {
            client,
            base_url,
            dim,
            timeout,
            read_only,
        })
    }

    pub(super) async fn health(&self) -> Result<(), KnowledgeError> {
        let response = self
            .client
            .get(self.url(""))
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|error| unavailable("Qdrant health check", error))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(response_error("Qdrant health check", response).await)
        }
    }

    pub(super) async fn upsert(
        &self,
        space: &VectorSpace,
        doc_id: &str,
        ids: &[&str],
        vectors: &[Vec<f32>],
    ) -> Result<(), KnowledgeError> {
        self.require_writable()?;
        if ids.len() != vectors.len() {
            return Err(KnowledgeError::Backend(format!(
                "upsert_vectors: {} ids but {} vectors",
                ids.len(),
                vectors.len()
            )));
        }
        for vector in vectors {
            if vector.len() != self.dim {
                return Err(KnowledgeError::Backend(format!(
                    "upsert_vectors: vector width {} != collection dim {}",
                    vector.len(),
                    self.dim
                )));
            }
        }
        if ids.is_empty() {
            return Ok(());
        }

        let collection = Self::collection(space);
        self.ensure_collection(&collection).await?;
        let points: Vec<serde_json::Value> = ids
            .iter()
            .zip(vectors)
            .map(|(id, vector)| {
                json!({
                    "id": point_id(id),
                    "vector": vector,
                    "payload": {
                        "ohara_id": id,
                        "doc_id": doc_id,
                    }
                })
            })
            .collect();
        let response = self
            .client
            .put(self.url(&format!("collections/{collection}/points")))
            .query(&[("wait", "true")])
            .json(&json!({ "points": points }))
            .send()
            .await
            .map_err(|error| unavailable("Qdrant vector upsert", error))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(response_error("Qdrant vector upsert", response).await)
        }
    }

    pub(super) async fn knn(
        &self,
        space: &VectorSpace,
        query: &[f32],
        limit: usize,
        _filter: &ChunkFilter,
    ) -> Result<Vec<ScoredHit>, KnowledgeError> {
        if query.len() != self.dim {
            return Err(KnowledgeError::Backend(format!(
                "knn: query width {} != collection dim {}",
                query.len(),
                self.dim
            )));
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let collection = Self::collection(space);
        let response = self
            .client
            .post(self.url(&format!("collections/{collection}/points/search")))
            .json(&json!({
                "vector": query,
                "limit": limit,
                "with_payload": true,
            }))
            .send()
            .await
            .map_err(|error| unavailable("Qdrant vector search", error))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !response.status().is_success() {
            return Err(response_error("Qdrant vector search", response).await);
        }
        let body: SearchResponse = response.json().await.map_err(|error| {
            KnowledgeError::Backend(format!("decode Qdrant search response: {error}"))
        })?;
        body.result
            .into_iter()
            .map(|point| {
                let id = point
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("ohara_id"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| point.id.as_str().map(str::to_owned))
                    .ok_or_else(|| {
                        KnowledgeError::Backend(
                            "Qdrant search point has no string id payload".into(),
                        )
                    })?;
                Ok(ScoredHit {
                    id,
                    score: point.score,
                })
            })
            .collect()
    }

    pub(super) async fn has_vector(
        &self,
        space: &VectorSpace,
        id: &str,
    ) -> Result<bool, KnowledgeError> {
        let collection = Self::collection(space);
        let response = self
            .client
            .get(self.url(&format!("collections/{collection}/points/{}", point_id(id))))
            .send()
            .await
            .map_err(|error| unavailable("Qdrant vector lookup", error))?;
        if response.status() == StatusCode::NOT_FOUND {
            Ok(false)
        } else if response.status().is_success() {
            Ok(true)
        } else {
            Err(response_error("Qdrant vector lookup", response).await)
        }
    }

    pub(super) async fn delete_document(&self, doc_id: &str) -> Result<(), KnowledgeError> {
        self.require_writable()?;
        let collections = self.collections().await?;
        for collection in collections
            .into_iter()
            .filter(|name| name.starts_with(CHUNK_PREFIX))
        {
            let response = self
                .client
                .post(self.url(&format!("collections/{collection}/points/delete")))
                .query(&[("wait", "true")])
                .json(&json!({
                    "filter": {
                        "must": [{ "key": "doc_id", "match": { "value": doc_id } }]
                    }
                }))
                .send()
                .await
                .map_err(|error| unavailable("Qdrant document deletion", error))?;
            if !response.status().is_success() {
                return Err(response_error("Qdrant document deletion", response).await);
            }
        }
        Ok(())
    }

    pub(super) async fn delete_entity(&self, entity_id: &str) -> Result<(), KnowledgeError> {
        self.require_writable()?;
        let collection = ENTITY_COLLECTION;
        let response = self
            .client
            .post(self.url(&format!("collections/{collection}/points/delete")))
            .query(&[("wait", "true")])
            .json(&json!({ "points": [point_id(entity_id)] }))
            .send()
            .await
            .map_err(|error| unavailable("Qdrant entity deletion", error))?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(response_error("Qdrant entity deletion", response).await)
        }
    }

    fn require_writable(&self) -> Result<(), KnowledgeError> {
        if self.read_only {
            Err(KnowledgeError::Unavailable(
                "knowledge store is read-only in this process".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn collection(space: &VectorSpace) -> String {
        match space {
            VectorSpace::Chunks { model_id } => format!("{CHUNK_PREFIX}{}", hex_digest(model_id)),
            VectorSpace::EntityNames => ENTITY_COLLECTION.to_string(),
        }
    }

    async fn ensure_collection(&self, collection: &str) -> Result<(), KnowledgeError> {
        let lookup = self
            .client
            .get(self.url(&format!("collections/{collection}")))
            .send()
            .await
            .map_err(|error| unavailable("Qdrant collection lookup", error))?;
        if lookup.status().is_success() {
            return Ok(());
        }
        if lookup.status() != StatusCode::NOT_FOUND {
            return Err(response_error("Qdrant collection lookup", lookup).await);
        }
        let response = self
            .client
            .put(self.url(&format!("collections/{collection}")))
            .json(&json!({
                "vectors": { "size": self.dim, "distance": "Cosine" }
            }))
            .send()
            .await
            .map_err(|error| unavailable("Qdrant collection creation", error))?;
        if response.status().is_success() || response.status() == StatusCode::CONFLICT {
            Ok(())
        } else {
            Err(response_error("Qdrant collection creation", response).await)
        }
    }

    async fn collections(&self) -> Result<Vec<String>, KnowledgeError> {
        let response = self
            .client
            .get(self.url("collections"))
            .send()
            .await
            .map_err(|error| unavailable("Qdrant collection listing", error))?;
        if !response.status().is_success() {
            return Err(response_error("Qdrant collection listing", response).await);
        }
        let body: CollectionList = response.json().await.map_err(|error| {
            KnowledgeError::Backend(format!("decode Qdrant collection list: {error}"))
        })?;
        Ok(body.collections.into_iter().map(|item| item.name).collect())
    }

    fn url(&self, path: &str) -> Url {
        self.base_url
            .join(path)
            .unwrap_or_else(|_| self.base_url.clone())
    }
}

fn hex_digest(model_id: &ModelId) -> String {
    let digest = Sha256::digest(model_id.as_str().as_bytes());
    let mut result = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(result, "{byte:02x}");
    }
    result
}

fn point_id(id: &str) -> String {
    let mut bytes: [u8; 32] = Sha256::digest(id.as_bytes()).into();
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut uuid_bytes = [0_u8; 16];
    uuid_bytes.copy_from_slice(&bytes[..16]);
    Uuid::from_bytes(uuid_bytes).to_string()
}

fn unavailable(context: &str, error: impl std::fmt::Display) -> KnowledgeError {
    KnowledgeError::Unavailable(format!("{context}: {error}"))
}

async fn response_error(context: &str, response: reqwest::Response) -> KnowledgeError {
    let status = response.status();
    let detail = response
        .text()
        .await
        .unwrap_or_else(|_| "response body unavailable".to_string());
    KnowledgeError::Backend(format!("{context} returned {status}: {detail}"))
}
