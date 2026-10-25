use crate::domain::pipeline_input::{
    PipelineInput, PipelineInputRole, PipelineInputSource, PipelineInputSourceKind, PullAuth,
    PushAuth, VariantRule,
};
use sqlx::{FromRow, SqlitePool};
use tracing::warn;

#[derive(FromRow)]
struct PipelineInputRow {
    id: String,
    pipeline_id: String,
    label: String,
    stream_key: String,
    role: String,
    enabled: bool,
    selected: bool,
    source_kind: String,
    source_url: Option<String>,
    variant_rule: String,
    source_auth: Option<String>,
    push_auth: Option<String>,
}

impl TryFrom<PipelineInputRow> for PipelineInput {
    type Error = sqlx::Error;

    fn try_from(row: PipelineInputRow) -> Result<Self, Self::Error> {
        let input_id = row.id.clone();
        let role = PipelineInputRole::try_from(row.role.as_str())
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let source_kind = PipelineInputSourceKind::try_from(row.source_kind.as_str())
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let variant_rule = VariantRule::try_from(row.variant_rule.as_str())
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        Ok(Self {
            id: row.id,
            pipeline_id: row.pipeline_id,
            label: row.label,
            stream_key: row.stream_key,
            role,
            enabled: row.enabled,
            selected: row.selected,
            source_kind,
            source_url: row.source_url,
            variant_rule,
            source_auth: deserialize_auth(row.source_auth.as_deref(), &input_id),
            push_auth: deserialize_push_auth(row.push_auth.as_deref())?,
        })
    }
}

const SELECT_FIELDS: &str = "id, pipeline_id, label, stream_key, role, enabled, selected, \
     source_kind, source_url, variant_rule, source_auth, push_auth";

/// Serialize the pull auth for the `source_auth` column; `None` for no auth.
fn serialize_auth(auth: &PullAuth) -> Option<String> {
    match auth {
        PullAuth::None => None,
        _ => serde_json::to_string(auth).ok(),
    }
}

fn serialize_push_auth(auth: &PushAuth) -> Result<Option<String>, sqlx::Error> {
    match auth {
        PushAuth::None => Ok(None),
        PushAuth::Basic { .. } => serde_json::to_string(auth)
            .map(Some)
            .map_err(|_| sqlx::Error::Protocol("cannot serialize pipeline input push auth".into())),
    }
}

/// Only SQL NULL means unset. Corrupt configured state must never permit a push.
fn deserialize_push_auth(raw: Option<&str>) -> Result<PushAuth, sqlx::Error> {
    const INVALID: &str = "invalid stored pipeline input push auth";
    let Some(raw) = raw else {
        return Ok(PushAuth::None);
    };
    // The column CHECK constrains JSON and `kind`, not a variant's fields, so
    // validate the exact shape here: a stored `none` with a stray hash, or a
    // basic row missing a field, must fail closed instead of becoming no auth.
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| sqlx::Error::Protocol(INVALID.into()))?;
    let object = value
        .as_object()
        .ok_or_else(|| sqlx::Error::Protocol(INVALID.into()))?;
    match object.get("kind").and_then(serde_json::Value::as_str) {
        Some("none") if object.len() == 1 => Ok(PushAuth::None),
        Some("basic") if object.len() == 3 => {
            let auth: PushAuth =
                serde_json::from_value(value).map_err(|_| sqlx::Error::Protocol(INVALID.into()))?;
            if matches!(&auth, PushAuth::Basic { username, password_hash }
                if username.is_empty() || password_hash.is_empty())
            {
                return Err(sqlx::Error::Protocol(INVALID.into()));
            }
            Ok(auth)
        }
        _ => Err(sqlx::Error::Protocol(INVALID.into())),
    }
}

/// Parse the `source_auth` column. An empty or absent value means no auth.
///
/// A value that does not parse is treated as no auth, and logged. The column's
/// CHECK constrains the JSON and the kind, not the variant's own fields, so a
/// row can hold `{"kind":"bearer"}` with no token; failing the whole row (or
/// the whole list) over one input's credential would take the inputs list and
/// with it SRT's policy store down for every pipeline.
fn deserialize_auth(raw: Option<&str>, input_id: &str) -> PullAuth {
    match raw {
        Some(raw) if !raw.trim().is_empty() => serde_json::from_str::<PullAuth>(raw)
            .inspect_err(|error| {
                warn!(input_id = %input_id, err = %error,
                    "pipeline input source auth does not parse; showing it as none");
            })
            .unwrap_or(PullAuth::None),
        _ => PullAuth::None,
    }
}

/// Whether the stored credential parses as the variant its kind names. A pull
/// whose credential does not parse is refused rather than fetched without it:
/// an origin that answers an unauthenticated request with a slate or a preview
/// would otherwise publish that as this input's content.
fn auth_parses(raw: Option<&str>) -> bool {
    match raw {
        Some(raw) if !raw.trim().is_empty() => serde_json::from_str::<PullAuth>(raw).is_ok(),
        _ => true,
    }
}

/// Convert listed rows, skipping one the schema cannot describe rather than
/// failing the list: an unreadable row must not take every other pipeline's
/// inputs (and the SRT policy store built from them) with it.
fn convert_rows(rows: Vec<PipelineInputRow>) -> Vec<PipelineInput> {
    rows.into_iter()
        .filter_map(|row| {
            let input_id = row.id.clone();
            match PipelineInput::try_from(row) {
                Ok(input) => Some(input),
                Err(error) => {
                    warn!(input_id = %input_id, err = %error,
                        "pipeline input row skipped: it does not match the schema");
                    None
                }
            }
        })
        .collect()
}

fn primary_input_id(pipeline_id: &str) -> String {
    format!("input_primary_{pipeline_id}")
}

pub(crate) async fn insert_primary_pipeline_input(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    pipeline_id: &str,
    stream_key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO pipeline_inputs
         (id, pipeline_id, label, stream_key, role, enabled, selected)
         VALUES (?, ?, 'Primary', ?, 'primary', 1, 1)",
    )
    .bind(primary_input_id(pipeline_id))
    .bind(pipeline_id)
    .bind(stream_key)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn convert_optional(row: Option<PipelineInputRow>) -> Result<Option<PipelineInput>, sqlx::Error> {
    row.map(PipelineInput::try_from).transpose()
}

pub async fn get_pipeline_input(
    pool: &SqlitePool,
    pipeline_id: &str,
    input_id: &str,
) -> Result<Option<PipelineInput>, sqlx::Error> {
    let query =
        format!("SELECT {SELECT_FIELDS} FROM pipeline_inputs WHERE pipeline_id = ? AND id = ?");
    convert_optional(
        sqlx::query_as::<_, PipelineInputRow>(sqlx::AssertSqlSafe(query))
            .bind(pipeline_id)
            .bind(input_id)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn get_pipeline_input_by_stream_key(
    pool: &SqlitePool,
    stream_key: &str,
) -> Result<Option<PipelineInput>, sqlx::Error> {
    let query = format!("SELECT {SELECT_FIELDS} FROM pipeline_inputs WHERE stream_key = ?");
    convert_optional(
        sqlx::query_as::<_, PipelineInputRow>(sqlx::AssertSqlSafe(query))
            .bind(stream_key)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn list_pipeline_inputs(
    pool: &SqlitePool,
    pipeline_id: &str,
) -> Result<Vec<PipelineInput>, sqlx::Error> {
    let query = format!(
        "SELECT {SELECT_FIELDS} FROM pipeline_inputs
         WHERE pipeline_id = ? ORDER BY role DESC, rowid ASC"
    );
    let rows = sqlx::query_as::<_, PipelineInputRow>(sqlx::AssertSqlSafe(query))
        .bind(pipeline_id)
        .fetch_all(pool)
        .await?;
    Ok(convert_rows(rows))
}

/// Every input of every pipeline, each pipeline's in the order
/// [`list_pipeline_inputs`] lists them.
pub async fn list_all_pipeline_inputs(
    pool: impl sqlx::SqliteExecutor<'_>,
) -> Result<Vec<PipelineInput>, sqlx::Error> {
    let query = format!(
        "SELECT {SELECT_FIELDS} FROM pipeline_inputs
         ORDER BY pipeline_id ASC, role DESC, rowid ASC"
    );
    let rows = sqlx::query_as::<_, PipelineInputRow>(sqlx::AssertSqlSafe(query))
        .fetch_all(pool)
        .await?;
    Ok(convert_rows(rows))
}

pub async fn create_pipeline_input(
    pool: &SqlitePool,
    id: &str,
    pipeline_id: &str,
    label: &str,
    stream_key: &str,
    source: &PipelineInputSource,
) -> Result<PipelineInput, sqlx::Error> {
    sqlx::query(
        "INSERT INTO pipeline_inputs
         (id, pipeline_id, label, stream_key, role, enabled, selected,
          source_kind, source_url, variant_rule, source_auth, push_auth)
         VALUES (?, ?, ?, ?, 'backup', 1, 0, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(pipeline_id)
    .bind(label)
    .bind(stream_key)
    .bind(source.kind.as_str())
    .bind(source.url.as_deref())
    .bind(source.variant_rule.as_str())
    .bind(serialize_auth(&source.auth))
    .bind(serialize_push_auth(&source.push_auth)?)
    .execute(pool)
    .await?;
    get_pipeline_input(pool, pipeline_id, id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

/// Every `srt_call` input, enabled or not: the reconciler needs the
/// disabled ones too, to hang up a call that is already up.
pub async fn list_srt_call_inputs(pool: &SqlitePool) -> Result<Vec<PipelineInput>, sqlx::Error> {
    let query = format!(
        "SELECT {SELECT_FIELDS} FROM pipeline_inputs
         WHERE source_kind = 'srt_call' ORDER BY pipeline_id ASC, id ASC"
    );
    let rows = sqlx::query_as::<_, PipelineInputRow>(sqlx::AssertSqlSafe(query))
        .fetch_all(pool)
        .await?;
    Ok(convert_rows(rows))
}

/// Every `hls_pull` input, enabled or not: the reconciler needs the disabled
/// ones too, to cancel a puller that is already running.
pub async fn list_pull_inputs(pool: &SqlitePool) -> Result<Vec<PipelineInput>, sqlx::Error> {
    let query = format!(
        "SELECT {SELECT_FIELDS} FROM pipeline_inputs
         WHERE source_kind = 'hls_pull' ORDER BY pipeline_id ASC, id ASC"
    );
    let rows = sqlx::query_as::<_, PipelineInputRow>(sqlx::AssertSqlSafe(query))
        .fetch_all(pool)
        .await?;
    // A row whose credential does not parse is left out of *this* list, so the
    // pull is refused instead of fetched without it; the inputs list still
    // shows the row, so an operator can see and fix it.
    let usable: Vec<PipelineInputRow> = rows
        .into_iter()
        .filter(|row| {
            let parses = auth_parses(row.source_auth.as_deref());
            if !parses {
                warn!(input_id = %row.id,
                    "HLS pull not started: its stored credential does not parse");
            }
            parses
        })
        .collect();
    Ok(convert_rows(usable))
}

/// The ids of every input row, whatever its source kind. The reconciler needs
/// them to release the ingest key state of an input whose row is gone. An id
/// query is enough there, and it cannot warn about or skip a row the way
/// converting full rows would.
pub async fn list_pipeline_input_ids(
    pool: &SqlitePool,
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM pipeline_inputs")
        .fetch_all(pool)
        .await?;
    Ok(ids.into_iter().collect())
}

pub async fn promote_pipeline_input(
    pool: &SqlitePool,
    pipeline_id: &str,
    input_id: &str,
) -> Result<Option<PipelineInput>, sqlx::Error> {
    super::with_busy_retry(|| async {
        let mut transaction = pool.begin().await?;
        let target_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
            SELECT 1 FROM pipeline_inputs
            WHERE pipeline_id = ? AND id = ? AND enabled = 1
        )",
        )
        .bind(pipeline_id)
        .bind(input_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !target_exists {
            return Ok(None);
        }
        // Two statements, not one CASE-based UPDATE: `idx_pipeline_inputs_one_selected`
        // is a partial unique index that SQLite checks after each row a
        // multi-row UPDATE touches, in rowid order, not once at end-of-statement.
        // A single UPDATE that flips every row's `selected` together can hit a
        // transient state where the target row is set to 1 before the
        // currently-selected row (a lower rowid, e.g. the primary input created
        // first) is cleared, tripping the unique constraint on a perfectly valid
        // end state. Clearing first removes that transient collision.
        sqlx::query(
            "UPDATE pipeline_inputs SET selected = 0 WHERE pipeline_id = ? AND selected = 1",
        )
        .bind(pipeline_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("UPDATE pipeline_inputs SET selected = 1 WHERE pipeline_id = ? AND id = ?")
            .bind(pipeline_id)
            .bind(input_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        get_pipeline_input(pool, pipeline_id, input_id).await
    })
    .await
}

pub async fn update_pipeline_input(
    pool: &SqlitePool,
    pipeline_id: &str,
    input_id: &str,
    label: &str,
    enabled: bool,
    source: &PipelineInputSource,
) -> Result<Option<PipelineInput>, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE pipeline_inputs
         SET label = ?, enabled = ?, source_kind = ?, source_url = ?,
             variant_rule = ?, source_auth = ?, push_auth = ?
         WHERE pipeline_id = ? AND id = ?",
    )
    .bind(label)
    .bind(enabled)
    .bind(source.kind.as_str())
    .bind(source.url.as_deref())
    .bind(source.variant_rule.as_str())
    .bind(serialize_auth(&source.auth))
    .bind(serialize_push_auth(&source.push_auth)?)
    .bind(pipeline_id)
    .bind(input_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_pipeline_input(pool, pipeline_id, input_id).await
}

pub async fn delete_pipeline_input(
    pool: &SqlitePool,
    pipeline_id: &str,
    input_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM pipeline_inputs WHERE pipeline_id = ? AND id = ?")
        .bind(pipeline_id)
        .bind(input_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_pipeline, create_pool, setup_database_schema};

    async fn test_pool() -> SqlitePool {
        let pool = create_pool("sqlite::memory:").await.unwrap();
        setup_database_schema(&pool).await.unwrap();
        pool
    }

    /// A database from before `srt_call` keeps every row through the CHECK
    /// rebuild, then accepts caller inputs, with its indexes back in place.
    #[tokio::test]
    async fn the_source_kind_rebuild_keeps_rows_and_admits_srt_call_inputs() {
        let dir = std::env::temp_dir().join(format!(
            "restream-srt-call-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite://{}", dir.join("db.sqlite").display());
        let pool = create_pool(&url).await.unwrap();
        setup_database_schema(&pool).await.unwrap();
        // Put the table back the way a previous release created it.
        let old = sqlx::query_scalar::<_, String>(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'pipeline_inputs'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .replace("'keyed', 'hls_pull', 'srt_call'", "'keyed', 'hls_pull'")
        .replace("pipeline_inputs", "pipeline_inputs_old");
        sqlx::query(sqlx::AssertSqlSafe(old))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DROP TABLE pipeline_inputs")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE pipeline_inputs_old RENAME TO pipeline_inputs")
            .execute(&pool)
            .await
            .unwrap();
        create_pipeline(&pool, "p1", "Pipeline", "sk1", None, None)
            .await
            .unwrap();
        let pull = PipelineInputSource {
            kind: PipelineInputSourceKind::HlsPull,
            url: Some("http://origin.example/master.m3u8".to_string()),
            ..PipelineInputSource::keyed()
        };
        create_pipeline_input(&pool, "in-pull", "p1", "Pull", "sk-pull", &pull)
            .await
            .unwrap();
        let call = PipelineInputSource {
            kind: PipelineInputSourceKind::SrtCall,
            url: Some("srt://encoder.example:9000".to_string()),
            ..PipelineInputSource::keyed()
        };
        assert!(
            create_pipeline_input(&pool, "in-call", "p1", "Call", "sk-call", &call)
                .await
                .is_err(),
            "the old CHECK refuses the new kind"
        );

        setup_database_schema(&pool).await.unwrap();
        create_pipeline_input(&pool, "in-call", "p1", "Call", "sk-call", &call)
            .await
            .unwrap();
        let ids: Vec<String> = list_pipeline_inputs(&pool, "p1")
            .await
            .unwrap()
            .into_iter()
            .map(|input| input.id)
            .collect();
        assert!(ids.contains(&"in-pull".to_string()), "{ids:?}");
        let calls = list_srt_call_inputs(&pool).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].source_url.as_deref(),
            Some("srt://encoder.example:9000")
        );
        assert!(
            create_pipeline_input(&pool, "in-dup", "p1", "Dup", "sk-call", &call)
                .await
                .is_err(),
            "the stream-key unique index is rebuilt"
        );
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn promote_can_fail_back_to_an_input_created_before_the_current_selection() {
        // The primary input is inserted first (lowest rowid) and starts
        // selected. Promoting a later-created backup input, then failing
        // back to the primary, must succeed both ways: the partial unique
        // index on `selected = 1` must not reject a valid end state just
        // because the two-row UPDATE crosses rowid order transiently.
        let pool = test_pool().await;
        create_pipeline(&pool, "p1", "Pipeline", "sk1", None, None)
            .await
            .unwrap();
        create_pipeline_input(
            &pool,
            "input_backup_1",
            "p1",
            "Backup",
            "sk2",
            &PipelineInputSource::keyed(),
        )
        .await
        .unwrap();

        let promoted = promote_pipeline_input(&pool, "p1", "input_backup_1")
            .await
            .unwrap()
            .expect("backup input exists");
        assert!(promoted.selected);

        let failed_back = promote_pipeline_input(&pool, "p1", "input_primary_p1")
            .await
            .unwrap()
            .expect("primary input exists");
        assert!(
            failed_back.selected,
            "failing back to the primary input must succeed, not hit the unique-selected index"
        );

        let inputs = list_pipeline_inputs(&pool, "p1").await.unwrap();
        let selected_count = inputs.iter().filter(|input| input.selected).count();
        assert_eq!(
            selected_count, 1,
            "exactly one input must remain selected after fail-back"
        );
    }
}
