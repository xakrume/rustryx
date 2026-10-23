//! Robot tokens, and the expiry notice rows of personal tokens.

use std::time::Duration;

use async_trait::async_trait;
use rustryx_core::error::StoreError;
use rustryx_core::ids::RobotTokenId;
use rustryx_core::ports::{Page, PageRequest, RobotFilter, RobotState, RobotTokenRepo};
use rustryx_core::robot::RobotToken;
use time::OffsetDateTime;

use super::paging::paginate;
use super::support::bump_version;
use super::{MemStore, conflict, not_found};
use crate::support::add_std;

/// Derived state of a robot, most final first: a revoked robot is `Revoked` even when it is
/// also disabled or past its expiry.
fn state_of(robot: &RobotToken, now: OffsetDateTime) -> RobotState {
    if robot.revoked_at.is_some() {
        RobotState::Revoked
    } else if robot.disabled_at.is_some() {
        RobotState::Disabled
    } else if robot.expires_at.is_some_and(|expiry| expiry <= now) {
        RobotState::Expired
    } else {
        RobotState::Active
    }
}

#[async_trait]
impl RobotTokenRepo for MemStore {
    async fn create(&self, token: &RobotToken) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if let Some(project) = token.project_id
            && !state.projects.contains_key(&project)
        {
            return Err(conflict(format!("project {project} does not exist")));
        }
        let duplicate = state
            .robots
            .values()
            .any(|r| r.id == token.id || r.name == token.name || r.token_hash == token.token_hash);
        if duplicate {
            return Err(conflict("a robot with that name or secret already exists"));
        }
        state.robots.insert(token.id, token.clone());
        Ok(())
    }

    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<RobotToken>, StoreError> {
        let state = self.tables()?;
        Ok(state
            .robots
            .values()
            .find(|r| r.token_hash == token_hash)
            .cloned())
    }

    async fn get(&self, id: RobotTokenId) -> Result<Option<RobotToken>, StoreError> {
        Ok(self.tables()?.robots.get(&id).cloned())
    }

    async fn list(
        &self,
        filter: &RobotFilter,
        page: &PageRequest,
    ) -> Result<Page<RobotToken>, StoreError> {
        let now = self.now();
        let rows: Vec<(String, RobotToken)> = self
            .tables()?
            .robots
            .values()
            .filter(|r| filter.project.is_none_or(|p| r.project_id == Some(p)))
            .filter(|r| !filter.system_only || r.project_id.is_none())
            .filter(|r| filter.state.is_none_or(|wanted| state_of(r, now) == wanted))
            .map(|r| (r.name.clone(), r.clone()))
            .collect();
        paginate(rows, page)
    }

    async fn rotate(
        &self,
        id: RobotTokenId,
        new_hash: &str,
        new_prefix: &str,
        at: OffsetDateTime,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let existing = state
            .robots
            .get(&id)
            .ok_or_else(|| not_found("robot token", id))?;
        let version = bump_version(existing.version, expected_version)?;
        if state
            .robots
            .values()
            .any(|r| r.id != id && r.token_hash == new_hash)
        {
            return Err(conflict("a robot with that secret already exists"));
        }
        if let Some(robot) = state.robots.get_mut(&id) {
            // Only one hash is stored, so the old secret stops working in this very write.
            // A disabled robot stays disabled: rotation never re-enables.
            new_hash.clone_into(&mut robot.token_hash);
            new_prefix.clone_into(&mut robot.display_prefix);
            robot.rotated_at = Some(at);
            robot.version = version;
        }
        Ok(())
    }

    async fn set_disabled(
        &self,
        id: RobotTokenId,
        disabled: bool,
        at: OffsetDateTime,
        expected_version: i64,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let robot = state
            .robots
            .get_mut(&id)
            .ok_or_else(|| not_found("robot token", id))?;
        let version = bump_version(robot.version, expected_version)?;
        robot.disabled_at = disabled.then_some(at);
        robot.version = version;
        Ok(())
    }

    async fn revoke(&self, id: RobotTokenId, at: OffsetDateTime) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        let robot = state
            .robots
            .get_mut(&id)
            .ok_or_else(|| not_found("robot token", id))?;
        // Revoking twice keeps the first time: the audit trail says when access ended.
        robot.revoked_at.get_or_insert(at);
        Ok(())
    }

    async fn delete(&self, id: RobotTokenId) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        if state.robots.remove(&id).is_none() {
            return Err(not_found("robot token", id));
        }
        state.robot_notified.remove(&id);
        Ok(())
    }

    async fn touch_last_used(
        &self,
        id: RobotTokenId,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        // A robot deleted between the request and the flush is not an error: `touch_*` calls
        // are idempotent counters, and the authenticator coalesces them anyway.
        if let Some(robot) = state.robots.get_mut(&id) {
            robot.last_used_at = Some(robot.last_used_at.map_or(at, |old| old.max(at)));
        }
        Ok(())
    }

    async fn purge_expired(&self, before: OffsetDateTime) -> Result<u64, StoreError> {
        let mut state = self.tables()?;
        let doomed: Vec<RobotTokenId> = state
            .robots
            .values()
            .filter(|r| {
                r.expires_at.is_some_and(|at| at < before)
                    || r.revoked_at.is_some_and(|at| at < before)
            })
            .map(|r| r.id)
            .collect();
        for id in &doomed {
            state.robots.remove(id);
            state.robot_notified.remove(id);
        }
        Ok(u64::try_from(doomed.len()).unwrap_or(u64::MAX))
    }

    async fn list_expiring(
        &self,
        within: Duration,
        now: OffsetDateTime,
        limit: u32,
    ) -> Result<Vec<RobotToken>, StoreError> {
        let horizon = add_std(now, within);
        let state = self.tables()?;
        let mut robots: Vec<RobotToken> = state
            .robots
            .values()
            .filter(|r| r.revoked_at.is_none())
            .filter(|r| r.expires_at.is_some_and(|at| at > now && at <= horizon))
            .filter(|r| !state.robot_notified.contains_key(&r.id))
            .cloned()
            .collect();
        robots.sort_by_key(|r| (r.expires_at, r.id));
        robots.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(robots)
    }

    async fn mark_expiry_notified(
        &self,
        id: RobotTokenId,
        at: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let mut state = self.tables()?;
        // Idempotent for a robot that is gone: the notice job can race a delete.
        if state.robots.contains_key(&id) {
            state.robot_notified.entry(id).or_insert(at);
        }
        Ok(())
    }
}
