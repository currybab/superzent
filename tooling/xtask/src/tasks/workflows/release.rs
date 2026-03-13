use gh_workflow::{Event, Expression, Job, Level, Permissions, Push, Run, Step, Use, Workflow};
use indoc::{formatdoc, indoc};

use crate::tasks::workflows::{
    run_bundling::upload_artifact,
    runners::{self, Arch, Platform},
    steps::{self, DEFAULT_REPOSITORY_OWNER_GUARD, FluentBuilder, NamedJob, dependant_job, named},
    vars::{self, JobOutput, StepOutput, assets, bundle_envs},
};

const RELEASE_ARTIFACT: &str = "superzent-aarch64.dmg";
const REMOTE_SERVER_LINUX_AARCH64_ARTIFACT: &str = assets::REMOTE_SERVER_LINUX_AARCH64;
const REMOTE_SERVER_LINUX_X86_64_ARTIFACT: &str = assets::REMOTE_SERVER_LINUX_X86_64;
const CHECKSUM_ARTIFACT: &str = "sha256sums.txt";

fn stable_release_tag_guard(valid_release_tag: &JobOutput) -> Expression {
    Expression::new(format!(
        "{DEFAULT_REPOSITORY_OWNER_GUARD} && {} == 'true'",
        valid_release_tag.expr()
    ))
}

pub(crate) fn release() -> Workflow {
    let (validate_release_tag, valid_release_tag) = validate_release_tag();
    let bundle_mac = bundle_mac_stable(&validate_release_tag, &valid_release_tag);
    let bundle_linux_x86_64 =
        bundle_linux_remote_server_stable(Arch::X86_64, &validate_release_tag, &valid_release_tag);
    let bundle_linux_aarch64 =
        bundle_linux_remote_server_stable(Arch::AARCH64, &validate_release_tag, &valid_release_tag);
    let publish = publish_release(
        &[
            &validate_release_tag,
            &bundle_mac,
            &bundle_linux_x86_64,
            &bundle_linux_aarch64,
        ],
        &valid_release_tag,
    );

    named::workflow()
        .on(Event::default().push(Push::default().tags(vec!["v*.*.*".to_string()])))
        .permissions(Permissions::default().contents(Level::Write))
        .concurrency(vars::one_workflow_per_non_main_branch())
        .add_env(("CARGO_TERM_COLOR", "always"))
        .add_env(("RUST_BACKTRACE", "1"))
        .add_job(validate_release_tag.name, validate_release_tag.job)
        .add_job(bundle_mac.name, bundle_mac.job)
        .add_job(bundle_linux_x86_64.name, bundle_linux_x86_64.job)
        .add_job(bundle_linux_aarch64.name, bundle_linux_aarch64.job)
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

fn validate_release_tag() -> (NamedJob, JobOutput) {
    let step = release_tag_output_step();
    let output = StepOutput::new(&step, "is_release_tag");

    let job = Job::default()
        .cond(Expression::new(DEFAULT_REPOSITORY_OWNER_GUARD))
        .runs_on(runners::LINUX_SMALL)
        .timeout_minutes(1u32)
        .outputs([(output.name.to_owned(), output.to_string())])
        .add_step(step);

    let job = NamedJob {
        name: "validate_release_tag".to_string(),
        job,
    };
    let output = output.as_job_output(&job);

    (job, output)
}

fn release_tag_output_step() -> Step<Run> {
    named::bash(indoc! {r#"
        if printf '%s\n' "$GITHUB_REF_NAME" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
          echo "is_release_tag=true" >> "$GITHUB_OUTPUT"
        else
          echo "is_release_tag=false" >> "$GITHUB_OUTPUT"
        fi
    "#})
    .id("validate-release-tag")
}

fn bundle_mac_stable(validate_release_tag: &NamedJob, valid_release_tag: &JobOutput) -> NamedJob {
    NamedJob {
        name: "bundle_mac_stable".to_string(),
        job: dependant_job(&[validate_release_tag])
            .cond(stable_release_tag_guard(valid_release_tag))
            .runs_on(runners::MAC_DEFAULT)
            .timeout_minutes(360u32)
            .add_env(("CARGO_TARGET_DIR", "target"))
            .add_env(("SUPERZENT_RELEASE_CHANNEL", "stable"))
            .add_env(("SUPERZENT_MACOS_CERTIFICATE", vars::MACOS_CERTIFICATE))
            .add_env((
                "SUPERZENT_MACOS_CERTIFICATE_PASSWORD",
                vars::MACOS_CERTIFICATE_PASSWORD,
            ))
            .add_env((
                "SUPERZENT_APPLE_NOTARIZATION_KEY",
                vars::APPLE_NOTARIZATION_KEY,
            ))
            .add_env((
                "SUPERZENT_APPLE_NOTARIZATION_KEY_ID",
                vars::APPLE_NOTARIZATION_KEY_ID,
            ))
            .add_env((
                "SUPERZENT_APPLE_NOTARIZATION_ISSUER_ID",
                vars::APPLE_NOTARIZATION_ISSUER_ID,
            ))
            .add_env((
                "SUPERZENT_MACOS_SIGNING_IDENTITY",
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

fn bundle_linux_remote_server_stable(
    arch: Arch,
    validate_release_tag: &NamedJob,
    valid_release_tag: &JobOutput,
) -> NamedJob {
    let zig_version = "0.14.1";
    let remote_server_triple = match arch {
        Arch::X86_64 => "x86_64-unknown-linux-musl",
        Arch::AARCH64 => "aarch64-unknown-linux-musl",
    };
    let artifact_name = match arch {
        Arch::X86_64 => REMOTE_SERVER_LINUX_X86_64_ARTIFACT,
        Arch::AARCH64 => REMOTE_SERVER_LINUX_AARCH64_ARTIFACT,
    };
    let build_script = formatdoc!(
        r#"
        if ! command -v zig >/dev/null 2>&1; then
          case "$(uname -m)" in
            x86_64) zig_arch=x86_64 ;;
            aarch64|arm64) zig_arch=aarch64 ;;
            *)
              echo "Unsupported architecture for Zig bootstrap: $(uname -m)" >&2
              exit 1
              ;;
          esac
          zig_root="$(mktemp -d)"
          curl -fsSL "https://ziglang.org/download/{zig_version}/zig-${{zig_arch}}-linux-{zig_version}.tar.xz" -o "${{zig_root}}/zig.tar.xz"
          tar -xJf "${{zig_root}}/zig.tar.xz" -C "${{zig_root}}" --strip-components=1
          export PATH="${{zig_root}}:$PATH"
        fi
        if ! command -v cargo-zigbuild >/dev/null 2>&1; then
          cargo install --locked cargo-zigbuild
        fi
        rustup target add "{remote_server_triple}"
        export RUSTFLAGS="${{RUSTFLAGS:-}} -C target-feature=+crt-static"
        cargo zigbuild --release --target "{remote_server_triple}" --package remote_server
        objcopy --strip-debug "target/{remote_server_triple}/release/remote_server"
        gzip -f --stdout --best "target/{remote_server_triple}/release/remote_server" > "target/{artifact_name}"
        "#,
        zig_version = zig_version,
        remote_server_triple = remote_server_triple,
        artifact_name = artifact_name,
    );

    NamedJob {
        name: format!("bundle_linux_remote_server_stable_{arch}"),
        job: dependant_job(&[validate_release_tag])
            .cond(stable_release_tag_guard(valid_release_tag))
            .runs_on(arch.linux_bundler())
            .timeout_minutes(60u32)
            .envs(bundle_envs(Platform::Linux))
            .add_env(("SUPERZENT_RELEASE_CHANNEL", "stable"))
            .add_env(("CC", "clang-18"))
            .add_env(("CXX", "clang++-18"))
            .add_step(steps::checkout_repo())
            .map(steps::install_linux_dependencies)
            .add_step(named::bash(&build_script))
            .add_step(upload_artifact(&format!("target/{artifact_name}"))),
    }
}

fn publish_release(deps: &[&NamedJob], valid_release_tag: &JobOutput) -> NamedJob {
    let publish_script = formatdoc!(
        r#"
        mkdir -p release-artifacts
        cp "./artifacts/{release_artifact}/{release_artifact}" "release-artifacts/{release_artifact}"
        cp "./artifacts/{remote_server_linux_x86_64_artifact}/{remote_server_linux_x86_64_artifact}" "release-artifacts/{remote_server_linux_x86_64_artifact}"
        cp "./artifacts/{remote_server_linux_aarch64_artifact}/{remote_server_linux_aarch64_artifact}" "release-artifacts/{remote_server_linux_aarch64_artifact}"
        shasum -a 256 "release-artifacts/{release_artifact}" > "release-artifacts/{checksum_artifact}"

        if ! gh release view "$GITHUB_REF_NAME" --repo "$GITHUB_REPOSITORY" >/dev/null 2>&1; then
          gh release create "$GITHUB_REF_NAME" \
            --repo "$GITHUB_REPOSITORY" \
            --title "$GITHUB_REF_NAME" \
            --generate-notes
        fi

        gh release upload "$GITHUB_REF_NAME" \
          --repo "$GITHUB_REPOSITORY" \
          --clobber \
          release-artifacts/*
        "#,
        release_artifact = RELEASE_ARTIFACT,
        remote_server_linux_x86_64_artifact = REMOTE_SERVER_LINUX_X86_64_ARTIFACT,
        remote_server_linux_aarch64_artifact = REMOTE_SERVER_LINUX_AARCH64_ARTIFACT,
        checksum_artifact = CHECKSUM_ARTIFACT,
    );

    NamedJob {
        name: "publish_release".to_string(),
        job: dependant_job(deps)
            .cond(stable_release_tag_guard(valid_release_tag))
            .runs_on(runners::LINUX_SMALL)
            .timeout_minutes(30u32)
            .add_step(download_workflow_artifacts())
            .add_step(steps::script("ls -lR ./artifacts"))
            .add_step(named::bash(&publish_script).add_env(("GITHUB_TOKEN", vars::GITHUB_TOKEN))),
    }
}
