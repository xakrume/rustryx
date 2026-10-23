//! Tests of `LdapConfig` validation.

use std::path::PathBuf;
use std::time::Duration;

use rustryx_core::Secret;

use super::config::{LdapConfig, LdapConfigError, LdapGroupStrategy, MAX_GROUP_STALENESS};
use super::filter::FilterTemplateError;

fn valid() -> LdapConfig {
    LdapConfig {
        enabled: true,
        url: Some("ldaps://ldap.example.org".into()),
        bind_dn: Some("cn=svc,dc=example,dc=org".into()),
        bind_password: Some(Secret::new("s3cret-service-pw")),
        user_base_dn: Some("ou=people,dc=example,dc=org".into()),
        ..LdapConfig::default()
    }
}

fn with_url(url: &str) -> LdapConfig {
    LdapConfig {
        url: Some(url.into()),
        ..valid()
    }
}

#[test]
fn accepts_ldaps_and_ldap_with_starttls() {
    assert_eq!(valid().validate(), Ok(()));
    let starttls = LdapConfig {
        starttls: true,
        ..with_url("ldap://ldap.example.org:389")
    };
    assert_eq!(starttls.validate(), Ok(()));
}

#[test]
fn must_not_accept_plain_ldap_without_starttls() {
    let plain = with_url("ldap://ldap.example.org");
    assert_eq!(plain.validate(), Err(LdapConfigError::PlaintextNotAllowed));
    assert!(plain.settings().is_err());
}

#[test]
fn must_not_accept_plain_ldap_in_a_disabled_section_either() {
    let plain = LdapConfig {
        enabled: false,
        url: Some("ldap://ldap.example.org".into()),
        ..LdapConfig::default()
    };
    assert_eq!(plain.validate(), Err(LdapConfigError::PlaintextNotAllowed));
}

#[test]
fn a_disabled_section_without_a_url_is_valid() {
    assert_eq!(LdapConfig::default().validate(), Ok(()));
    let blank = LdapConfig {
        url: Some("  ".into()),
        ..LdapConfig::default()
    };
    assert_eq!(blank.validate(), Ok(()));
}

#[test]
fn an_enabled_section_needs_a_url() {
    let config = LdapConfig {
        url: None,
        ..valid()
    };
    assert_eq!(config.validate(), Err(LdapConfigError::Empty("url")));
}

#[test]
fn must_not_accept_a_plaintext_flag_in_the_file() {
    for key in [
        "allow_plaintext",
        "insecure",
        "tls_verify",
        "no_tls_verify",
        "plaintext",
        "skip_tls_verify",
    ] {
        let json = format!(r#"{{"enabled": true, "url": "ldap://h", "{key}": true}}"#);
        let parsed: Result<LdapConfig, _> = serde_json::from_str(&json);
        assert!(parsed.is_err(), "`{key}` must be an unknown field");
    }
}

#[test]
fn must_not_accept_ldaps_with_the_starttls_flag() {
    let both = LdapConfig {
        starttls: true,
        ..valid()
    };
    assert_eq!(both.validate(), Err(LdapConfigError::StartTlsWithLdaps));
}

#[test]
fn rejects_other_schemes_and_malformed_urls() {
    for (url, expected) in [
        (
            "ldapi:///var/run/ldapi",
            LdapConfigError::UnsupportedScheme("ldapi".into()),
        ),
        (
            "http://ldap.example.org",
            LdapConfigError::UnsupportedScheme("http".into()),
        ),
        ("not a url", LdapConfigError::InvalidUrl),
        ("", LdapConfigError::Empty("url")),
        ("ldaps://", LdapConfigError::MissingHost),
        (
            "ldaps://host/dc=example",
            LdapConfigError::UnexpectedUrlParts,
        ),
        ("ldaps://host?x=1", LdapConfigError::UnexpectedUrlParts),
        ("ldaps://host#frag", LdapConfigError::UnexpectedUrlParts),
    ] {
        assert_eq!(with_url(url).validate(), Err(expected), "{url}");
    }
}

#[test]
fn must_not_echo_credentials_found_in_the_url() {
    let error = with_url("ldaps://svc:hunter2@ldap.example.org")
        .validate()
        .unwrap_err();
    assert_eq!(error, LdapConfigError::UnexpectedUrlParts);
    assert!(!error.to_string().contains("hunter2"));
    assert!(!format!("{error:?}").contains("hunter2"));
}

#[test]
fn must_not_accept_an_empty_service_password() {
    for password in [None, Some(Secret::new("")), Some(Secret::new("   "))] {
        let config = LdapConfig {
            bind_password: password,
            ..valid()
        };
        assert_eq!(
            config.validate(),
            Err(LdapConfigError::Empty("bind_password"))
        );
    }
}

#[test]
fn must_not_leak_the_bind_password_in_debug() {
    let config = valid();
    assert!(!format!("{config:?}").contains("s3cret-service-pw"));
    let settings = config.settings().unwrap();
    assert!(!format!("{settings:?}").contains("s3cret-service-pw"));
}

#[test]
fn requires_dns_and_attribute_names() {
    let cases = [
        (
            LdapConfig {
                bind_dn: Some(" ".into()),
                ..valid()
            },
            LdapConfigError::Empty("bind_dn"),
        ),
        (
            LdapConfig {
                bind_dn: None,
                ..valid()
            },
            LdapConfigError::Empty("bind_dn"),
        ),
        (
            LdapConfig {
                user_base_dn: None,
                ..valid()
            },
            LdapConfigError::Empty("user_base_dn"),
        ),
        (
            LdapConfig {
                user_base_dn: Some("ou=a\0".into()),
                ..valid()
            },
            LdapConfigError::Invalid {
                field: "user_base_dn",
                reason: "contains a NUL byte",
            },
        ),
        (
            LdapConfig {
                username_attribute: String::new(),
                ..valid()
            },
            LdapConfigError::Empty("username_attribute"),
        ),
        (
            LdapConfig {
                username_attribute: "uid)(x".into(),
                ..valid()
            },
            LdapConfigError::Invalid {
                field: "username_attribute",
                reason: "is not a valid attribute name",
            },
        ),
        (
            LdapConfig {
                group_attribute: "-memberOf".into(),
                ..valid()
            },
            LdapConfigError::Invalid {
                field: "group_attribute",
                reason: "is not a valid attribute name",
            },
        ),
    ];
    for (config, expected) in cases {
        assert_eq!(config.validate(), Err(expected));
    }
}

#[test]
fn validates_the_search_filter_template() {
    let missing = LdapConfig {
        search_filter: "(uid=alice)".into(),
        ..valid()
    };
    assert_eq!(
        missing.validate(),
        Err(LdapConfigError::Filter {
            field: "search_filter",
            source: FilterTemplateError::MissingPlaceholder("{username}".into()),
        })
    );
    let broken = LdapConfig {
        search_filter: "(uid={username}".into(),
        ..valid()
    };
    assert!(matches!(
        broken.validate(),
        Err(LdapConfigError::Filter { .. })
    ));
    assert!(
        missing
            .validate()
            .unwrap_err()
            .to_string()
            .contains("search_filter")
    );
}

#[test]
fn defaults_match_the_specification() {
    let settings = valid().settings().unwrap();
    assert_eq!(settings.timeouts.connect, Duration::from_secs(5));
    assert_eq!(settings.timeouts.bind, Duration::from_secs(5));
    assert_eq!(settings.timeouts.search, Duration::from_secs(5));
    assert_eq!(settings.groups.max_depth, 5);
    assert_eq!(settings.groups.strategy, LdapGroupStrategy::MemberOf);
    assert!(settings.groups.nested);
    assert!(!settings.groups.stale_on_error, "no grace unless asked for");
    assert!(settings.groups.cache_ttl <= MAX_GROUP_STALENESS);
    assert_eq!(MAX_GROUP_STALENESS, Duration::from_mins(5));
    assert_eq!(settings.pool_size, 4);
    assert_eq!(settings.pool_max_lifetime, Duration::from_mins(5));
    assert_eq!(settings.max_user_binds, 16);
    assert!(settings.endpoint.url.as_str().starts_with("ldaps://"));
}

#[test]
fn enforces_numeric_bounds() {
    let cases: Vec<LdapConfig> = vec![
        LdapConfig {
            connect_timeout_secs: 0,
            ..valid()
        },
        LdapConfig {
            bind_timeout_secs: 0,
            ..valid()
        },
        LdapConfig {
            search_timeout_secs: 301,
            ..valid()
        },
        LdapConfig {
            auth_deadline_secs: 601,
            ..valid()
        },
        LdapConfig {
            group_deadline_secs: 0,
            ..valid()
        },
        LdapConfig {
            pool_size: 0,
            ..valid()
        },
        LdapConfig {
            pool_size: 65,
            ..valid()
        },
        LdapConfig {
            pool_max_lifetime_secs: 0,
            ..valid()
        },
        LdapConfig {
            max_concurrent_user_binds: 0,
            ..valid()
        },
        LdapConfig {
            max_concurrent_user_binds: 257,
            ..valid()
        },
        LdapConfig {
            nested_group_depth: 0,
            ..valid()
        },
        LdapConfig {
            nested_group_depth: 17,
            ..valid()
        },
        LdapConfig {
            max_groups: 0,
            ..valid()
        },
        LdapConfig {
            max_groups: 10_001,
            ..valid()
        },
        LdapConfig {
            max_login_length: 0,
            ..valid()
        },
        LdapConfig {
            max_login_length: 1025,
            ..valid()
        },
    ];
    for config in cases {
        assert!(
            matches!(config.validate(), Err(LdapConfigError::Invalid { .. })),
            "{config:?}"
        );
    }
}

#[test]
fn must_not_allow_a_group_cache_longer_than_five_minutes() {
    let mut config = valid();
    config.groups_cache_ttl_secs = 300;
    assert_eq!(config.validate(), Ok(()));
    config.groups_cache_ttl_secs = 301;
    assert_eq!(
        config.validate(),
        Err(LdapConfigError::Invalid {
            field: "groups_cache_ttl_secs",
            reason: "is out of the allowed range",
        })
    );
}

#[test]
fn reverse_member_needs_a_group_base_and_valid_filters() {
    let mut config = valid();
    config.group_strategy = LdapGroupStrategy::ReverseMember;
    assert_eq!(
        config.validate(),
        Err(LdapConfigError::Empty("group_base_dn"))
    );
    config.group_base_dn = Some("ou=groups,dc=example,dc=org".into());
    assert_eq!(config.validate(), Ok(()));

    config.group_filter = "(memberUid={username})".into();
    assert_eq!(config.validate(), Ok(()));

    config.group_filter = "(member=static)".into();
    assert!(matches!(
        config.validate(),
        Err(LdapConfigError::Filter {
            field: "group_filter",
            ..
        })
    ));

    config.group_filter = "(member={dn})".into();
    config.nested_group_filter = "(memberUid={username})".into();
    assert!(matches!(
        config.validate(),
        Err(LdapConfigError::Filter {
            field: "nested_group_filter",
            ..
        })
    ));
}

#[test]
fn member_of_does_not_need_a_group_base() {
    let settings = valid().settings().unwrap();
    assert_eq!(settings.groups.group_base_dn, "");
}

#[test]
fn rejects_an_empty_ca_file_path() {
    let config = LdapConfig {
        ca_file: Some(PathBuf::new()),
        ..valid()
    };
    assert_eq!(config.validate(), Err(LdapConfigError::Empty("ca_file")));
    let config = LdapConfig {
        ca_file: Some(PathBuf::from("/etc/rustryx/ca.pem")),
        ..valid()
    };
    assert_eq!(config.validate(), Ok(()));
}

#[test]
fn deserializes_the_shape_of_the_config_crate_section() {
    let json = r#"{
        "enabled": true,
        "url": "ldap://ldap.example.org",
        "starttls": true,
        "ca_file": "/etc/ldap-ca.pem",
        "bind_dn": "cn=svc,dc=example,dc=org",
        "bind_password": "pw",
        "user_base_dn": "ou=people,dc=example,dc=org",
        "search_filter": "(sAMAccountName={username})",
        "group_strategy": "member_search",
        "group_attribute": "memberOf",
        "group_base_dn": "ou=g,dc=example,dc=org",
        "group_filter": "(member={dn})",
        "admin_groups": ["cn=admins,ou=g,dc=example,dc=org"],
        "connect_timeout_secs": 2,
        "bind_timeout_secs": 3,
        "search_timeout_secs": 4,
        "groups_cache_ttl_secs": 30,
        "nested_group_depth": 3,
        "max_groups": 50
    }"#;
    let config: LdapConfig = serde_json::from_str(json).unwrap();
    let settings = config.settings().unwrap();
    assert_eq!(settings.groups.strategy, LdapGroupStrategy::ReverseMember);
    assert_eq!(settings.groups.max_depth, 3);
    assert_eq!(settings.groups.max_groups, 50);
    assert_eq!(settings.groups.cache_ttl, Duration::from_secs(30));
    assert_eq!(settings.timeouts.connect, Duration::from_secs(2));
    assert_eq!(settings.timeouts.bind, Duration::from_secs(3));
    assert_eq!(settings.timeouts.search, Duration::from_secs(4));
    assert!(settings.endpoint.start_tls);
    assert_eq!(config.admin_groups.len(), 1, "accepted, not used here");
}

#[test]
fn accepts_the_start_tls_and_reverse_member_spellings() {
    let json = r#"{"enabled": true, "url": "ldap://h", "start_tls": true,
                   "group_strategy": "reverse_member"}"#;
    let config: LdapConfig = serde_json::from_str(json).unwrap();
    assert!(config.starttls);
    assert_eq!(config.group_strategy, LdapGroupStrategy::ReverseMember);
}
