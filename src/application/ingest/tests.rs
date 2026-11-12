use super::*;
use crate::application::ports::{
    IngestCatalogFuture, IngestDeleteFuture, IngestLookupFuture, IngestUpdateFuture,
    IngestWriteFuture, PipelineLookupFuture,
};
use std::collections::HashMap;
use std::sync::Arc;

struct FakePipelineStore {
    pipelines: HashMap<String, Pipeline>,
    error: Option<&'static str>,
}

impl PipelineStore for FakePipelineStore {
    fn get_pipeline_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineLookupFuture<'a> {
        Box::pin(async move {
            if let Some(message) = self.error {
                return Err(PipelineStoreError::new(message));
            }
            Ok(self.pipelines.get(stream_key).cloned())
        })
    }

    fn list_pipelines<'a>(&'a self) -> crate::application::ports::PipelineListFuture<'a> {
        Box::pin(async move { Ok(self.pipelines.values().cloned().collect()) })
    }

    fn get_pipeline<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::application::ports::PipelineLookupFuture<'a> {
        Box::pin(async move { Ok(self.pipelines.values().find(|p| p.id == id).cloned()) })
    }

    fn create_pipeline<'a>(
        &'a self,
        _id: &'a str,
        _name: &'a str,
        _stream_key: &'a str,
        _input_source: Option<&'a str>,
        _srt_ingest_policy: Option<&'a str>,
    ) -> crate::application::ports::PipelineCreateFuture<'a> {
        Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
    }

    fn update_pipeline<'a>(
        &'a self,
        _id: &'a str,
        _name: &'a str,
        _stream_key: &'a str,
        _input_source: Option<&'a str>,
        _srt_ingest_policy: Option<&'a str>,
    ) -> crate::application::ports::PipelineUpdateFuture<'a> {
        Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
    }

    fn delete_pipeline<'a>(
        &'a self,
        _id: &'a str,
    ) -> crate::application::ports::PipelineDeleteFuture<'a> {
        Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
    }

    fn get_ingest_host<'a>(&'a self) -> crate::application::ports::PipelineIngestHostFuture<'a> {
        Box::pin(async move { Ok(None) })
    }

    fn update_pipeline_input_source<'a>(
        &'a self,
        pipeline: &'a Pipeline,
        input_source: Option<&'a str>,
    ) -> crate::application::ports::PipelineUpdateFuture<'a> {
        Box::pin(async move {
            let mut updated = pipeline.clone();
            updated.input_source = input_source.map(ToOwned::to_owned);
            Ok(Some(updated))
        })
    }
}

struct FakeIngestLookup {
    by_id: HashMap<String, Ingest>,
    by_stream_key: HashMap<String, Vec<Ingest>>,
    error: Option<&'static str>,
}

struct FakeIngestWriter {
    created: std::sync::Mutex<Vec<Ingest>>,
    deleted: std::sync::Mutex<Vec<String>>,
    fail: Option<&'static str>,
    update_returns_none: bool,
}

impl IngestWriter for FakeIngestWriter {
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
            if let Some(message) = self.fail {
                return Err(IngestWriteError::new(message));
            }
            let ingest = Ingest {
                id: id.to_string(),
                filename: filename.to_string(),
                stream_key: stream_key.to_string(),
                loop_flag,
                start_time: start_time.to_string(),
                live_optimized,
                target_gop_seconds,
                playlist: Vec::new(),
            };
            restream_sync::lock::lock(&self.created).push(ingest.clone());
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
            if self.update_returns_none {
                return Ok(None);
            }
            self.create_ingest(
                id,
                filename,
                stream_key,
                loop_flag,
                start_time,
                live_optimized,
                target_gop_seconds,
            )
            .await
            .map(Some)
        })
    }

    fn update_ingest_filename<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move {
            if self.update_returns_none {
                return Ok(None);
            }
            if let Some(message) = self.fail {
                return Err(IngestWriteError::new(message));
            }
            self.create_ingest(id, filename, "", false, "", false, 0)
                .await
                .map(Some)
        })
    }

    fn set_ingest_playlist<'a>(
        &'a self,
        _id: &'a str,
        _playlist: &'a [String],
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move { Ok(None) })
    }

    fn rename_ingest_file<'a>(
        &'a self,
        _id: &'a str,
        _from: &'a str,
        _to: &'a str,
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move { Ok(None) })
    }

    fn delete_ingest<'a>(&'a self, id: &'a str) -> IngestDeleteFuture<'a> {
        Box::pin(async move {
            if let Some(message) = self.fail {
                return Err(IngestWriteError::new(message));
            }
            restream_sync::lock::lock(&self.deleted).push(id.to_string());
            Ok(true)
        })
    }
}

impl FakeIngestLookup {
    fn ingest(id: &str, stream_key: &str) -> Ingest {
        Ingest {
            id: id.to_string(),
            filename: "clip.mp4".to_string(),
            stream_key: stream_key.to_string(),
            loop_flag: true,
            start_time: "00:00:05".to_string(),
            live_optimized: true,
            target_gop_seconds: 4,
            playlist: Vec::new(),
        }
    }
}

impl IngestLookup for FakeIngestLookup {
    fn get_ingest<'a>(&'a self, id: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move {
            if let Some(message) = self.error {
                return Err(IngestLookupError::new(message));
            }
            Ok(self.by_id.get(id).cloned())
        })
    }

    fn get_ingest_by_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move {
            if let Some(message) = self.error {
                return Err(IngestLookupError::new(message));
            }
            Ok(self
                .by_stream_key
                .get(stream_key)
                .and_then(|ingests| ingests.last().cloned()))
        })
    }

    fn list_ingests<'a>(&'a self) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            if let Some(message) = self.error {
                return Err(IngestLookupError::new(message));
            }
            Ok(self.by_id.values().cloned().collect())
        })
    }

    fn list_ingests_for_filename<'a>(&'a self, filename: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            if let Some(message) = self.error {
                return Err(IngestLookupError::new(message));
            }
            Ok(self
                .by_id
                .values()
                .filter(|ingest| ingest.filename == filename)
                .cloned()
                .collect())
        })
    }

    fn list_ingests_for_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            if let Some(message) = self.error {
                return Err(IngestLookupError::new(message));
            }
            Ok(self
                .by_stream_key
                .get(stream_key)
                .cloned()
                .unwrap_or_default())
        })
    }
}

#[tokio::test]
async fn resolve_file_ingest_context_returns_none_for_missing_ingest() {
    let ingest_lookup = FakeIngestLookup {
        by_id: HashMap::new(),
        by_stream_key: HashMap::new(),
        error: None,
    };
    let pipeline_lookup = FakePipelineStore {
        pipelines: HashMap::new(),
        error: None,
    };

    let result = resolve_file_ingest_context(&ingest_lookup, &pipeline_lookup, "missing")
        .await
        .unwrap();

    assert!(result.is_none());
}

#[tokio::test]
async fn resolve_file_ingest_context_surfaces_missing_pipeline() {
    let ingest = FakeIngestLookup::ingest("ingest-1", "stream-key");
    let ingest_lookup = FakeIngestLookup {
        by_id: HashMap::from([(ingest.id.clone(), ingest.clone())]),
        by_stream_key: HashMap::from([("stream-key".to_string(), vec![ingest])]),
        error: None,
    };
    let pipeline_lookup = FakePipelineStore {
        pipelines: HashMap::new(),
        error: None,
    };

    let result = resolve_file_ingest_context(&ingest_lookup, &pipeline_lookup, "ingest-1").await;

    assert!(matches!(
        result,
        Err(ResolveFileIngestError::MissingPipelineForStreamKey(stream_key))
            if stream_key == "stream-key"
    ));
}

#[tokio::test]
async fn resolve_file_ingest_context_returns_ingest_and_pipeline() {
    let ingest = FakeIngestLookup::ingest("ingest-1", "stream-key");
    let pipeline = Pipeline {
        id: "pipeline-1".to_string(),
        name: "Pipeline".to_string(),
        stream_key: "stream-key".to_string(),
        input_source: None,
        srt_ingest_policy: None,
    };
    let ingest_lookup = FakeIngestLookup {
        by_id: HashMap::from([(ingest.id.clone(), ingest.clone())]),
        by_stream_key: HashMap::from([("stream-key".to_string(), vec![ingest.clone()])]),
        error: None,
    };
    let pipeline_lookup = FakePipelineStore {
        pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
        error: None,
    };

    let result = resolve_file_ingest_context(&ingest_lookup, &pipeline_lookup, "ingest-1")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(result.ingest.id, "ingest-1");
    assert_eq!(result.pipeline.id, "pipeline-1");
}

#[tokio::test]
async fn load_pipeline_file_ingest_state_returns_latest_ingest_and_running_flag() {
    let ingest = FakeIngestLookup::ingest("ingest-1", "stream-key");
    let lookup = FakeIngestLookup {
        by_id: HashMap::new(),
        by_stream_key: HashMap::from([("stream-key".to_string(), vec![ingest.clone()])]),
        error: None,
    };
    let engine = MediaEngine::new();
    engine.mark_file_ingest_running(&ingest.id).await;
    let pipeline = Pipeline {
        id: "pipeline-1".to_string(),
        name: "Pipeline".to_string(),
        stream_key: "stream-key".to_string(),
        input_source: None,
        srt_ingest_policy: None,
    };

    let state = load_pipeline_file_ingest_state(&lookup, &engine, &pipeline)
        .await
        .unwrap();

    assert_eq!(state.ingest.unwrap().id, "ingest-1");
    assert!(state.running);
}

#[tokio::test]
async fn clear_stream_key_file_ingests_unregisters_pipeline_and_clears_running_state() {
    let ingest = FakeIngestLookup::ingest("ingest-1", "stream-key");
    let pipeline = Pipeline {
        id: "pipeline-1".to_string(),
        name: "Pipeline".to_string(),
        stream_key: "stream-key".to_string(),
        input_source: None,
        srt_ingest_policy: None,
    };
    let ingest_lookup = FakeIngestLookup {
        by_id: HashMap::new(),
        by_stream_key: HashMap::from([("stream-key".to_string(), vec![ingest.clone()])]),
        error: None,
    };
    let pipeline_lookup = FakePipelineStore {
        pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
        error: None,
    };
    let engine = Arc::new(MediaEngine::new());
    let _registration = engine
        .try_register_ingest(&pipeline.id, &pipeline.stream_key, "file")
        .await
        .expect("pipeline should register");
    engine.mark_file_ingest_running(&ingest.id).await;

    clear_stream_key_file_ingests(
        &pipeline_lookup,
        &ingest_lookup,
        &engine,
        &pipeline.stream_key,
    )
    .await
    .unwrap();

    assert!(!engine.has_active_ingest(&pipeline.id).await);
    assert!(!engine.is_file_ingest_running(&ingest.id).await);
}

#[tokio::test]
async fn persist_pipeline_file_ingest_updates_pipeline_and_deletes_stale_ingests() {
    let current = FakeIngestLookup::ingest("ingest-current", "stream-key");
    let stale = FakeIngestLookup::ingest("ingest-stale", "stream-key");
    let pipeline = Pipeline {
        id: "pipeline-1".to_string(),
        name: "Pipeline".to_string(),
        stream_key: "stream-key".to_string(),
        input_source: None,
        srt_ingest_policy: None,
    };
    let ingest_lookup = FakeIngestLookup {
        by_id: HashMap::new(),
        by_stream_key: HashMap::from([(
            "stream-key".to_string(),
            vec![stale.clone(), current.clone()],
        )]),
        error: None,
    };
    let pipeline_store = FakePipelineStore {
        pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
        error: None,
    };
    let ingest_writer = FakeIngestWriter {
        created: std::sync::Mutex::new(Vec::new()),
        deleted: std::sync::Mutex::new(Vec::new()),
        fail: None,
        update_returns_none: false,
    };

    let saved = persist_pipeline_file_ingest(
        &ingest_lookup,
        &ingest_writer,
        &pipeline_store,
        &pipeline,
        &FileIngestConfig {
            filename: "updated.mp4".to_string(),
            loop_flag: false,
            start_time: "00:00:10".to_string(),
            live_optimized: false,
            target_gop_seconds: 2,
            playlist: Vec::new(),
        },
        || "generated".to_string(),
    )
    .await
    .unwrap();

    assert_eq!(saved.id, "ingest-current");
    assert_eq!(saved.filename, "updated.mp4");
    let deleted = restream_sync::lock::lock(&ingest_writer.deleted).clone();
    assert_eq!(deleted, vec!["ingest-stale".to_string()]);
}

#[tokio::test]
async fn resolve_file_ingest_context_surfaces_ingest_lookup_error() {
    let ingest_lookup = FakeIngestLookup {
        by_id: HashMap::new(),
        by_stream_key: HashMap::new(),
        error: Some("db unavailable"),
    };
    let pipeline_lookup = FakePipelineStore {
        pipelines: HashMap::new(),
        error: None,
    };

    let result = resolve_file_ingest_context(&ingest_lookup, &pipeline_lookup, "ingest-1").await;

    assert!(matches!(
        result,
        Err(ResolveFileIngestError::IngestLookup(_))
    ));
}

#[tokio::test]
async fn persist_pipeline_file_ingest_creates_new_ingest_when_none_exists() {
    let pipeline = Pipeline {
        id: "pipeline-1".to_string(),
        name: "Pipeline".to_string(),
        stream_key: "stream-key".to_string(),
        input_source: None,
        srt_ingest_policy: None,
    };
    let ingest_lookup = FakeIngestLookup {
        by_id: HashMap::new(),
        by_stream_key: HashMap::new(),
        error: None,
    };
    let pipeline_store = FakePipelineStore {
        pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
        error: None,
    };
    let ingest_writer = FakeIngestWriter {
        created: std::sync::Mutex::new(Vec::new()),
        deleted: std::sync::Mutex::new(Vec::new()),
        fail: None,
        update_returns_none: false,
    };

    let saved = persist_pipeline_file_ingest(
        &ingest_lookup,
        &ingest_writer,
        &pipeline_store,
        &pipeline,
        &FileIngestConfig {
            filename: "new.mp4".to_string(),
            loop_flag: true,
            start_time: "00:00:00".to_string(),
            live_optimized: true,
            target_gop_seconds: 4,
            playlist: Vec::new(),
        },
        || "generated-id".to_string(),
    )
    .await
    .unwrap();

    assert_eq!(saved.id, "generated-id");
    assert_eq!(saved.filename, "new.mp4");
    let created = restream_sync::lock::lock(&ingest_writer.created).clone();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].id, "generated-id");
    let deleted = restream_sync::lock::lock(&ingest_writer.deleted).clone();
    assert!(deleted.is_empty());
}

#[tokio::test]
async fn persist_pipeline_file_ingest_surfaces_race_when_update_target_disappears() {
    let existing = FakeIngestLookup::ingest("ingest-current", "stream-key");
    let pipeline = Pipeline {
        id: "pipeline-1".to_string(),
        name: "Pipeline".to_string(),
        stream_key: "stream-key".to_string(),
        input_source: None,
        srt_ingest_policy: None,
    };
    let ingest_lookup = FakeIngestLookup {
        by_id: HashMap::new(),
        by_stream_key: HashMap::from([("stream-key".to_string(), vec![existing])]),
        error: None,
    };
    let pipeline_store = FakePipelineStore {
        pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
        error: None,
    };
    let ingest_writer = FakeIngestWriter {
        created: std::sync::Mutex::new(Vec::new()),
        deleted: std::sync::Mutex::new(Vec::new()),
        fail: None,
        update_returns_none: true,
    };

    let result = persist_pipeline_file_ingest(
        &ingest_lookup,
        &ingest_writer,
        &pipeline_store,
        &pipeline,
        &FileIngestConfig {
            filename: "updated.mp4".to_string(),
            loop_flag: false,
            start_time: "00:00:10".to_string(),
            live_optimized: false,
            target_gop_seconds: 2,
            playlist: Vec::new(),
        },
        || "generated".to_string(),
    )
    .await;

    assert!(matches!(
        result,
        Err(PersistFileIngestError::IngestWrite(_))
    ));
}

#[tokio::test]
async fn remove_pipeline_file_ingest_deletes_all_ingests_and_clears_input_source() {
    let first = FakeIngestLookup::ingest("ingest-1", "stream-key");
    let second = FakeIngestLookup::ingest("ingest-2", "stream-key");
    let pipeline = Pipeline {
        id: "pipeline-1".to_string(),
        name: "Pipeline".to_string(),
        stream_key: "stream-key".to_string(),
        input_source: Some("file:clip.mp4".to_string()),
        srt_ingest_policy: None,
    };
    let ingest_lookup = FakeIngestLookup {
        by_id: HashMap::new(),
        by_stream_key: HashMap::from([(
            "stream-key".to_string(),
            vec![first.clone(), second.clone()],
        )]),
        error: None,
    };
    let pipeline_store = FakePipelineStore {
        pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
        error: None,
    };
    let ingest_writer = FakeIngestWriter {
        created: std::sync::Mutex::new(Vec::new()),
        deleted: std::sync::Mutex::new(Vec::new()),
        fail: None,
        update_returns_none: false,
    };

    remove_pipeline_file_ingest(&ingest_lookup, &ingest_writer, &pipeline_store, &pipeline)
        .await
        .unwrap();

    let mut deleted = restream_sync::lock::lock(&ingest_writer.deleted).clone();
    deleted.sort();
    assert_eq!(
        deleted,
        vec!["ingest-1".to_string(), "ingest-2".to_string()]
    );
}
