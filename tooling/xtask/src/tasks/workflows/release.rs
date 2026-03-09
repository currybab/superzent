use gh_workflow::{Event, Push, Step, Use, Workflow};
use indoc::formatdoc;

use crate::tasks::workflows::{
    run_bundling::upload_artifact,
    runners::{self, Platform},
    steps::{self, CommonJobConditions, NamedJob, dependant_job, named},
    vars,
};

const RELEASE_ARTIFACT: &str = "superzet-aarch64.dmg";
const CHECKSUM_ARTIFACT: &str = "sha256sums.txt";

pub(crate) struct ReleaseBundleJobs {
    pub linux_aarch64: NamedJob,
    pub linux_x86_64: NamedJob,
    pub mac_aarch64: NamedJob,
    pub mac_x86_64: NamedJob,
    pub windows_aarch64: NamedJob,
    pub windows_x86_64: NamedJob,
}

impl ReleaseBundleJobs {
    pub(crate) fn into_jobs(self) -> Vec<NamedJob> {
        vec![
            self.linux_aarch64,
            self.linux_x86_64,
            self.mac_aarch64,
            self.mac_x86_64,
            self.windows_aarch64,
            self.windows_x86_64,
        ]
    }
}

pub(crate) fn release() -> Workflow {
    let bundle = bundle_mac_preview();
    let publish = publish_preview_release(&[&bundle]);

    named::workflow()
        .on(Event::default().push(Push::default().tags(vec!["v*-pre".to_string()])))
        .concurrency(vars::one_workflow_per_non_main_branch())
        .add_env(("CARGO_TERM_COLOR", "always"))
        .add_env(("RUST_BACKTRACE", "1"))
        .add_job(bundle.name, bundle.job)
        .add_job(publish.name, publish.job)
}

fn download_workflow_artifacts() -> Step<Use> {
    named::uses(
        "actions",
        "download-artifact",
        "018cc2cf5baa6db3ef3c5f8a56943fffe632ef53", // v6.0.0
    )
    .add_with(("path", "./artifacts/"))
}

fn bundle_mac_preview() -> NamedJob {
    NamedJob {
        name: "bundle_mac_preview".to_string(),
        job: dependant_job(&[])
            .with_repository_owner_guard()
            .runs_on(runners::MAC_DEFAULT)
            .timeout_minutes(90u32)
            .add_env(("SUPERZET_RELEASE_CHANNEL", "preview"))
            .add_env(("SUPERZET_MACOS_CERTIFICATE", vars::MACOS_CERTIFICATE))
            .add_env((
                "SUPERZET_MACOS_CERTIFICATE_PASSWORD",
                vars::MACOS_CERTIFICATE_PASSWORD,
            ))
            .add_env((
                "SUPERZET_APPLE_NOTARIZATION_KEY",
                vars::APPLE_NOTARIZATION_KEY,
            ))
            .add_env((
                "SUPERZET_APPLE_NOTARIZATION_KEY_ID",
                vars::APPLE_NOTARIZATION_KEY_ID,
            ))
            .add_env((
                "SUPERZET_APPLE_NOTARIZATION_ISSUER_ID",
                vars::APPLE_NOTARIZATION_ISSUER_ID,
            ))
            .add_env((
                "SUPERZET_MACOS_SIGNING_IDENTITY",
                vars::MACOS_SIGNING_IDENTITY,
            ))
            .add_step(steps::checkout_repo())
            .add_step(steps::setup_node())
            .add_step(steps::clear_target_dir_if_large(Platform::Mac))
            .add_step(named::bash("./script/bundle-mac aarch64-apple-darwin"))
            .add_step(upload_artifact(&format!(
                "target/aarch64-apple-darwin/release/{RELEASE_ARTIFACT}"
            ))),
    }
}

fn publish_preview_release(deps: &[&NamedJob]) -> NamedJob {
    let publish_script = formatdoc!(
        r#"
        mkdir -p release-artifacts
        cp "./artifacts/{release_artifact}/{release_artifact}" "release-artifacts/{release_artifact}"
        shasum -a 256 "release-artifacts/{release_artifact}" > "release-artifacts/{checksum_artifact}"

        if ! gh release view "$GITHUB_REF_NAME" --repo "$GITHUB_REPOSITORY" >/dev/null 2>&1; then
          gh release create "$GITHUB_REF_NAME" \
            --repo "$GITHUB_REPOSITORY" \
            --title "$GITHUB_REF_NAME" \
            --prerelease \
            --generate-notes
        fi

        gh release upload "$GITHUB_REF_NAME" \
          --repo "$GITHUB_REPOSITORY" \
          --clobber \
          release-artifacts/*
        "#,
        release_artifact = RELEASE_ARTIFACT,
        checksum_artifact = CHECKSUM_ARTIFACT,
    );

    NamedJob {
        name: "publish_preview_release".to_string(),
        job: dependant_job(deps)
            .with_repository_owner_guard()
            .runs_on(runners::LINUX_SMALL)
            .timeout_minutes(30u32)
            .add_step(download_workflow_artifacts())
            .add_step(steps::script("ls -lR ./artifacts"))
            .add_step(named::bash(&publish_script).add_env(("GITHUB_TOKEN", vars::GITHUB_TOKEN))),
    }
}
