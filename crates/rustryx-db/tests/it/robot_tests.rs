//! Robot tokens: unique name and hash, a missing project, and listing by project.

use rustryx_core::StoreError;
use rustryx_core::ids::{ProjectId, RobotTokenId};
use rustryx_core::ports::{PageRequest, RobotFilter, RobotTokenRepo};
use rustryx_core::robot::RobotToken;
use rustryx_db::repos::robot::PgRobotTokenRepo;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::support::TestDb;

fn hash_of(byte: char) -> String {
    byte.to_string().repeat(64)
}

fn robot(name: &str, hash: &str) -> RobotToken {
    RobotToken::new(
        RobotTokenId::generate(),
        name.to_owned(),
        name.to_owned(),
        vec!["repo:*:browse".to_owned(), "repo:*:read".to_owned()],
        hash.to_owned(),
        "rx_pfx".to_owned(),
        OffsetDateTime::UNIX_EPOCH,
    )
}

async fn project(pool: &sqlx::PgPool) -> ProjectId {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, 'robots')")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    ProjectId::from_uuid(id)
}

#[tokio::test(flavor = "multi_thread")]
async fn robot_tokens_are_unique_by_name_and_hash_and_listed_by_project() {
    let db = TestDb::new().await;
    let pool = db.pool().await;
    let project_id = project(&pool).await;
    let repo = PgRobotTokenRepo::new(pool);
    let first = robot("ci", &hash_of('a'));
    repo.create(&first).await.unwrap();
    for clash in [robot("ci", &hash_of('b')), robot("other", &hash_of('a'))] {
        let error = repo.create(&clash).await.unwrap_err();
        assert!(
            matches!(error, StoreError::Conflict(ref text) if text.contains("name or secret")),
            "{error}"
        );
    }
    let mut orphan = robot("orphan", &hash_of('c'));
    orphan.project_id = Some(ProjectId::generate());
    let missing = repo.create(&orphan).await.unwrap_err();
    assert!(
        matches!(missing, StoreError::Conflict(ref text) if text.contains("does not exist")),
        "{missing}"
    );
    let mut scoped = robot("scoped", &hash_of('d'));
    scoped.project_id = Some(project_id);
    repo.create(&scoped).await.unwrap();

    assert_eq!(
        repo.find_by_hash(&hash_of('a'))
            .await
            .unwrap()
            .map(|row| row.id),
        Some(first.id)
    );
    assert!(repo.find_by_hash(&hash_of('e')).await.unwrap().is_none());
    assert_eq!(
        repo.get(scoped.id).await.unwrap().map(|row| row.name),
        Some("scoped".to_owned())
    );

    let page = PageRequest {
        limit: 10,
        after: None,
    };
    let only_project = RobotFilter {
        project: Some(project_id),
        ..RobotFilter::default()
    };
    let names = repo
        .list(&only_project, &page)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|row| row.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["scoped"]);
    let system = RobotFilter {
        system_only: true,
        ..RobotFilter::default()
    };
    let system_names = repo
        .list(&system, &page)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|row| row.name)
        .collect::<Vec<_>>();
    assert_eq!(system_names, ["ci"]);
}
