//! `GET /api/v1/client-setup`: the snippets the repository page shows.
//!
//! Credentials are placeholders (`<TOKEN>`, `<USERNAME>`), never a real secret.
//! A proxy has no publish snippet. A group resolves through itself and publishes
//! to its first hosted member.

use std::sync::Arc;

use http::HeaderMap;
use http::header::HOST;
use rustryx_api::ApiError;
use rustryx_core::identity::{Action, Principal};
use rustryx_core::model::{RepoFormat, RepoType, Repository};
use rustryx_core::ports::{Decision, RbacChecker, RepositoryRepo, ResourceRef};
use serde::Serialize;
use url::Url;

#[derive(Clone)]
pub(crate) struct SetupHttp {
    pub base_url: Option<Url>,
    pub fallback_scheme: &'static str,
    pub anonymous_enabled: bool,
}

#[derive(Serialize)]
pub(crate) struct Setup {
    repository: Ref,
    base_url: String,
    base_url_configured: bool,
    variant: &'static str,
    publish_target: Option<Target>,
    snippets: Vec<Snippet>,
    notes: Vec<Note>,
    private_ca: bool,
}

#[derive(Serialize)]
struct Ref {
    id: rustryx_core::ids::RepositoryId,
    name: String,
    format: RepoFormat,
    repo_type: RepoType,
}

#[derive(Serialize)]
struct Target {
    repository: String,
    reason: &'static str,
}

#[derive(Serialize)]
struct Snippet {
    id: &'static str,
    client: &'static str,
    title: &'static str,
    purpose: &'static str,
    language: &'static str,
    file: Option<&'static str>,
    content: String,
    placeholders: Vec<Placeholder>,
    requires: &'static str,
}

#[derive(Clone, Serialize)]
struct Placeholder {
    name: &'static str,
    description: &'static str,
}

#[derive(Serialize)]
struct Note {
    kind: &'static str,
    text: &'static str,
}

const USERNAME: Placeholder = Placeholder {
    name: "USERNAME",
    description: "Your Rustryx user name.",
};
const TOKEN: Placeholder = Placeholder {
    name: "TOKEN",
    description: "A personal token with the access this step needs. Never commit it.",
};

pub(crate) async fn build(
    repositories: &Arc<dyn RepositoryRepo>,
    rbac: &Arc<dyn RbacChecker>,
    http: &SetupHttp,
    headers: &HeaderMap,
    principal: &Principal,
    repository: &Repository,
) -> Result<Setup, ApiError> {
    let origin = origin_of(http, headers);
    let url = format!("{origin}/repository/{}/", repository.name);
    let anonymous = repository.allow_anonymous && http.anonymous_enabled;
    let (publish, target) = publish_of(repositories, repository).await?;
    let can_write = match &publish {
        Some(target) => may_write(rbac, principal, target).await?,
        None => false,
    };
    let mut notes = Vec::new();
    if !repository.online {
        notes.push(Note {
            kind: "warning",
            text: "This repository is offline. Reads answer 503 until it is switched online.",
        });
    }
    if http.base_url.is_none() {
        notes.push(Note {
            kind: "warning",
            text: "The server base URL is not configured, so the snippets use the address of this request.",
        });
    }
    Ok(Setup {
        repository: Ref {
            id: repository.id,
            name: repository.name.to_string(),
            format: repository.format,
            repo_type: repository.repo_type,
        },
        base_url: origin,
        base_url_configured: http.base_url.is_some(),
        variant: if anonymous {
            "anonymous"
        } else {
            "authenticated"
        },
        publish_target: target,
        snippets: snippets(repository, &url, publish.as_ref(), anonymous, can_write),
        notes,
        private_ca: false,
    })
}

async fn publish_of(
    repositories: &Arc<dyn RepositoryRepo>,
    repository: &Repository,
) -> Result<(Option<Repository>, Option<Target>), ApiError> {
    if repository.repo_type == RepoType::Hosted {
        return Ok((Some(repository.clone()), None));
    }
    if repository.repo_type != RepoType::Group {
        return Ok((None, None));
    }
    let members = repository
        .group
        .as_ref()
        .map(|group| group.members.clone())
        .unwrap_or_default();
    for name in members {
        if let Some(member) = repositories.get_by_name(&name).await?
            && member.repo_type == RepoType::Hosted
        {
            let target = Target {
                repository: member.name.to_string(),
                reason: "Group repositories are read only; publish to the hosted member.",
            };
            return Ok((Some(member), Some(target)));
        }
    }
    Ok((None, None))
}

async fn may_write(
    rbac: &Arc<dyn RbacChecker>,
    principal: &Principal,
    repository: &Repository,
) -> Result<bool, ApiError> {
    if rbac.is_admin(principal).await.map_err(ApiError::from)? {
        return Ok(true);
    }
    let resource = ResourceRef::for_repository(repository, "");
    match rbac.check(principal, Action::Write, &resource).await {
        Ok(Decision::Allow) => Ok(true),
        Ok(Decision::Deny) => Ok(false),
        Err(error) => Err(ApiError::from(error)),
    }
}

fn origin_of(http: &SetupHttp, headers: &HeaderMap) -> String {
    if let Some(base) = &http.base_url {
        return base.as_str().trim_end_matches('/').to_owned();
    }
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    format!("{}://{host}", http.fallback_scheme)
}

fn snippets(
    repository: &Repository,
    url: &str,
    publish: Option<&Repository>,
    anonymous: bool,
    can_write: bool,
) -> Vec<Snippet> {
    let publish_url = publish.map(|repo| format_url(url, repo.name.as_str()));
    let publish_name = publish.map(|repo| repo.name.to_string());
    match repository.format {
        RepoFormat::Maven2 => maven(url, publish_url.as_deref(), anonymous, can_write),
        RepoFormat::Npm => npm(
            url,
            &repository.name.to_string(),
            publish_url.as_deref(),
            anonymous,
            can_write,
        ),
        RepoFormat::Pypi => pypi(url, publish_url.as_deref(), anonymous, can_write),
        RepoFormat::Docker | RepoFormat::Oci => docker(
            url,
            &repository.name.to_string(),
            publish_name.as_deref(),
            anonymous,
            can_write,
        ),
        RepoFormat::Helm => helm(
            url,
            &repository.name.to_string(),
            publish_name.as_deref(),
            anonymous,
            can_write,
        ),
        RepoFormat::Pub => pub_snippets(url, publish_url.as_deref(), anonymous, can_write),
        RepoFormat::Raw => raw(url, publish_url.as_deref(), anonymous, can_write),
        RepoFormat::Rpm => rpm(url, anonymous),
        RepoFormat::Apt => apt(url, anonymous),
        RepoFormat::Apk => apk(url),
        _ => Vec::new(),
    }
}

fn format_url(resolve_url: &str, publish_name: &str) -> String {
    let base = resolve_url
        .split("/repository/")
        .next()
        .unwrap_or(resolve_url);
    format!("{base}/repository/{publish_name}/")
}

fn host_of(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_owned()
}

#[allow(
    clippy::too_many_arguments,
    reason = "one argument per field of the snippet the client setup panel renders"
)]
fn snippet(
    id: &'static str,
    client: &'static str,
    title: &'static str,
    purpose: &'static str,
    language: &'static str,
    file: Option<&'static str>,
    content: String,
    placeholders: Vec<Placeholder>,
    requires: &'static str,
) -> Snippet {
    Snippet {
        id,
        client,
        title,
        purpose,
        language,
        file,
        content,
        placeholders,
        requires,
    }
}

fn maven(url: &str, publish: Option<&str>, anonymous: bool, can_write: bool) -> Vec<Snippet> {
    let mut out = vec![snippet(
        "mvn-settings",
        "mvn",
        "Resolve through the repository",
        "resolve",
        "xml",
        Some("~/.m2/settings.xml"),
        format!(
            "<mirror>\n  <id>rustryx</id>\n  <mirrorOf>*</mirrorOf>\n  <url>{url}</url>\n</mirror>"
        ),
        Vec::new(),
        "browse",
    )];
    if !anonymous {
        out.push(snippet(
            "mvn-credentials",
            "mvn",
            "Credentials",
            "login",
            "xml",
            Some("~/.m2/settings.xml"),
            "<server>\n  <id>rustryx</id>\n  <username><USERNAME></username>\n  <password><TOKEN></password>\n</server>".to_owned(),
            vec![USERNAME, TOKEN],
            "browse",
        ));
    }
    let gradle = if anonymous {
        format!("repositories {{\n    maven {{ url = uri(\"{url}\") }}\n}}")
    } else {
        format!(
            "repositories {{\n    maven {{\n        url = uri(\"{url}\")\n        credentials {{\n            username = \"<USERNAME>\"\n            password = \"<TOKEN>\"\n        }}\n    }}\n}}"
        )
    };
    out.push(snippet(
        "gradle-resolve",
        "gradle",
        "Resolve with Gradle",
        "resolve",
        "groovy",
        Some("build.gradle"),
        gradle,
        if anonymous {
            Vec::new()
        } else {
            vec![USERNAME, TOKEN]
        },
        "browse",
    ));
    if can_write && let Some(publish) = publish {
        out.push(snippet(
                "mvn-publish",
                "mvn",
                "Publish with mvn deploy",
                "publish",
                "xml",
                Some("pom.xml"),
                format!("<distributionManagement>\n  <repository>\n    <id>rustryx</id>\n    <url>{publish}</url>\n  </repository>\n</distributionManagement>"),
                Vec::new(),
                "write",
            ));
    }
    out
}

fn npm(
    url: &str,
    name: &str,
    publish: Option<&str>,
    anonymous: bool,
    can_write: bool,
) -> Vec<Snippet> {
    let host = host_of(url);
    let content = if anonymous {
        format!("registry={url}")
    } else {
        format!("registry={url}\n//{host}/repository/{name}/:_authToken=<TOKEN>")
    };
    let mut out = vec![snippet(
        "npm-registry",
        "npm",
        "Resolve through the repository",
        "resolve",
        "ini",
        Some("~/.npmrc"),
        content,
        if anonymous { Vec::new() } else { vec![TOKEN] },
        "browse",
    )];
    if can_write && let Some(publish) = publish {
        out.push(snippet(
            "npm-publish",
            "npm",
            "Publish a package",
            "publish",
            "bash",
            None,
            format!("npm publish --registry {publish}"),
            Vec::new(),
            "write",
        ));
    }
    out
}

fn pypi(url: &str, publish: Option<&str>, anonymous: bool, can_write: bool) -> Vec<Snippet> {
    let simple = format!("{url}simple/");
    let index = if anonymous {
        simple
    } else {
        simple.replacen("://", "://<USERNAME>:<TOKEN>@", 1)
    };
    let mut out = vec![snippet(
        "pip-index",
        "pip",
        "Resolve with pip",
        "resolve",
        "ini",
        Some("~/.config/pip/pip.conf"),
        format!("[global]\nindex-url = {index}"),
        if anonymous {
            Vec::new()
        } else {
            vec![USERNAME, TOKEN]
        },
        "browse",
    )];
    if can_write && let Some(publish) = publish {
        out.push(snippet(
            "twine-upload",
            "twine",
            "Publish with twine",
            "publish",
            "bash",
            None,
            format!("twine upload --repository-url {publish} -u <USERNAME> -p <TOKEN> dist/*"),
            vec![USERNAME, TOKEN],
            "write",
        ));
    }
    out
}

fn docker(
    url: &str,
    name: &str,
    publish_name: Option<&str>,
    anonymous: bool,
    can_write: bool,
) -> Vec<Snippet> {
    let host = host_of(url);
    let mut out = Vec::new();
    if !anonymous {
        out.push(snippet(
            "docker-login",
            "docker",
            "Sign in",
            "login",
            "bash",
            None,
            format!("docker login {host} -u <USERNAME> -p <TOKEN>"),
            vec![USERNAME, TOKEN],
            "browse",
        ));
    }
    out.push(snippet(
        "docker-pull",
        "docker",
        "Pull an image",
        "resolve",
        "bash",
        None,
        format!("docker pull {host}/{name}/<IMAGE>:<TAG>"),
        vec![
            Placeholder {
                name: "IMAGE",
                description: "Image name inside the repository.",
            },
            Placeholder {
                name: "TAG",
                description: "Tag or digest to pull.",
            },
        ],
        "browse",
    ));
    if can_write && let Some(publish_name) = publish_name {
        out.push(snippet(
                "docker-push",
                "docker",
                "Tag and push an image",
                "publish",
                "bash",
                None,
                format!("docker tag <IMAGE>:<TAG> {host}/{publish_name}/<IMAGE>:<TAG>\ndocker push {host}/{publish_name}/<IMAGE>:<TAG>"),
                vec![
                    Placeholder { name: "IMAGE", description: "Image name inside the repository." },
                    Placeholder { name: "TAG", description: "Tag or digest to pull." },
                ],
                "write",
            ));
    }
    out
}

fn helm(
    url: &str,
    name: &str,
    publish_name: Option<&str>,
    anonymous: bool,
    can_write: bool,
) -> Vec<Snippet> {
    let host = host_of(url);
    let mut out = Vec::new();
    if !anonymous {
        out.push(snippet(
            "helm-login",
            "helm",
            "Sign in to the registry",
            "login",
            "bash",
            None,
            format!("helm registry login {host} --username <USERNAME> --password <TOKEN>"),
            vec![USERNAME, TOKEN],
            "browse",
        ));
    }
    out.push(snippet(
        "helm-pull",
        "helm",
        "Pull a chart",
        "resolve",
        "bash",
        None,
        format!(
            "helm pull oci://{host}/{name}/<CHART> --version <VERSION>\nhelm repo add rustryx {url}"
        ),
        vec![
            Placeholder {
                name: "CHART",
                description: "Chart name.",
            },
            Placeholder {
                name: "VERSION",
                description: "Chart version.",
            },
        ],
        "browse",
    ));
    if can_write && let Some(publish_name) = publish_name {
        out.push(snippet(
            "helm-push",
            "helm",
            "Push a chart",
            "publish",
            "bash",
            None,
            format!("helm push <CHART>-<VERSION>.tgz oci://{host}/{publish_name}"),
            vec![
                Placeholder {
                    name: "CHART",
                    description: "Chart name.",
                },
                Placeholder {
                    name: "VERSION",
                    description: "Chart version.",
                },
            ],
            "write",
        ));
    }
    out
}

fn pub_snippets(
    url: &str,
    publish: Option<&str>,
    anonymous: bool,
    can_write: bool,
) -> Vec<Snippet> {
    let mut out = Vec::new();
    if !anonymous {
        out.push(snippet(
            "dart-token",
            "dart",
            "Store the token",
            "login",
            "bash",
            None,
            format!("echo \"<TOKEN>\" | dart pub token add {url}"),
            vec![TOKEN],
            "browse",
        ));
    }
    out.push(snippet(
        "dart-hosted",
        "dart",
        "Resolve from the repository",
        "resolve",
        "yaml",
        Some("pubspec.yaml"),
        format!("dependencies:\n  <PACKAGE>:\n    hosted: {url}\n    version: ^<VERSION>"),
        vec![
            Placeholder {
                name: "PACKAGE",
                description: "Package name.",
            },
            Placeholder {
                name: "VERSION",
                description: "Package version.",
            },
        ],
        "browse",
    ));
    if can_write && let Some(publish) = publish {
        out.push(snippet(
            "dart-publish",
            "dart",
            "Publish a package",
            "publish",
            "bash",
            None,
            format!("dart pub publish --server {publish}"),
            Vec::new(),
            "write",
        ));
    }
    out
}

fn rpm(url: &str, anonymous: bool) -> Vec<Snippet> {
    let auth = if anonymous {
        String::new()
    } else {
        "\nusername=<USERNAME>\npassword=<TOKEN>".to_owned()
    };
    let mut placeholders = Vec::new();
    if !anonymous {
        placeholders.extend([USERNAME, TOKEN]);
    }
    vec![snippet(
        "dnf-repo",
        "dnf",
        "Use the repository with dnf or yum",
        "resolve",
        "ini",
        Some("/etc/yum.repos.d/rustryx.repo"),
        format!("[rustryx]\nname=rustryx\nbaseurl={url}\nenabled=1\ngpgcheck=0{auth}\n"),
        placeholders,
        "browse",
    )]
}

fn apt(url: &str, anonymous: bool) -> Vec<Snippet> {
    let line = if anonymous {
        format!("deb {url} <SUITE> main")
    } else {
        format!("deb [trusted=yes] {url} <SUITE> main")
    };
    let mut placeholders = vec![Placeholder {
        name: "SUITE",
        description: "Distribution suite, for example trixie or noble.",
    }];
    if !anonymous {
        placeholders.extend([USERNAME, TOKEN]);
    }
    vec![snippet(
        "apt-source",
        "apt",
        "Add the repository to apt",
        "resolve",
        "text",
        Some("/etc/apt/sources.list.d/rustryx.list"),
        line,
        placeholders,
        "browse",
    )]
}

fn apk(url: &str) -> Vec<Snippet> {
    vec![snippet(
        "apk-repo",
        "apk",
        "Add the repository to apk",
        "resolve",
        "text",
        Some("/etc/apk/repositories"),
        url.to_owned(),
        Vec::new(),
        "browse",
    )]
}

fn raw(url: &str, publish: Option<&str>, anonymous: bool, can_write: bool) -> Vec<Snippet> {
    let content = if anonymous {
        format!("curl -fO {url}<PATH>")
    } else {
        format!("curl -fO -u <USERNAME>:<TOKEN> {url}<PATH>")
    };
    let mut placeholders = Vec::new();
    if !anonymous {
        placeholders.extend([USERNAME, TOKEN]);
    }
    placeholders.push(Placeholder {
        name: "PATH",
        description: "Path of the file inside the repository.",
    });
    let mut out = vec![snippet(
        "curl-download",
        "curl",
        "Download a file",
        "resolve",
        "bash",
        None,
        content,
        placeholders,
        "browse",
    )];
    if can_write && let Some(publish) = publish {
        out.push(snippet(
            "curl-upload",
            "curl",
            "Upload a file",
            "publish",
            "bash",
            None,
            format!("curl -fT <FILE> -u <USERNAME>:<TOKEN> {publish}<PATH>"),
            vec![
                USERNAME,
                TOKEN,
                Placeholder {
                    name: "FILE",
                    description: "Local file to upload.",
                },
                Placeholder {
                    name: "PATH",
                    description: "Path of the file inside the repository.",
                },
            ],
            "write",
        ));
    }
    out
}
