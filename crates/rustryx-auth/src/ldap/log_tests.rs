//! "Secrets never in logs": every path that logs is driven with recognisable secrets and
//! the captured output is searched for them.

use std::io;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

use super::config::LdapGroupStrategy;
use super::testing::{
    GROUPS_DN, Harness, SERVICE_PASSWORD, base_config, ldap_principal, standard_directory,
};
use super::transport::TransportError;

use rustryx_core::Secret;
use rustryx_core::ports::GroupResolver;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MakeWriter<'_> for Capture {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

fn capture() -> (Capture, tracing::subscriber::DefaultGuard) {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    (capture, tracing::subscriber::set_default(subscriber))
}

const USER_SECRET: &str = "UserPw-9f2c7a";
const WRONG_GUESS: &str = "wrong-guess-pw-777";

#[tokio::test]
async fn must_not_log_passwords_on_any_login_path() {
    let (logs, _guard) = capture();
    let directory = standard_directory().user("carol", USER_SECRET);
    let harness = Harness::new(&base_config(), directory);

    harness.login("carol", USER_SECRET).await.unwrap();
    harness.login("carol", WRONG_GUESS).await.unwrap_err();
    harness.login("nobody", WRONG_GUESS).await.unwrap_err();
    harness.login("carol", "").await.unwrap_err();
    harness.login("carol", "pass\0word").await.unwrap_err();
    harness.fake.knobs().fail_connect = true;
    harness.fake.kill_existing_connections();
    harness.login("carol", USER_SECRET).await.unwrap_err();

    let wrong_service = {
        let mut config = base_config();
        config.bind_password = Some(Secret::new("not-the-service-password"));
        Harness::new(&config, standard_directory())
    };
    wrong_service.login("alice", "alice-pw").await.unwrap_err();

    let text = logs.text();
    assert!(
        text.contains("ldap"),
        "the capture must have recorded the warnings these paths emit: {text:?}"
    );
    for secret in [
        USER_SECRET,
        WRONG_GUESS,
        SERVICE_PASSWORD,
        "not-the-service-password",
        "alice-pw",
    ] {
        assert!(
            !text.contains(secret),
            "{secret:?} leaked into the logs:\n{text}"
        );
    }
}

#[tokio::test]
async fn must_not_log_secrets_on_group_lookup_failure_paths() {
    let (logs, _guard) = capture();
    let mut config = base_config();
    config.group_strategy = LdapGroupStrategy::ReverseMember;
    config.group_base_dn = Some(GROUPS_DN.into());
    let harness = Harness::new(&config, standard_directory());
    let principal = harness.login("alice", "alice-pw").await.unwrap().unwrap();

    harness.fake.knobs().search_error = Some(TransportError::Closed);
    harness.clock.advance(time::Duration::seconds(100));
    harness
        .resolver
        .external_groups(&principal)
        .await
        .unwrap_err();
    harness.clock.advance(time::Duration::seconds(400));
    harness
        .resolver
        .external_groups(&principal)
        .await
        .unwrap_err();
    harness
        .resolver
        .external_groups(&ldap_principal("*)(uid=*"))
        .await
        .unwrap_err();

    let text = logs.text();
    assert!(
        text.contains("group"),
        "the capture must have recorded group warnings: {text:?}"
    );
    for secret in ["alice-pw", SERVICE_PASSWORD] {
        assert!(
            !text.contains(secret),
            "{secret:?} leaked into the logs:\n{text}"
        );
    }
}
