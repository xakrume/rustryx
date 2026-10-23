//! [`SystemConfigRepo`]: runtime configuration in the database, with sealed fields.
//!
//! One row per key (`settings.smtp`, `system.read_only`, ...), a JSON value, a revision from
//! `system_config_revision_seq` and the author. Every write publishes
//! `InvalidationEvent::Config(revision)` inside its transaction, so nodes hear of it after
//! commit only.
//!
//! Sealed fields. A caller names JSON pointers of strings to seal (`/password`). The stored
//! value then holds the envelope (`rx1.<key id>.<..>`, db.md section 7) at that pointer and a
//! top level `_sealed` array that lists the pointers, so the row says by itself which fields
//! are sealed and no side table or column is needed (the N-1 binary reads the row as opaque
//! JSON). The envelope is bound to the key AND the pointer: one copied to another key or to
//! another field of the same value does not open.

use std::sync::Arc;

use async_trait::async_trait;
use rustryx_core::StoreError;
use rustryx_core::ids::UserId;
use rustryx_core::ports::{ConfigEntry, InvalidationEvent, SecretView, SystemConfigRepo};
use serde_json::{Map, Value};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{backend, map_err};
use crate::repos::notify;
use crate::sealer::SecretSealer;

/// Longest key the table accepts (its CHECK).
const MAX_KEY_CHARS: usize = 200;
/// The reserved top level key that lists the sealed pointers of a stored value.
const SEALED_KEY: &str = "_sealed";

/// `system_config` accessor: small JSON values that operators change at runtime (settings,
/// the anonymous access switch, read only mode) without a redeploy.
#[derive(Debug, Clone)]
pub struct PgSystemConfig {
    pool: PgPool,
    sealer: Arc<dyn SecretSealer>,
}

impl PgSystemConfig {
    /// Creates the accessor over `pool`; sealed fields are protected with `sealer`.
    #[must_use]
    pub fn new(pool: PgPool, sealer: Arc<dyn SecretSealer>) -> Self {
        Self { pool, sealer }
    }
}

#[derive(Debug, FromRow)]
struct Row {
    key: String,
    value: Value,
    revision: i64,
    updated_at: OffsetDateTime,
    updated_by: Option<Uuid>,
}

fn check_key(key: &str) -> Result<(), StoreError> {
    if key.is_empty() || key.chars().count() > MAX_KEY_CHARS {
        return Err(StoreError::Invalid(
            "config key must be 1 to 200 characters".to_owned(),
        ));
    }
    Ok(())
}

/// A validated JSON pointer: the original text and its unescaped object keys.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pointer {
    text: String,
    tokens: Vec<String>,
}

impl Pointer {
    /// RFC 6901 syntax, restricted to object keys: it starts with `/`, has no empty token, and
    /// does not reach into the reserved `_sealed` list.
    fn parse(text: &str) -> Result<Self, StoreError> {
        let invalid = || StoreError::Invalid("invalid sealed field pointer".to_owned());
        let rest = text.strip_prefix('/').ok_or_else(invalid)?;
        let tokens: Vec<String> = rest
            .split('/')
            .map(|token| token.replace("~1", "/").replace("~0", "~"))
            .collect();
        if tokens.iter().any(String::is_empty)
            || tokens.first().map(String::as_str) == Some(SEALED_KEY)
        {
            return Err(invalid());
        }
        Ok(Self {
            text: text.to_owned(),
            tokens,
        })
    }

    /// The text of a string at the pointer.
    fn string_in<'a>(&self, value: &'a Value) -> Option<&'a str> {
        value.pointer(&self.text).and_then(Value::as_str)
    }

    /// Writes `new` at the pointer, creating the intermediate objects. `false` when a
    /// non object is in the way.
    fn set_in(&self, value: &mut Value, new: Value) -> bool {
        let Some((last, parents)) = self.tokens.split_last() else {
            return false;
        };
        let mut node = value;
        for token in parents {
            let Some(object) = node.as_object_mut() else {
                return false;
            };
            node = object
                .entry(token.clone())
                .or_insert_with(|| Value::Object(Map::new()));
        }
        match node.as_object_mut() {
            Some(object) => {
                object.insert(last.clone(), new);
                true
            }
            None => false,
        }
    }

    /// Removes the field at the pointer, when present.
    fn remove_in(&self, value: &mut Value) {
        let Some((last, parents)) = self.tokens.split_last() else {
            return;
        };
        let mut node = value;
        for token in parents {
            match node.get_mut(token.as_str()) {
                Some(next) => node = next,
                None => return,
            }
        }
        if let Some(object) = node.as_object_mut() {
            object.remove(last.as_str());
        }
    }
}

/// Associated data of an envelope: the key and the pointer, so a value cannot be moved to
/// another key or another field.
fn aad(key: &str, pointer: &str) -> Vec<u8> {
    let mut aad = b"rustryx/system-config/seal/v1/".to_vec();
    aad.extend_from_slice(key.as_bytes());
    aad.push(0);
    aad.extend_from_slice(pointer.as_bytes());
    aad
}

/// The pointers a stored value lists as sealed.
fn sealed_pointers(stored: &Value) -> Result<Vec<Pointer>, StoreError> {
    let corrupt = || {
        backend(
            "read system config",
            "stored row does not match the domain model",
        )
    };
    let Some(list) = stored.get(SEALED_KEY) else {
        return Ok(Vec::new());
    };
    list.as_array()
        .ok_or_else(corrupt)?
        .iter()
        .map(|item| {
            let text = item.as_str().ok_or_else(corrupt)?;
            Pointer::parse(text).map_err(|_| corrupt())
        })
        .collect()
}

fn entry(row: Row, value: Value) -> ConfigEntry {
    ConfigEntry {
        key: row.key,
        value,
        revision: row.revision,
        updated_at: row.updated_at,
        updated_by: row.updated_by.map(UserId::from_uuid),
    }
}

impl PgSystemConfig {
    /// The stored value as a reader may see it. `Mask` removes every sealed field and keeps
    /// the `_sealed` list, so the API can say `password_set: true` without the value;
    /// `Reveal` opens the envelopes and drops the list, so a typed consumer deserializes the
    /// plain section.
    fn present(&self, row: Row, view: SecretView) -> Result<ConfigEntry, StoreError> {
        let mut value = row.value.clone();
        let pointers = sealed_pointers(&value)?;
        if !pointers.is_empty() {
            match view {
                SecretView::Mask => {
                    for pointer in &pointers {
                        pointer.remove_in(&mut value);
                    }
                }
                SecretView::Reveal => {
                    for pointer in &pointers {
                        let envelope = pointer.string_in(&row.value).ok_or_else(|| {
                            backend("read system config", "sealed field is missing")
                        })?;
                        let opened = self.sealer.open(&aad(&row.key, &pointer.text), envelope)?;
                        if !pointer.set_in(&mut value, Value::String(opened.expose().to_owned())) {
                            return Err(backend("read system config", "sealed field is misplaced"));
                        }
                    }
                    if let Some(object) = value.as_object_mut() {
                        object.remove(SEALED_KEY);
                    }
                }
            }
        }
        Ok(entry(row, value))
    }

    /// The value to store for `value` given the previous stored value: sealed fields are
    /// sealed, carried over from `previous` when absent, or cleared by `null`.
    fn prepare(
        &self,
        key: &str,
        value: &Value,
        seal: &[&str],
        previous: Option<&Value>,
    ) -> Result<Value, StoreError> {
        let pointers: Vec<Pointer> = seal
            .iter()
            .map(|text| Pointer::parse(text))
            .collect::<Result<_, _>>()?;
        let mut unique: Vec<&str> = pointers.iter().map(|p| p.text.as_str()).collect();
        unique.sort_unstable();
        if unique.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(StoreError::Invalid(
                "duplicate sealed field pointer".to_owned(),
            ));
        }
        if let Some(object) = value.as_object()
            && object.contains_key(SEALED_KEY)
        {
            return Err(StoreError::Invalid(
                "the key _sealed is reserved".to_owned(),
            ));
        }
        if !pointers.is_empty() && !value.is_object() {
            return Err(StoreError::Invalid(
                "sealed fields need an object value".to_owned(),
            ));
        }
        let previous_sealed: Vec<String> = match previous {
            Some(old) => sealed_pointers(old)?.into_iter().map(|p| p.text).collect(),
            None => Vec::new(),
        };
        let misplaced = || StoreError::Invalid("sealed field cannot be placed".to_owned());
        let mut stored = value.clone();
        let mut sealed: Vec<String> = Vec::new();
        for pointer in &pointers {
            match value.pointer(&pointer.text) {
                Some(Value::String(plain)) => {
                    let envelope = self.sealer.seal(&aad(key, &pointer.text), plain)?;
                    if !pointer.set_in(&mut stored, Value::String(envelope)) {
                        return Err(misplaced());
                    }
                    sealed.push(pointer.text.clone());
                }
                // A form that leaves the password blank sends null: the stored secret is gone.
                Some(Value::Null) => {}
                Some(_) => {
                    return Err(StoreError::Invalid(
                        "a sealed field must be a string or null".to_owned(),
                    ));
                }
                // Absent: the stored secret stays, so editing another field of the section
                // does not need the password again.
                None => {
                    let carried = previous
                        .filter(|_| previous_sealed.contains(&pointer.text))
                        .and_then(|old| pointer.string_in(old));
                    if let Some(envelope) = carried {
                        if !pointer.set_in(&mut stored, Value::String(envelope.to_owned())) {
                            return Err(misplaced());
                        }
                        sealed.push(pointer.text.clone());
                    }
                }
            }
        }
        if !sealed.is_empty() {
            sealed.sort_unstable();
            if let Some(object) = stored.as_object_mut() {
                object.insert(
                    SEALED_KEY.to_owned(),
                    Value::Array(sealed.into_iter().map(Value::String).collect()),
                );
            }
        }
        Ok(stored)
    }
}

const SELECT: &str = "SELECT key, value, revision, updated_at, updated_by FROM system_config";

#[async_trait]
impl SystemConfigRepo for PgSystemConfig {
    async fn get(&self, key: &str, view: SecretView) -> Result<Option<ConfigEntry>, StoreError> {
        check_key(key)?;
        sqlx::query_as::<_, Row>(
            "SELECT key, value, revision, updated_at, updated_by FROM system_config WHERE key = $1",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_err("read system config", e))?
        .map(|row| self.present(row, view))
        .transpose()
    }

    async fn list_prefix(
        &self,
        prefix: &str,
        view: SecretView,
    ) -> Result<Vec<ConfigEntry>, StoreError> {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(SELECT);
        builder
            .push(" WHERE key LIKE ")
            .push_bind(format!("{}%", crate::repos::escape_like(prefix)))
            .push(" ESCAPE '\\' ORDER BY key");
        builder
            .build_query_as::<Row>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| map_err("list system config", e))?
            .into_iter()
            .map(|row| self.present(row, view))
            .collect()
    }

    /// One transaction: lock the row, compare the revision, seal, upsert, announce. The row
    /// lock makes the compare and set exact (two editors of one section cannot both pass), and
    /// the revision of the result comes from the sequence through the table default or the
    /// `system_config_bump` trigger, never from the caller. The returned entry is the MASKED
    /// view: a write must not echo a secret back.
    async fn put(
        &self,
        key: &str,
        value: &Value,
        seal: &[&str],
        expected_revision: Option<i64>,
        actor: Option<UserId>,
    ) -> Result<ConfigEntry, StoreError> {
        check_key(key)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("write system config", e))?;
        let current: Option<(Value, i64)> =
            sqlx::query_as("SELECT value, revision FROM system_config WHERE key = $1 FOR UPDATE")
                .bind(key)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| map_err("lock system config", e))?;
        if let Some(expected) = expected_revision
            && current.as_ref().map(|(_, revision)| *revision) != Some(expected)
        {
            return Err(StoreError::Conflict(
                "settings were changed by someone else".to_owned(),
            ));
        }
        let stored = self.prepare(key, value, seal, current.as_ref().map(|(old, _)| old))?;
        let row: Row = sqlx::query_as(
            "INSERT INTO system_config (key, value, updated_by) VALUES ($1, $2, $3) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, \
                 updated_by = EXCLUDED.updated_by \
             RETURNING key, value, revision, updated_at, updated_by",
        )
        .bind(key)
        .bind(&stored)
        .bind(actor.map(|a| a.as_uuid()))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_err("write system config", e))?;
        notify(&mut tx, &InvalidationEvent::Config(row.revision)).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("write system config", e))?;
        self.present(row, SecretView::Mask)
    }

    /// Removing a missing key succeeds and announces nothing. A removal takes a fresh
    /// revision from the sequence for its announcement, so the nodes that compare revisions
    /// reload; `current_revision` is a maximum over existing rows and can therefore go down
    /// after a delete, which is why a node must also reload on the event itself.
    async fn delete(&self, key: &str, _actor: Option<UserId>) -> Result<(), StoreError> {
        check_key(key)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_err("delete system config", e))?;
        let deleted = sqlx::query("DELETE FROM system_config WHERE key = $1")
            .bind(key)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_err("delete system config", e))?;
        if deleted.rows_affected() == 0 {
            return Ok(());
        }
        let revision: i64 = sqlx::query_scalar("SELECT nextval('system_config_revision_seq')")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| map_err("delete system config", e))?;
        notify(&mut tx, &InvalidationEvent::Config(revision)).await?;
        tx.commit()
            .await
            .map_err(|e| map_err("delete system config", e))
    }

    async fn current_revision(&self) -> Result<i64, StoreError> {
        sqlx::query_scalar(
            "SELECT COALESCE(MAX(revision), 0)::bigint FROM system_config \
             WHERE key LIKE 'settings.%' OR key LIKE 'system.%'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_err("read config revision", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointers_follow_rfc_6901_for_object_keys() {
        let pointer = Pointer::parse("/auth/pass~1word/a~0b").unwrap();
        assert_eq!(pointer.tokens, ["auth", "pass/word", "a~b"]);
        for bad in ["", "password", "/", "/a//b", "/_sealed", "/_sealed/x"] {
            assert!(
                matches!(Pointer::parse(bad), Err(StoreError::Invalid(_))),
                "{bad:?}"
            );
        }
        assert!(Pointer::parse("/a/_sealed").is_ok());
    }

    #[test]
    fn set_creates_objects_and_refuses_to_cross_a_scalar() {
        let pointer = Pointer::parse("/a/b").unwrap();
        let mut value = serde_json::json!({});
        assert!(pointer.set_in(&mut value, Value::String("x".into())));
        assert_eq!(value, serde_json::json!({"a": {"b": "x"}}));
        let mut blocked = serde_json::json!({"a": 5});
        assert!(!pointer.set_in(&mut blocked, Value::Null));
        let mut scalar = Value::String("s".into());
        assert!(!pointer.set_in(&mut scalar, Value::Null));
    }

    #[test]
    fn remove_ignores_a_missing_path() {
        let pointer = Pointer::parse("/a/b").unwrap();
        let mut value = serde_json::json!({"a": {"b": 1, "c": 2}});
        pointer.remove_in(&mut value);
        assert_eq!(value, serde_json::json!({"a": {"c": 2}}));
        let mut other = serde_json::json!({"x": 1});
        pointer.remove_in(&mut other);
        assert_eq!(other, serde_json::json!({"x": 1}));
    }

    #[test]
    fn keys_are_length_checked_as_invalid_input() {
        assert!(check_key("settings.smtp").is_ok());
        assert!(matches!(check_key(""), Err(StoreError::Invalid(_))));
        assert!(matches!(
            check_key(&"k".repeat(201)),
            Err(StoreError::Invalid(_))
        ));
        assert!(check_key(&"k".repeat(200)).is_ok());
    }

    #[test]
    fn sealed_list_must_be_an_array_of_valid_pointers() {
        let ok = serde_json::json!({"_sealed": ["/a", "/b/c"]});
        assert_eq!(sealed_pointers(&ok).unwrap().len(), 2);
        assert_eq!(sealed_pointers(&serde_json::json!({})).unwrap().len(), 0);
        for bad in [
            serde_json::json!({"_sealed": "x"}),
            serde_json::json!({"_sealed": [1]}),
            serde_json::json!({"_sealed": ["nope"]}),
        ] {
            assert!(sealed_pointers(&bad).is_err(), "{bad}");
        }
    }
}
