# Release

The current public desktop release flow is macOS Apple Silicon only.

- Tag releases as `vX.Y.Z`
- GitHub Actions builds `superzent-aarch64.dmg`
- The release workflow also uploads Linux `remote_server` support assets
- `releases.nangman.ai/releases/...` is served by a thin Cloudflare worker that points the app at those GitHub assets

## Release Infrastructure

To publish a release with in-app updates, you need:

- GitHub Releases enabled for this repository
- Cloudflare configured for `nangman.ai` with a `releases.nangman.ai/releases*` route
- the release worker deployed from `.cloudflare/release-assets`
- optional but recommended: a Cloudflare worker secret named `GITHUB_RELEASES_TOKEN` to avoid GitHub API rate limits on update checks
- Apple signing and notarization credentials in GitHub secrets:
  - `MACOS_CERTIFICATE`
  - `MACOS_CERTIFICATE_PASSWORD`
  - `APPLE_NOTARIZATION_KEY`
  - `APPLE_NOTARIZATION_KEY_ID`
  - `APPLE_NOTARIZATION_ISSUER_ID`
- the mac signing identity in the `MACOS_SIGNING_IDENTITY` repository variable

The app prefers `SUPERZENT_*` runtime env vars for release/update overrides, but legacy `ZED_*` aliases still work during the transition.
