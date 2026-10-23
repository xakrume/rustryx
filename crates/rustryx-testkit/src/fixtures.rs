//! Ready-made entities and helpers for tests.
//!
//! Fixtures are infallible on purpose: a test that passes a bad literal should fail at
//! once, not thread a `Result` through setup code.

use rustryx_core::identity::{AuthMethod, NewUser, Principal, RoleRef, UserSource};
use rustryx_core::ids::{BlobId, BlobStoreId, ProjectId, RepositoryId, RoleId, UserId};
use rustryx_core::model::{
    BlobRecord, BlobStoreDef, BlobStoreKind, Checksums, GroupConfig, NewAsset, ProxyConfig,
    RepoFormat, RepoType, Repository, WritePolicy,
};
use rustryx_core::names::RepoName;
use rustryx_core::ports::{BlobRead, ResourceRef};
use rustryx_core::project::{Project, ProjectName};
use rustryx_core::{BlobError, Secret};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::store::builtin_role_id;
use crate::support::StreamHasher;

/// Id of the blob store every fixture repository points at. `MemStore::new()` is empty;
/// call `MemStore::with_default_blob_store` (or create [`blob_store_def`]) when a test
/// needs the definition to exist.
pub const BLOB_STORE_ID: BlobStoreId =
    BlobStoreId::from_uuid(Uuid::from_u128(0x0192_6f00_0000_7000_8000_0000_0000_0101));

/// Issuer written into OIDC fixture users.
pub const OIDC_ISSUER: &str = "https://idp.example.test/realms/rustryx";

/// Upstream URL of fixture proxy repositories. The `.test` TLD never resolves, so a
/// test that forgets to stub the upstream fails instead of reaching the network.
pub const PROXY_REMOTE_URL: &str = "https://upstream.example.test/";

/// A repository named `name` in the fixture blob store.
///
/// Hosted repositories allow overwrite, proxy and group repositories are read only
/// there. Proxy and group repositories carry the config section their type requires.
///
/// # Panics
/// When `name` is not a valid repository name.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "test fixture: an invalid literal is a bug in the calling test and must fail it at once"
)]
pub fn repository(name: &str, format: RepoFormat, repo_type: RepoType) -> Repository {
    let now = OffsetDateTime::now_utc();
    let mut repository = Repository::new(
        RepositoryId::generate(),
        RepoName::parse(name).expect("fixture repository name must be valid"),
        format,
        repo_type,
        BLOB_STORE_ID,
        now,
    );
    if repo_type != RepoType::Hosted {
        repository.write_policy = WritePolicy::Deny;
    }
    if repo_type == RepoType::Proxy {
        let mut proxy = ProxyConfig::new(PROXY_REMOTE_URL);
        // Metadata freshness differs from the production default on purpose: a test that
        // confuses the two numbers then fails instead of passing by accident.
        proxy.metadata_max_age_secs = 600;
        repository.proxy = Some(proxy);
    }
    if repo_type == RepoType::Group {
        repository.group = Some(GroupConfig::default());
    }
    repository
}

/// A [`ResourceRef`] for `path` in a fixture repository named `name`.
///
/// There is deliberately no constructor that takes the anonymous flag as a loose argument:
/// the flag comes from the repository, which is the bug-proofing the type exists for. A
/// test that needs an anonymous readable resource sets `allow_anonymous` on a fixture
/// repository and calls `ResourceRef::for_repository` itself.
///
/// # Panics
/// When `name` is not a valid repository name.
#[must_use]
pub fn resource(repository_name: &str, format: RepoFormat, path: &str) -> ResourceRef {
    let repository = repository(repository_name, format, RepoType::Hosted);
    ResourceRef::for_repository(&repository, path)
}

/// A private project named `name`, created now, without a creator.
///
/// # Panics
/// When `name` is not a valid project name.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "test fixture: an invalid literal is a bug in the calling test and must fail it at once"
)]
pub fn project(name: &str) -> Project {
    Project::new(
        ProjectId::generate(),
        ProjectName::parse(name).expect("fixture project name must be valid"),
        OffsetDateTime::now_utc(),
    )
}

/// The blob store definition [`BLOB_STORE_ID`] stands for.
#[must_use]
pub fn blob_store_def() -> BlobStoreDef {
    BlobStoreDef::new(
        BLOB_STORE_ID,
        "default",
        BlobStoreKind::Local,
        serde_json::json!({ "path": "/var/lib/rustryx/blobs" }),
        OffsetDateTime::now_utc(),
    )
}

/// Input for a user account.
///
/// OIDC users get [`OIDC_ISSUER`] and the subject `sub-<username>`. No user has a
/// password hash: tests that log in set one through `UserRepo::set_password_hash`.
#[must_use]
pub fn user(username: &str, source: UserSource) -> NewUser {
    let (issuer, subject) = if source == UserSource::Oidc {
        (
            Some(OIDC_ISSUER.to_owned()),
            Some(format!("sub-{username}")),
        )
    } else {
        (None, None)
    };
    NewUser {
        username: username.to_owned(),
        source,
        issuer,
        subject,
        email: Some(format!("{username}@example.test")),
        display_name: Some(username.to_owned()),
        password_hash: None::<Secret>,
    }
}

/// An authenticated local principal holding roles with the given names.
///
/// Role ids are generated, except for the built-in names `nx-admin`, `nx-developer`
/// and `nx-anonymous`: those carry the ids `MemStore::with_builtin_roles` seeds, so the
/// principal's roles resolve against a seeded store.
#[must_use]
pub fn principal_with_roles(roles: &[&str]) -> Principal {
    let mut principal = Principal::new("fixture-user", UserSource::Local, AuthMethod::BasicLocal);
    principal.user_id = Some(UserId::generate());
    principal.roles = roles
        .iter()
        .map(|name| RoleRef {
            id: builtin_role_id(name).unwrap_or_else(RoleId::generate),
            name: (*name).to_owned(),
        })
        .collect();
    principal
}

/// Digests of `data`, computed with the same incremental hasher the blob store uses.
#[must_use]
pub fn checksums_of(data: &[u8]) -> Checksums {
    let mut hasher = StreamHasher::new();
    hasher.update(data);
    hasher.finish().1
}

/// A blob row for `content` under `key`, in the fixture blob store, created now.
#[must_use]
pub fn blob_record(key: &str, content: &[u8]) -> BlobRecord {
    blob_record_at(key, content, OffsetDateTime::now_utc())
}

/// Like [`blob_record`] with an explicit creation time, for garbage collection tests
/// that need blobs of different ages.
#[must_use]
pub fn blob_record_at(key: &str, content: &[u8], created_at: OffsetDateTime) -> BlobRecord {
    BlobRecord {
        id: BlobId::generate(),
        blob_store_id: BLOB_STORE_ID,
        key: key.to_owned(),
        size_bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
        checksums: checksums_of(content),
        created_at,
    }
}

/// A publish of `content` at `path` in `repository`, in the fixture blob store.
///
/// The blob key is unique per call: a store refuses a second blob row with the same
/// `(store, key)`, as the real schema does, so two publishes of one path need two keys.
#[must_use]
pub fn new_asset(repository: RepositoryId, path: &str, content: &[u8]) -> NewAsset {
    NewAsset::new(
        repository,
        path,
        "application/octet-stream",
        blob_record(&format!("key-{}", BlobId::generate()), content),
    )
}

/// Reads a stream to the end, keeping the first error.
///
/// # Errors
/// The first error the stream yields.
pub async fn try_collect_stream(read: BlobRead) -> Result<Vec<u8>, BlobError> {
    use futures::StreamExt;

    let mut stream = read.stream;
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk?);
    }
    Ok(body)
}

/// Reads a stream to the end into one buffer. Fine in tests, forbidden in production
/// code, where bodies are streamed.
///
/// # Panics
/// When the stream yields an error.
#[allow(
    clippy::panic,
    reason = "test helper: a failed read must fail the calling test, not return a truncated body"
)]
pub async fn collect_stream(read: BlobRead) -> Vec<u8> {
    match try_collect_stream(read).await {
        Ok(body) => body,
        Err(error) => panic!("blob stream failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use rustryx_core::identity::{ROLE_NX_ADMIN, ROLE_NX_DEVELOPER};
    use rustryx_core::ports::BlobStore;

    use super::*;
    use crate::{MemBlobStore, MemStore};

    #[test]
    fn repository_carries_the_config_its_type_requires() {
        let hosted = repository("h", RepoFormat::Raw, RepoType::Hosted);
        assert!(hosted.proxy.is_none() && hosted.group.is_none());
        assert_eq!(hosted.write_policy, WritePolicy::Allow);
        let proxy = repository("p", RepoFormat::Npm, RepoType::Proxy);
        assert_eq!(proxy.proxy.unwrap().remote_url, PROXY_REMOTE_URL);
        let group = repository("g", RepoFormat::Maven2, RepoType::Group);
        assert_eq!(group.group.unwrap().members.len(), 0);
        assert_eq!(group.blob_store_id, BLOB_STORE_ID);
        assert_ne!(
            hosted.id,
            repository("h", RepoFormat::Raw, RepoType::Hosted).id
        );
    }

    #[test]
    #[should_panic(expected = "fixture repository name must be valid")]
    fn invalid_repository_name_fails_the_calling_test() {
        let _ = repository("not a name", RepoFormat::Raw, RepoType::Hosted);
    }

    #[test]
    fn users_of_every_source_are_consistent() {
        let local = user("alice", UserSource::Local);
        assert!(local.issuer.is_none() && local.subject.is_none());
        let ldap = user("bob", UserSource::Ldap);
        assert_eq!(ldap.source, UserSource::Ldap);
        let oidc = user("carol", UserSource::Oidc);
        assert_eq!(oidc.issuer.as_deref(), Some(OIDC_ISSUER));
        assert_eq!(oidc.subject.as_deref(), Some("sub-carol"));
        assert!(oidc.password_hash.is_none());
    }

    #[test]
    fn principal_roles_use_given_names_and_builtin_ids_where_known() {
        let principal = principal_with_roles(&[ROLE_NX_ADMIN, "custom", "custom-2"]);
        let names: Vec<&str> = principal.roles.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, [ROLE_NX_ADMIN, "custom", "custom-2"]);
        assert!(principal.has_role(ROLE_NX_ADMIN));
        assert_eq!(
            principal.roles[0].id,
            builtin_role_id(ROLE_NX_ADMIN).unwrap()
        );
        assert_ne!(principal.roles[1].id, principal.roles[2].id);
        assert_eq!(principal_with_roles(&[]).roles.len(), 0);
    }

    #[tokio::test]
    async fn principal_of_builtin_roles_resolves_in_a_seeded_store() {
        use rustryx_core::ports::RoleRepo;

        let store = MemStore::with_builtin_roles();
        let principal = principal_with_roles(&[ROLE_NX_DEVELOPER]);
        let ids: Vec<RoleId> = principal.roles.iter().map(|r| r.id).collect();
        assert_eq!(store.effective_privileges(&ids).await.unwrap().len(), 1);
    }

    #[test]
    fn blob_record_digests_the_content() {
        let record = blob_record("k", b"abc");
        assert_eq!(record.size_bytes, 3);
        assert_eq!(record.checksums.md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(record.blob_store_id, BLOB_STORE_ID);
        let old = blob_record_at("k", b"", OffsetDateTime::UNIX_EPOCH);
        assert_eq!(old.created_at, OffsetDateTime::UNIX_EPOCH);
        assert_eq!(old.size_bytes, 0);
    }

    #[test]
    fn project_and_new_asset_fixtures_are_valid_and_unique() {
        let created = project("payments");
        assert_eq!(created.name.as_str(), "payments");
        assert_eq!(created.version, 1);
        assert_ne!(created.id, project("payments").id);
        let repo = RepositoryId::generate();
        let first = new_asset(repo, "a/b.bin", b"abc");
        let second = new_asset(repo, "a/b.bin", b"abc");
        assert_eq!(first.path, "a/b.bin");
        assert_eq!(first.blob.size_bytes, 3);
        assert_eq!(first.blob.blob_store_id, BLOB_STORE_ID);
        assert_ne!(first.blob.key, second.blob.key, "a key per call");
        assert_eq!(first.blob.checksums, second.blob.checksums);
    }

    #[test]
    #[should_panic(expected = "fixture project name must be valid")]
    fn invalid_project_name_fails_the_calling_test() {
        let _ = project("Not Valid");
    }

    #[test]
    fn resource_fixture_copies_the_flags_of_a_fixture_repository() {
        let resource = resource("maven-hosted", RepoFormat::Maven2, "org/acme/a.jar");
        assert_eq!(resource.repository, "maven-hosted");
        assert_eq!(resource.format, RepoFormat::Maven2);
        assert_eq!(resource.path, "org/acme/a.jar");
        assert!(!resource.allow_anonymous);
        assert!(resource.project.is_none() && resource.coordinate.is_none());
    }

    #[test]
    fn blob_store_def_matches_the_constant() {
        let def = blob_store_def();
        assert_eq!(def.id, BLOB_STORE_ID);
        assert_eq!(def.kind, BlobStoreKind::Local);
    }

    #[tokio::test]
    async fn collect_stream_reassembles_chunks() {
        let store = MemBlobStore::new();
        let key = rustryx_core::ports::BlobKey::new("k");
        let data = vec![9_u8; 20_000];
        store.put(&key, data.clone());
        let read = store.get(&key, None).await.unwrap();
        assert_eq!(collect_stream(read).await, data);
    }

    #[tokio::test]
    #[should_panic(expected = "blob stream failed")]
    async fn collect_stream_panics_on_a_failed_read() {
        use futures::StreamExt;

        let stream = futures::stream::iter(vec![Err(BlobError::NotFound)]).boxed();
        let _ = collect_stream(BlobRead {
            total_size: 0,
            stream,
        })
        .await;
    }
}
