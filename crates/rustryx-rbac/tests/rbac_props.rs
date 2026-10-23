//! Property tests of the authorization invariants: whatever privileges, scopes
//! and resources are generated, the rules that must always hold do hold.

// Tests may unwrap and expect: a panic is the failure signal there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Env, open_resource, principal, selector_grant, view};
use proptest::prelude::*;
use rustryx_core::identity::{Action, Principal, PrivilegeGrant, ROLE_NX_ANONYMOUS};
use rustryx_core::ids::{ContentSelectorId, PrivilegeId, RoleId};
use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::{Decision, RbacChecker, ResourceRef};
use rustryx_rbac::RbacConfig;

const ACTIONS: [Action; 4] = [Action::Browse, Action::Read, Action::Write, Action::Delete];

#[derive(Debug, Clone)]
enum GrantSpec {
    Admin,
    View(&'static str, Option<RepoFormat>),
    Selector(&'static str),
}

#[derive(Debug, Clone)]
struct Spec {
    grant: GrantSpec,
    actions: Vec<Action>,
}

fn spec() -> impl Strategy<Value = Spec> {
    let grant = prop_oneof![
        Just(GrantSpec::Admin),
        (
            prop::sample::select(vec!["*", "a", "b"]),
            prop::option::of(prop::sample::select(vec![
                RepoFormat::Raw,
                RepoFormat::Maven2
            ])),
        )
            .prop_map(|(repo, format)| GrantSpec::View(repo, format)),
        prop::sample::select(vec!["*", "a", "b"]).prop_map(GrantSpec::Selector),
    ];
    (grant, prop::sample::subsequence(ACTIONS.to_vec(), 0..=4))
        .prop_map(|(grant, actions)| Spec { grant, actions })
}

/// Every generated resource is open to anonymous callers, so that the properties about
/// anonymous callers test the privileges and not the repository flag (which has its own
/// tests in `rbac_anonymous.rs`).
fn resources() -> Vec<ResourceRef> {
    let mut all = Vec::new();
    for repository in ["a", "b", "c"] {
        for format in [RepoFormat::Raw, RepoFormat::Maven2] {
            for path in ["/a/x", "/b/y", "/"] {
                all.push(open_resource(repository, format, path));
            }
        }
    }
    all
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

/// Stores the specs as privileges of one role and returns the role id.
async fn role_with(env: &Env, name: &str, specs: &[Spec]) -> RoleId {
    // Selectors start with "/a", so they match some of the generated paths only.
    let selector: ContentSelectorId = env
        .selector(&format!("{name}-sel"), "path.startsWith(\"/a\")")
        .await;
    let mut ids: Vec<PrivilegeId> = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        let grant = match &spec.grant {
            GrantSpec::Admin => PrivilegeGrant::Admin,
            GrantSpec::View(repo, format) => view(repo, *format),
            GrantSpec::Selector(repo) => selector_grant(repo, selector),
        };
        ids.push(
            env.privilege(&format!("{name}-p{i}"), grant, &spec.actions)
                .await,
        );
    }
    env.role(name, &ids, &[]).await
}

async fn decisions(env: &Env, p: &Principal) -> Vec<(usize, Action, bool)> {
    let mut out = Vec::new();
    for (i, res) in resources().iter().enumerate() {
        for action in ACTIONS {
            let decision = env.rbac.check(p, action, res).await.unwrap();
            out.push((i, action, decision == Decision::Allow));
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Whatever nx-anonymous is granted, anonymous write and delete are denied.
    #[test]
    fn anonymous_never_gets_write_or_delete(specs in prop::collection::vec(spec(), 0..6)) {
        let leaked = runtime().block_on(async {
            let env = Env::bare();
            role_with(&env, ROLE_NX_ANONYMOUS, &specs).await;
            decisions(&env, &Principal::anonymous())
                .await
                .into_iter()
                .any(|(_, action, allow)| allow && matches!(action, Action::Write | Action::Delete))
        });
        prop_assert!(!leaked);
    }

    /// Anonymous access needs the repository flag AND the global switch, whatever nx-anonymous
    /// is granted: with either gate closed nothing is allowed, for any action.
    #[test]
    fn anonymous_needs_both_gates(
        specs in prop::collection::vec(spec(), 0..6),
        switch in any::<bool>(),
        flag in any::<bool>(),
    ) {
        let allowed_any = runtime().block_on(async {
            let config = RbacConfig { anonymous_enabled: switch, ..RbacConfig::default() };
            let env = Env::bare_with_config(&config);
            role_with(&env, ROLE_NX_ANONYMOUS, &specs).await;
            let anonymous = Principal::anonymous();
            let mut allowed_any = false;
            for mut res in resources() {
                res.allow_anonymous = flag;
                for action in ACTIONS {
                    allowed_any |=
                        env.rbac.check(&anonymous, action, &res).await.unwrap() == Decision::Allow;
                }
            }
            allowed_any
        });
        prop_assert!(!allowed_any || (switch && flag));
    }

    /// Anonymous callers never receive more than the same role would give a
    /// real user, and an Admin privilege adds nothing for them.
    #[test]
    fn anonymous_is_never_wider_than_the_role_it_stands_for(specs in prop::collection::vec(spec(), 0..6)) {
        let (anonymous, without_admin) = runtime().block_on(async {
            let env = Env::bare();
            role_with(&env, ROLE_NX_ANONYMOUS, &specs).await;
            let anonymous = decisions(&env, &Principal::anonymous()).await;
            let rest: Vec<Spec> = specs
                .iter()
                .filter(|s| !matches!(s.grant, GrantSpec::Admin))
                .cloned()
                .collect();
            let other = role_with(&env, "same-without-admin", &rest).await;
            let user = decisions(&env, &principal(&[(other, "same-without-admin")])).await;
            (anonymous, user)
        });
        for ((_, action, anon), (_, _, user)) in anonymous.iter().zip(&without_admin) {
            // Anonymous may only be narrower: no Admin, no write, no delete.
            prop_assert!(!anon || *user, "{action:?}");
        }
    }

    /// A token scope list can only remove rights.
    #[test]
    fn token_scopes_never_extend_rights(
        specs in prop::collection::vec(spec(), 0..6),
        scopes in prop::collection::vec(
            prop::sample::select(vec![
                "*", "repo:a:read", "repo:*:write", "repo:b:*", "repo:*:*", "repo:c:delete",
                "garbage", "repo:a:", "repo::read", "REPO:a:read", "repo:a:browse",
            ]),
            0..4,
        ),
    ) {
        let (plain, scoped) = runtime().block_on(async {
            let env = Env::bare();
            let role = role_with(&env, "owner", &specs).await;
            let owner = principal(&[(role, "owner")]);
            let plain = decisions(&env, &owner).await;
            let mut token = owner.clone();
            token.token_scopes = Some(scopes.iter().map(|s| (*s).to_owned()).collect());
            (plain, decisions(&env, &token).await)
        });
        for ((_, action, without), (_, _, with)) in plain.iter().zip(&scoped) {
            prop_assert!(!with || *without, "scopes widened {action:?}");
        }
    }

    /// Privileges only allow: giving a role more privileges never removes access.
    #[test]
    fn more_privileges_never_reduce_access(
        base in prop::collection::vec(spec(), 0..4),
        extra in prop::collection::vec(spec(), 0..4),
    ) {
        let (small, large) = runtime().block_on(async {
            let env = Env::bare();
            let small_role = role_with(&env, "small", &base).await;
            let all: Vec<Spec> = base.iter().chain(&extra).cloned().collect();
            let large_role = role_with(&env, "large", &all).await;
            (
                decisions(&env, &principal(&[(small_role, "small")])).await,
                decisions(&env, &principal(&[(large_role, "large")])).await,
            )
        });
        for ((_, action, a), (_, _, b)) in small.iter().zip(&large) {
            prop_assert!(!a || *b, "access lost for {action:?}");
        }
    }

    /// A user with several roles may do exactly what the union of the roles may do.
    #[test]
    fn several_roles_are_the_union_of_their_privileges(
        first in prop::collection::vec(spec(), 0..4),
        second in prop::collection::vec(spec(), 0..4),
    ) {
        let (one, two, both) = runtime().block_on(async {
            let env = Env::bare();
            let r1 = role_with(&env, "r1", &first).await;
            let r2 = role_with(&env, "r2", &second).await;
            (
                decisions(&env, &principal(&[(r1, "r1")])).await,
                decisions(&env, &principal(&[(r2, "r2")])).await,
                decisions(&env, &principal(&[(r1, "r1"), (r2, "r2")])).await,
            )
        });
        for (((_, action, a), (_, _, b)), (_, _, union)) in one.iter().zip(&two).zip(&both) {
            prop_assert_eq!(*union, *a || *b, "{:?}", action);
        }
    }
}
