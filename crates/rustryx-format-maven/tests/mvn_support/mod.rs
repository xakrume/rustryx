//! Helpers of the real `mvn` tests: a seeded local repository, `settings.xml`, small projects.
//!
//! Why a seed: a fresh `-Dmaven.repo.local` has none of the plugins `mvn deploy` and
//! `dependency:get` need, and downloading them (about 30 MB) for every test is slow and ties
//! every test to the network. The seed is built once per checkout, under `CARGO_TARGET_TMPDIR`,
//! from Maven Central, by running the same goals the tests run against a throwaway `file://`
//! repository. Every test then starts from a COPY of it, so the plugins are there and nothing
//! the test installs or resolves leaks into the next test or into the user's `~/.m2`.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::unused_self,
    clippy::format_collect,
    clippy::duration_suboptimal_units,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::format_push_string,
    reason = "test helpers: a panic is the failure signal"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use rustryx_testserver::{Output, TempProject, Tool, run, run_program};
use tokio::sync::OnceCell;

/// Marker written when the seed is complete.
const MARKER: &str = ".complete";

static SEED: OnceCell<Result<PathBuf, String>> = OnceCell::const_new();

fn seed_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("maven-it-seed")
}

/// The seeded repository, building it on first use. `Err` carries the reason it is not
/// available (no network for the first build), for the SKIP line.
pub async fn seed() -> Result<&'static Path, String> {
    SEED.get_or_init(build_seed)
        .await
        .as_ref()
        .map(PathBuf::as_path)
        .map_err(Clone::clone)
}

async fn build_seed() -> Result<PathBuf, String> {
    let dir = seed_dir();
    if dir.join(MARKER).exists() {
        return Ok(dir);
    }
    // A half built seed from an earlier failure is not trusted.
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let work = TempProject::new();
    write_project(&work, "rustryx.seed", "seed-lib", "1.0", "Seed", None).await;
    let deploy_repo = work.join("seed-deploy");
    let repo_arg = format!("-Dmaven.repo.local={}", dir.display());
    let alt = format!(
        "-DaltDeploymentRepository=seed::file://{}",
        deploy_repo.display()
    );
    let steps: [Vec<String>; 3] = [
        vec![
            "-B".into(),
            "-ntp".into(),
            repo_arg.clone(),
            "deploy".into(),
            alt,
        ],
        vec![
            "-B".into(),
            "-ntp".into(),
            repo_arg.clone(),
            "dependency:resolve".into(),
            "dependency:sources".into(),
        ],
        vec![
            "-B".into(),
            "-ntp".into(),
            repo_arg,
            "dependency:get".into(),
            "-Dartifact=rustryx.seed:seed-lib:1.0".into(),
            format!(
                "-DremoteRepositories=seed::default::file://{}",
                deploy_repo.display()
            ),
        ],
    ];
    for args in steps {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = run(Tool::Mvn, &refs, &[], work.path()).await;
        if !output.success() {
            return Err(format!(
                "cannot seed the Maven plugin cache (needs Maven Central once): {}",
                output
                    .combined()
                    .lines()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
    }
    tokio::fs::write(dir.join(MARKER), b"ok")
        .await
        .map_err(|e| e.to_string())?;
    Ok(dir)
}

/// A Maven run environment: a project directory, a copy of the seed, a `settings.xml`.
pub struct MvnEnv {
    pub project: TempProject,
    pub repo: PathBuf,
    pub settings: PathBuf,
}

/// `None` (after printing SKIP) when the seed is not available.
pub async fn env(servers: &[(&str, &str, &str)]) -> Option<MvnEnv> {
    let seed = match seed().await {
        Ok(seed) => seed,
        Err(reason) => {
            println!("SKIP: mvn: {reason}");
            return None;
        }
    };
    let project = TempProject::new();
    let repo = project.join("m2repo");
    let copied = run_program(
        "cp",
        &[
            "-R",
            &format!("{}/.", seed.display()),
            &repo.display().to_string(),
        ],
        &[],
        project.path(),
        Duration::from_secs(120),
    )
    .await;
    if !copied.success() {
        // `cp -R seed/. dest` needs the destination to exist.
        tokio::fs::create_dir_all(&repo).await.unwrap();
        let again = run_program(
            "cp",
            &[
                "-R",
                &format!("{}/.", seed.display()),
                &repo.display().to_string(),
            ],
            &[],
            project.path(),
            Duration::from_secs(120),
        )
        .await;
        again.assert_success();
    }
    let settings = project.write("settings.xml", settings_xml(servers)).await;
    Some(MvnEnv {
        project,
        repo,
        settings,
    })
}

/// `settings.xml`: credentials for the servers, and Central redirected to a dead port so that a
/// test can never be satisfied by the internet. The mirror keeps the id `central`, which is the
/// name the cached plugin metadata of the seed is filed under.
pub fn settings_xml(servers: &[(&str, &str, &str)]) -> String {
    let mut text =
        String::from("<settings xmlns=\"http://maven.apache.org/SETTINGS/1.2.0\">\n  <servers>\n");
    for (id, user, password) in servers {
        text.push_str(&format!(
            "    <server><id>{id}</id><username>{user}</username><password>{password}</password></server>\n"
        ));
    }
    text.push_str(
        "  </servers>\n  <mirrors>\n    <mirror><id>central</id><mirrorOf>central</mirrorOf>\
         <url>http://127.0.0.1:9/offline</url></mirror>\n  </mirrors>\n</settings>\n",
    );
    text
}

impl MvnEnv {
    /// Runs `mvn` in `dir` (the project root when `None`) with batch mode, this environment's
    /// settings and local repository.
    pub async fn mvn(&self, dir: Option<&Path>, goals: &[&str]) -> Output {
        let repo_arg = format!("-Dmaven.repo.local={}", self.repo.display());
        let settings = self.settings.display().to_string();
        let mut args: Vec<&str> = vec!["-B", "-ntp", "-s", &settings, &repo_arg];
        args.extend_from_slice(goals);
        run(Tool::Mvn, &args, &[], dir.unwrap_or(self.project.path())).await
    }

    /// A file of the local repository.
    pub fn local(&self, relative: &str) -> PathBuf {
        self.repo.join(relative)
    }
}

/// Writes `pom.xml` and one class of a project. `deploy_url` becomes the release and the
/// snapshot repository (`id` `rustryx`).
pub async fn write_project(
    project: &TempProject,
    group: &str,
    artifact: &str,
    version: &str,
    class_name: &str,
    deploy_url: Option<&str>,
) {
    let distribution = deploy_url.map_or_else(String::new, |url| {
        format!(
            "  <distributionManagement>\n    <repository><id>rustryx</id><url>{url}</url></repository>\n    \
             <snapshotRepository><id>rustryx</id><url>{url}</url></snapshotRepository>\n  </distributionManagement>\n"
        )
    });
    let pom = format!(
        "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  <modelVersion>4.0.0</modelVersion>\n  \
         <groupId>{group}</groupId>\n  <artifactId>{artifact}</artifactId>\n  <version>{version}</version>\n  \
         <properties>\n    <maven.compiler.release>17</maven.compiler.release>\n    \
         <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>\n  </properties>\n{distribution}  \
         <build><plugins><plugin><artifactId>maven-source-plugin</artifactId><executions><execution>\
         <id>sources</id><goals><goal>jar-no-fork</goal></goals></execution></executions></plugin></plugins></build>\n</project>\n"
    );
    project.write("pom.xml", pom).await;
    let package = group.replace('-', "_");
    let source = format!(
        "package {package};\npublic final class {class_name} {{ public static String id() {{ return \"{artifact}-{version}-{class_name}\"; }} }}\n"
    );
    project
        .write(
            &format!(
                "src/main/java/{}/{class_name}.java",
                package.replace('.', "/")
            ),
            source,
        )
        .await;
}

/// A consumer project that depends on `group:artifact:version` and resolves it from `repo_url`.
pub async fn write_consumer(
    project: &TempProject,
    dir: &str,
    group: &str,
    artifact: &str,
    version: &str,
    repo_url: &str,
) -> PathBuf {
    let pom = format!(
        "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  <modelVersion>4.0.0</modelVersion>\n  \
         <groupId>io.rustryx.consumer</groupId>\n  <artifactId>consumer</artifactId>\n  <version>1</version>\n  \
         <repositories><repository><id>rustryx</id><url>{repo_url}</url></repository></repositories>\n  \
         <dependencies><dependency><groupId>{group}</groupId><artifactId>{artifact}</artifactId>\
         <version>{version}</version></dependency></dependencies>\n</project>\n"
    );
    project.write(&format!("{dir}/pom.xml"), pom).await;
    project.join(dir)
}

/// Lists every file below `root` as `(path relative to root with slashes, bytes)`.
pub async fn read_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            let path = entry.path();
            if entry.file_type().await.unwrap().is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push((relative, tokio::fs::read(&path).await.unwrap()));
            }
        }
    }
    files.sort();
    files
}
