//! The settings store (`system_config`) and read only mode.

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::ids::UserId;
use rustryx_core::ports::{ConfigEntry, InvalidationEvent, SecretView, SystemConfigRepo};
use rustryx_core::settings::ReadOnlyState;
use serde_json::Value;
use time::OffsetDateTime;

use super::guards::READ_ONLY_KEY;
use super::state::{ConfigRow, State};
use super::{MemStore, conflict, not_found};

/// Longest `system_config` key.
const MAX_CONFIG_KEY_CHARS: usize = 200;

fn pointer_tokens(pointer: &str) -> Vec<String> {
    pointer
        .split('/')
        .skip(1)
        .map(|token| token.replace("~1", "/").replace("~0", "~"))
        .collect()
}

/// Sets the value at a JSON pointer, creating the objects on the way. Only objects are
/// walked, because a sealed field is a member of an object.
fn set_at(root: &mut Value, pointer: &str, value: Value) {
    let tokens = pointer_tokens(pointer);
    let Some((last, parents)) = tokens.split_last() else {
        return;
    };
    let mut node = root;
    for token in parents {
        if !node.is_object() {
            return;
        }
        node = &mut node[token.as_str()];
        if node.is_null() {
            *node = Value::Object(serde_json::Map::new());
        }
    }
    if let Some(object) = node.as_object_mut() {
        object.insert(last.clone(), value);
    }
}

/// Removes the member at a JSON pointer, if there is one.
fn remove_at(root: &mut Value, pointer: &str) {
    let tokens = pointer_tokens(pointer);
    let Some((last, parents)) = tokens.split_last() else {
        return;
    };
    let mut node = root;
    for token in parents {
        let Some(next) = node.get_mut(token.as_str()) else {
            return;
        };
        node = next;
    }
    if let Some(object) = node.as_object_mut() {
        object.remove(last);
    }
}

fn entry_of(key: &str, row: &ConfigRow, view: SecretView) -> ConfigEntry {
    let mut value = row.value.clone();
    match view {
        SecretView::Mask => {
            if !row.sealed.is_empty() {
                // The pointers of the sealed fields, so the API can say "password set" without
                // the value ever leaving the store.
                let pointers: Vec<&String> = row.sealed.keys().collect();
                if let Some(object) = value.as_object_mut() {
                    object.insert("_sealed".to_owned(), serde_json::json!(pointers));
                }
            }
        }
        SecretView::Reveal => {
            for (pointer, secret) in &row.sealed {
                set_at(&mut value, pointer, Value::String(secret.clone()));
            }
        }
    }
    ConfigEntry {
        key: key.to_owned(),
        value,
        revision: row.revision,
        updated_at: row.updated_at,
        updated_by: row.updated_by,
    }
}

impl State {
    /// Writes a row, assigning the next revision. `seal` pointers move out of the value into
    /// the sealed map; a sealed field absent from `value` keeps its stored text, a JSON null
    /// clears it.
    fn put_config(
        &mut self,
        key: &str,
        value: &Value,
        seal: &[&str],
        expected_revision: Option<i64>,
        actor: Option<UserId>,
        now: OffsetDateTime,
    ) -> Result<i64, StoreError> {
        if key.is_empty() || key.chars().count() > MAX_CONFIG_KEY_CHARS {
            return Err(StoreError::Invalid(
                "a config key has 1 to 200 characters".to_owned(),
            ));
        }
        let existing = self.config.get(key);
        match (existing, expected_revision) {
            (Some(row), Some(expected)) if row.revision != expected => {
                return Err(conflict(format!(
                    "stale revision: expected {expected}, found {}",
                    row.revision
                )));
            }
            (None, Some(_)) => {
                return Err(conflict("stale revision: the key does not exist"));
            }
            _ => {}
        }
        let mut stored = value.clone();
        let mut sealed = existing.map(|row| row.sealed.clone()).unwrap_or_default();
        for pointer in seal {
            match value.pointer(pointer) {
                Some(Value::String(secret)) => {
                    sealed.insert((*pointer).to_owned(), secret.clone());
                    remove_at(&mut stored, pointer);
                }
                Some(Value::Null) => {
                    sealed.remove(*pointer);
                    remove_at(&mut stored, pointer);
                }
                None => {}
                Some(_) => {
                    return Err(StoreError::Invalid(format!(
                        "the sealed field {pointer} must be a string or null"
                    )));
                }
            }
        }
        let revision = self.next_config_revision();
        self.config.insert(
            key.to_owned(),
            ConfigRow {
                value: stored,
                sealed,
                revision,
                updated_at: now,
                updated_by: actor,
            },
        );
        Ok(revision)
    }
}

#[async_trait]
impl SystemConfigRepo for MemStore {
    async fn get(&self, key: &str, view: SecretView) -> Result<Option<ConfigEntry>, StoreError> {
        let state = self.tables()?;
        Ok(state.config.get(key).map(|row| entry_of(key, row, view)))
    }

    async fn list_prefix(
        &self,
        prefix: &str,
        view: SecretView,
    ) -> Result<Vec<ConfigEntry>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .config
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, row)| entry_of(key, row, view))
            .collect())
    }

    async fn put(
        &self,
        key: &str,
        value: &Value,
        seal: &[&str],
        expected_revision: Option<i64>,
        actor: Option<UserId>,
    ) -> Result<ConfigEntry, StoreError> {
        let now = self.now();
        let mut state = self.tables()?;
        let revision = state.put_config(key, value, seal, expected_revision, actor, now)?;
        let entry = state
            .config
            .get(key)
            .map(|row| entry_of(key, row, SecretView::Mask))
            .ok_or_else(|| not_found("config key", key))?;
        drop(state);
        self.notify(InvalidationEvent::Config(revision));
        Ok(entry)
    }

    async fn delete(&self, key: &str, _actor: Option<UserId>) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.config.remove(key).is_none() {
            return Err(not_found("config key", key));
        }
        // A delete takes a revision of its own, so nodes see that something changed even when
        // the highest remaining row is older.
        let revision = state.next_config_revision();
        drop(state);
        self.notify(InvalidationEvent::Config(revision));
        Ok(())
    }

    async fn current_revision(&self) -> Result<i64, StoreError> {
        let state = self.tables()?;
        Ok(state
            .config
            .iter()
            .filter(|(key, _)| key.starts_with("settings.") || key.starts_with("system."))
            .map(|(_, row)| row.revision)
            .max()
            .unwrap_or(0))
    }
}

impl MemStore {
    /// Switches read only mode on or off the way the system endpoint does: one write of
    /// `system.read_only`, which every guard of the store reads and which publishes
    /// `InvalidationEvent::Config`. Writes of assets and components are then refused with
    /// `PolicyViolation::ReadOnly` (or `Conflict` through the phase 1 methods).
    ///
    /// # Errors
    /// `Unavailable` when the store is switched off.
    pub fn set_read_only(&self, enabled: bool, reason: &str) -> Result<(), StoreError> {
        let now = self.now();
        let state = ReadOnlyState {
            enabled,
            reason: reason.to_owned(),
            set_by: None,
            set_at: enabled.then_some(now),
        };
        let value = serde_json::to_value(&state)
            .map_err(|_| StoreError::Invalid("read only state does not serialize".to_owned()))?;
        let mut tables = self.tables()?;
        let revision = tables.put_config(READ_ONLY_KEY, &value, &[], None, None, now)?;
        drop(tables);
        self.notify(InvalidationEvent::Config(revision));
        Ok(())
    }
}
