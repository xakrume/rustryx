//! Fixtures for a running [`TestServer`]: repositories, users, roles with privileges and
//! content selectors.
//!
//! The helpers without a `try_` prefix panic when the store refuses (a duplicate name, an
//! unknown role): a test that cannot build its fixture has nothing to assert, and the panic
//! message names the cause. All of them are `async` because the store ports are.

use std::fmt;

use rustryx_core::identity::{
    API_TOKEN_PREFIX, Action, ContentSelector, Privilege, PrivilegeGrant, Role, RoleOrigin,
    UserSource,
};
use rustryx_core::ids::{ContentSelectorId, PrivilegeId, RepositoryId, RoleId};
use rustryx_core::model::{
    GroupConfig, ProxyConfig, RepoFormat, RepoType, Repository, WritePolicy,
};
use rustryx_core::names::RepoName;
use rustryx_testkit::fixtures;
use time::OffsetDateTime;

use crate::auth::generate_secret;
use crate::builder::TestServer;
use crate::error::HarnessError;

/// Browse and read: what a consumer needs.
pub const READ: [Action; 2] = [Action::Browse, Action::Read];
/// Browse, read and write: what a publisher needs.
pub const READ_WRITE: [Action; 3] = [Action::Browse, Action::Read, Action::Write];
/// Every action.
pub const ALL_ACTIONS: [Action; 4] = [Action::Browse, Action::Read, Action::Write, Action::Delete];

/// A repository to create with [`TestServer::add_repository`].
///
/// Hosted repositories allow overwrite; proxy and group repositories are created with
/// `WritePolicy::Deny`, the way the testkit fixtures do (the write policy is only evaluated
/// for hosted repositories, the system fills proxy caches).
#[derive(Debug, Clone)]
#[must_use]
pub struct NewTestRepo {
    name: String,
    format: RepoFormat,
    repo_type: RepoType,
    remote_url: Option<String>,
    members: Vec<String>,
    allow_anonymous: bool,
    write_policy: Option<WritePolicy>,
    proxy_config: Option<ProxyConfig>,
    online: bool,
    quota_bytes: Option<u64>,
    format_config: Option<serde_json::Value>,
}

impl NewTestRepo {
    fn new(name: &str, format: RepoFormat, repo_type: RepoType) -> Self {
        Self {
            name: name.to_owned(),
            format,
            repo_type,
            remote_url: None,
            members: Vec::new(),
            allow_anonymous: false,
            write_policy: None,
            proxy_config: None,
            online: true,
            quota_bytes: None,
            format_config: None,
        }
    }

    /// A hosted repository.
    pub fn hosted(name: &str, format: RepoFormat) -> Self {
        Self::new(name, format, RepoType::Hosted)
    }

    /// A proxy repository of `remote_url` (for a wiremock upstream: `UpstreamStub::url()`).
    pub fn proxy(name: &str, format: RepoFormat, remote_url: &str) -> Self {
        let mut new = Self::new(name, format, RepoType::Proxy);
        new.remote_url = Some(remote_url.to_owned());
        new
    }

    /// A group repository over `members`, in resolution order.
    pub fn group(name: &str, format: RepoFormat, members: &[&str]) -> Self {
        let mut new = Self::new(name, format, RepoType::Group);
        new.members = members.iter().map(|m| (*m).to_owned()).collect();
        new
    }

    /// The repository's own anonymous flag (anonymous access also needs the global switch).
    pub fn allow_anonymous(mut self, allow: bool) -> Self {
        self.allow_anonymous = allow;
        self
    }

    /// The write policy (`Allow`, `AllowOnce`, `Deny`).
    pub fn write_policy(mut self, policy: WritePolicy) -> Self {
        self.write_policy = Some(policy);
        self
    }

    /// Replaces the whole proxy configuration, `remote_url` included.
    pub fn proxy_config(mut self, config: ProxyConfig) -> Self {
        self.proxy_config = Some(config);
        self
    }

    /// `false` creates an offline repository (reads answer 503).
    pub fn online(mut self, online: bool) -> Self {
        self.online = online;
        self
    }

    /// A size limit in bytes for the repository.
    pub fn quota_bytes(mut self, bytes: u64) -> Self {
        self.quota_bytes = Some(bytes);
        self
    }

    /// Format specific settings (the JSON object of `Repository::format_config`).
    pub fn format_config(mut self, config: serde_json::Value) -> Self {
        self.format_config = Some(config);
        self
    }

    fn build(self) -> Result<Repository, HarnessError> {
        let name = parse_name(&self.name)?;
        let mut repository = Repository::new(
            RepositoryId::generate(),
            name,
            self.format,
            self.repo_type,
            fixtures::BLOB_STORE_ID,
            OffsetDateTime::now_utc(),
        );
        repository.allow_anonymous = self.allow_anonymous;
        repository.online = self.online;
        repository.quota_bytes = self.quota_bytes;
        if let Some(config) = self.format_config {
            repository.format_config = config;
        }
        repository.write_policy =
            self.write_policy
                .unwrap_or(if self.repo_type == RepoType::Hosted {
                    WritePolicy::Allow
                } else {
                    WritePolicy::Deny
                });
        match self.repo_type {
            RepoType::Proxy => {
                repository.proxy = Some(
                    self.proxy_config
                        .unwrap_or_else(|| ProxyConfig::new(self.remote_url.unwrap_or_default())),
                );
            }
            RepoType::Group => {
                let members = self
                    .members
                    .iter()
                    .map(|member| parse_name(member))
                    .collect::<Result<Vec<_>, _>>()?;
                repository.group = Some(GroupConfig { members });
            }
            RepoType::Hosted => {}
        }
        Ok(repository)
    }
}

fn parse_name(name: &str) -> Result<RepoName, HarnessError> {
    RepoName::parse(name).map_err(|error| HarnessError::InvalidRepositoryName {
        name: name.to_owned(),
        reason: error.to_string(),
    })
}

/// A user created by [`TestServer::add_user`].
///
/// The secrets are redacted in `Debug`; read the fields to use them.
#[derive(Clone, PartialEq, Eq)]
pub struct TestUser {
    /// Login name.
    pub username: String,
    /// The account password, for Basic authentication.
    pub password: String,
    /// An API token (`ryx_...`). It works as `Authorization: Bearer <token>` and as the Basic
    /// password with any user name, which is how maven, npm, pip and pub clients carry it.
    pub token: String,
}

impl TestUser {
    /// `Basic base64(username:password)`, the value of an `Authorization` header.
    #[must_use]
    pub fn basic_header(&self) -> String {
        basic_value(&self.username, &self.password)
    }

    /// `Basic base64(username:token)`: the API token in the password position.
    #[must_use]
    pub fn token_basic_header(&self) -> String {
        basic_value(&self.username, &self.token)
    }

    /// `Bearer <token>`.
    #[must_use]
    pub fn bearer_header(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

fn basic_value(user: &str, password: &str) -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

impl fmt::Debug for TestUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestUser")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// One privilege of a role created by [`TestServer::add_role_with_privileges`].
#[derive(Debug, Clone)]
pub struct TestPrivilege {
    grant: PrivilegeGrant,
    actions: Vec<Action>,
}

impl TestPrivilege {
    /// `actions` on `repository` (or `*`), whatever its format.
    #[must_use]
    pub fn repository_view(repository: &str, actions: &[Action]) -> Self {
        Self {
            grant: PrivilegeGrant::RepositoryView {
                format: None,
                repository: repository.to_owned(),
            },
            actions: actions.to_vec(),
        }
    }

    /// `actions` on `repository` (or `*`), only for repositories of `format`.
    #[must_use]
    pub fn repository_view_of_format(
        format: RepoFormat,
        repository: &str,
        actions: &[Action],
    ) -> Self {
        Self {
            grant: PrivilegeGrant::RepositoryView {
                format: Some(format),
                repository: repository.to_owned(),
            },
            actions: actions.to_vec(),
        }
    }

    /// `actions` on the part of `repository` (or `*`) that `selector` matches.
    #[must_use]
    pub fn content_selector(
        repository: &str,
        selector: &ContentSelector,
        actions: &[Action],
    ) -> Self {
        Self {
            grant: PrivilegeGrant::RepositoryContentSelector {
                repository: repository.to_owned(),
                selector_id: selector.id,
            },
            actions: actions.to_vec(),
        }
    }
}

impl TestServer {
    /// Creates a repository and returns it as stored.
    ///
    /// # Panics
    /// On an invalid or duplicate name; see [`TestServer::try_add_repository`].
    #[allow(
        clippy::expect_used,
        reason = "test harness: a fixture that cannot be built must fail the calling test with the cause"
    )]
    pub async fn add_repository(&self, new: NewTestRepo) -> Repository {
        self.try_add_repository(new)
            .await
            .expect("add_repository failed")
    }

    /// Creates a repository, returning the reason when it cannot.
    ///
    /// # Errors
    /// [`HarnessError::InvalidRepositoryName`] or [`HarnessError::Store`] (a duplicate name).
    pub async fn try_add_repository(&self, new: NewTestRepo) -> Result<Repository, HarnessError> {
        let repository = new.build()?;
        self.repositories.create(&repository).await?;
        // Read back: the store applies project and visibility rules, so the stored row is the
        // truth, not the value that was sent.
        let stored = self.repositories.get(repository.id).await?;
        Ok(stored.unwrap_or(repository))
    }

    /// Creates a local user holding `roles` (by name: the built-in `nx-admin`,
    /// `nx-developer`, `nx-anonymous`, or a role from [`TestServer::add_role_with_privileges`]).
    ///
    /// # Panics
    /// On a duplicate user name or an unknown role; see [`TestServer::try_add_user`].
    #[allow(
        clippy::expect_used,
        reason = "test harness: a fixture that cannot be built must fail the calling test with the cause"
    )]
    pub async fn add_user(&self, username: &str, roles: &[&str]) -> TestUser {
        self.try_add_user(username, roles)
            .await
            .expect("add_user failed")
    }

    /// Creates a user, returning the reason when it cannot.
    ///
    /// # Errors
    /// [`HarnessError::UnknownRole`] or [`HarnessError::Store`].
    pub async fn try_add_user(
        &self,
        username: &str,
        roles: &[&str],
    ) -> Result<TestUser, HarnessError> {
        let role_ids = self.resolve_roles(roles).await?;
        let created = self
            .users
            .create(&fixtures::user(username, UserSource::Local))
            .await?;
        self.users
            .replace_roles_for_origin(created.id, RoleOrigin::Manual, &role_ids)
            .await?;
        let user = TestUser {
            username: username.to_owned(),
            password: generate_secret(""),
            token: generate_secret(API_TOKEN_PREFIX),
        };
        self.auth
            .register_user(username, created.id, &user.password, &user.token);
        Ok(user)
    }

    /// Replaces the roles of an existing user. The next request sees them.
    ///
    /// # Panics
    /// On an unknown user or role.
    #[allow(
        clippy::expect_used,
        reason = "test harness: a fixture that cannot be built must fail the calling test with the cause"
    )]
    pub async fn set_user_roles(&self, user: &TestUser, roles: &[&str]) {
        let role_ids = self
            .resolve_roles(roles)
            .await
            .expect("set_user_roles: unknown role");
        let found = self
            .users
            .find_by_username(&user.username)
            .await
            .expect("set_user_roles: store failed")
            .expect("set_user_roles: unknown user");
        self.users
            .replace_roles_for_origin(found.id, RoleOrigin::Manual, &role_ids)
            .await
            .expect("set_user_roles: store refused");
    }

    async fn resolve_roles(&self, roles: &[&str]) -> Result<Vec<RoleId>, HarnessError> {
        let mut ids = Vec::with_capacity(roles.len());
        for name in roles {
            let role = self
                .roles
                .get_by_name(name)
                .await?
                .ok_or_else(|| HarnessError::UnknownRole((*name).to_owned()))?;
            ids.push(role.id);
        }
        Ok(ids)
    }

    /// An extra API token for `user`, restricted to `scopes` (`repo:<name>:<action>`, `*`).
    /// A scope list can only narrow what the owner may do.
    ///
    /// # Panics
    /// When the user was not created by this server.
    #[allow(
        clippy::expect_used,
        reason = "test harness: a fixture that cannot be built must fail the calling test with the cause"
    )]
    #[must_use]
    pub fn add_scoped_token(&self, user: &TestUser, scopes: &[&str]) -> String {
        let scopes: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
        self.auth
            .issue_scoped_token(&user.username, &scopes)
            .expect("add_scoped_token: the user does not exist on this server")
    }

    /// Creates a content selector. The expression is compiled first, like the admin API does
    /// (SEC-18), so a typo fails here and not as a silent no-match later.
    ///
    /// # Panics
    /// On an expression that does not compile or a duplicate name.
    #[allow(
        clippy::expect_used,
        reason = "test harness: a fixture that cannot be built must fail the calling test with the cause"
    )]
    pub async fn add_selector(&self, name: &str, expression: &str) -> ContentSelector {
        self.try_add_selector(name, expression)
            .await
            .expect("add_selector failed")
    }

    /// Creates a content selector, returning the reason when it cannot.
    ///
    /// # Errors
    /// [`HarnessError::InvalidSelector`] or [`HarnessError::Store`].
    pub async fn try_add_selector(
        &self,
        name: &str,
        expression: &str,
    ) -> Result<ContentSelector, HarnessError> {
        self.rbac
            .validate_selector(expression)
            .map_err(|error| HarnessError::InvalidSelector(error.to_string()))?;
        let selector = ContentSelector {
            id: ContentSelectorId::generate(),
            name: name.to_owned(),
            description: String::new(),
            expression: expression.to_owned(),
            updated_at: OffsetDateTime::now_utc(),
        };
        self.selectors.create(&selector).await?;
        Ok(selector)
    }

    /// Creates a role holding `privileges`.
    ///
    /// # Panics
    /// On a duplicate role name.
    #[allow(
        clippy::expect_used,
        reason = "test harness: a fixture that cannot be built must fail the calling test with the cause"
    )]
    pub async fn add_role_with_privileges(&self, name: &str, privileges: &[TestPrivilege]) -> Role {
        self.try_add_role_with_privileges(name, privileges)
            .await
            .expect("add_role_with_privileges failed")
    }

    /// Creates a role, returning the reason when it cannot.
    ///
    /// # Errors
    /// [`HarnessError::Store`] (a duplicate role or privilege name).
    pub async fn try_add_role_with_privileges(
        &self,
        name: &str,
        privileges: &[TestPrivilege],
    ) -> Result<Role, HarnessError> {
        let mut privilege_ids = Vec::with_capacity(privileges.len());
        for (index, wanted) in privileges.iter().enumerate() {
            let privilege = Privilege {
                id: PrivilegeId::generate(),
                name: format!("{name}-privilege-{index}"),
                description: String::new(),
                builtin: false,
                grant: wanted.grant.clone(),
                actions: wanted.actions.clone(),
            };
            self.privileges.create(&privilege).await?;
            privilege_ids.push(privilege.id);
        }
        let role = Role {
            id: RoleId::generate(),
            name: name.to_owned(),
            description: String::new(),
            builtin: false,
            privilege_ids,
            nested_role_ids: Vec::new(),
        };
        self.roles.create(&role).await?;
        Ok(role)
    }
}
