use std::sync::Arc;

use crate::application::models::Ingest;
use crate::application::ports::{IngestLookup, IngestWriter};

use super::error::{ApiError, ApiResult};

#[derive(Clone)]
pub struct IngestService {
    lookup: Arc<dyn IngestLookup>,
    writer: Arc<dyn IngestWriter>,
}

impl IngestService {
    pub fn with_ports(lookup: Arc<dyn IngestLookup>, writer: Arc<dyn IngestWriter>) -> Self {
        Self { lookup, writer }
    }

    pub async fn list_ingests(&self) -> ApiResult<Vec<Ingest>> {
        self.lookup
            .list_ingests()
            .await
            .map_err(|e| ApiError::internal(format!("list ingests: {e}")))
    }

    pub async fn get_by_id(&self, id: &str) -> ApiResult<Ingest> {
        self.lookup
            .get_ingest(id)
            .await
            .map_err(|e| ApiError::internal(format!("get ingest: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("ingest {id} not found")))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_ingest(
        &self,
        id: &str,
        filename: &str,
        stream_key: &str,
        loop_flag: bool,
        start_time: &str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> ApiResult<Ingest> {
        self.writer
            .create_ingest(
                id,
                filename,
                stream_key,
                loop_flag,
                start_time,
                live_optimized,
                target_gop_seconds,
            )
            .await
            .map_err(|e| ApiError::internal(format!("create ingest: {e}")))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_ingest(
        &self,
        id: &str,
        filename: &str,
        stream_key: &str,
        loop_flag: bool,
        start_time: &str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> ApiResult<Ingest> {
        self.writer
            .update_ingest(
                id,
                filename,
                stream_key,
                loop_flag,
                start_time,
                live_optimized,
                target_gop_seconds,
            )
            .await
            .map_err(|e| ApiError::internal(format!("update ingest: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("ingest {id} not found")))
    }

    pub async fn list_for_filename(&self, filename: &str) -> ApiResult<Vec<Ingest>> {
        self.lookup
            .list_ingests_for_filename(filename)
            .await
            .map_err(|e| ApiError::internal(format!("list ingests for filename: {e}")))
    }

    pub async fn delete_ingest(&self, id: &str) -> ApiResult<bool> {
        self.writer
            .delete_ingest(id)
            .await
            .map_err(|e| ApiError::internal(format!("delete ingest: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::{
        IngestCatalogFuture, IngestDeleteFuture, IngestLookup, IngestLookupFuture,
        IngestUpdateFuture, IngestWriteFuture, IngestWriter,
    };
    use std::sync::Mutex;

    struct FakeIngestStore {
        ingests: Mutex<Vec<Ingest>>,
    }

    impl FakeIngestStore {
        fn new() -> Self {
            Self {
                ingests: Mutex::new(Vec::new()),
            }
        }
    }

    impl IngestLookup for FakeIngestStore {
        fn get_ingest<'a>(&'a self, id: &'a str) -> IngestLookupFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .ingests
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|ingest| ingest.id == id)
                    .cloned())
            })
        }

        fn get_ingest_by_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestLookupFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .ingests
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|ingest| ingest.stream_key == stream_key)
                    .cloned())
            })
        }

        fn list_ingests<'a>(&'a self) -> IngestCatalogFuture<'a> {
            Box::pin(async move { Ok(self.ingests.lock().unwrap().clone()) })
        }

        fn list_ingests_for_filename<'a>(&'a self, filename: &'a str) -> IngestCatalogFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .ingests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|ingest| ingest.filename == filename)
                    .cloned()
                    .collect())
            })
        }

        fn list_ingests_for_stream_key<'a>(
            &'a self,
            stream_key: &'a str,
        ) -> IngestCatalogFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .ingests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|ingest| ingest.stream_key == stream_key)
                    .cloned()
                    .collect())
            })
        }
    }

    impl IngestWriter for FakeIngestStore {
        fn create_ingest<'a>(
            &'a self,
            id: &'a str,
            filename: &'a str,
            stream_key: &'a str,
            loop_flag: bool,
            start_time: &'a str,
            live_optimized: bool,
            target_gop_seconds: u32,
        ) -> IngestWriteFuture<'a> {
            Box::pin(async move {
                let ingest = Ingest {
                    id: id.to_string(),
                    filename: filename.to_string(),
                    stream_key: stream_key.to_string(),
                    loop_flag,
                    start_time: start_time.to_string(),
                    live_optimized,
                    target_gop_seconds,
                };
                self.ingests.lock().unwrap().push(ingest.clone());
                Ok(ingest)
            })
        }

        fn update_ingest<'a>(
            &'a self,
            id: &'a str,
            filename: &'a str,
            stream_key: &'a str,
            loop_flag: bool,
            start_time: &'a str,
            live_optimized: bool,
            target_gop_seconds: u32,
        ) -> IngestUpdateFuture<'a> {
            Box::pin(async move {
                let mut ingests = self.ingests.lock().unwrap();
                let Some(ingest) = ingests.iter_mut().find(|ingest| ingest.id == id) else {
                    return Ok(None);
                };
                ingest.filename = filename.to_string();
                ingest.stream_key = stream_key.to_string();
                ingest.loop_flag = loop_flag;
                ingest.start_time = start_time.to_string();
                ingest.live_optimized = live_optimized;
                ingest.target_gop_seconds = target_gop_seconds;
                Ok(Some(ingest.clone()))
            })
        }

        fn delete_ingest<'a>(&'a self, id: &'a str) -> IngestDeleteFuture<'a> {
            Box::pin(async move {
                let mut ingests = self.ingests.lock().unwrap();
                let before = ingests.len();
                ingests.retain(|ingest| ingest.id != id);
                Ok(ingests.len() != before)
            })
        }
    }

    #[tokio::test]
    async fn ingest_service_uses_injected_ports() {
        let store = Arc::new(FakeIngestStore::new());
        let service = IngestService::with_ports(store.clone(), store);

        let created = service
            .create_ingest("ing-1", "clip.mp4", "stream-key", true, "now", false, 2)
            .await
            .unwrap();
        assert_eq!(created.filename, "clip.mp4");

        let updated = service
            .update_ingest("ing-1", "clip2.mp4", "stream-key", false, "", true, 4)
            .await
            .unwrap();
        assert_eq!(updated.filename, "clip2.mp4");

        let by_filename = service.list_for_filename("clip2.mp4").await.unwrap();
        assert_eq!(by_filename.len(), 1);

        assert!(service.delete_ingest("ing-1").await.unwrap());
        assert!(service.list_ingests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ingest_service_missing_update_is_not_found() {
        let store = Arc::new(FakeIngestStore::new());
        let service = IngestService::with_ports(store.clone(), store);

        let err = service
            .update_ingest("missing", "clip.mp4", "stream-key", false, "", false, 2)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)));
    }
}
