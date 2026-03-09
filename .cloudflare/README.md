We keep three Cloudflare workers in this repo, but only the release worker is
part of the current automated deployment path.

- `open-source-website-assets` is used for static open-source assets such as install helpers
- `docs-proxy` is used for `https://superzet.dev/docs`
- `release-assets` is used for `https://releases.nangman.ai/releases`

On push to `main`, only the release worker is deployed by the
`deploy_cloudflare.yml` workflow.

### Deployment

The current workflow deploys `release-assets` only. Cloudflare should route
`releases.nangman.ai/releases*` to that worker.

For the release worker, configure an optional `GITHUB_RELEASES_TOKEN` secret in Cloudflare if you want higher GitHub API rate limits for update checks.

### Testing

You can use [wrangler](https://developers.cloudflare.com/workers/cli-wrangler/install-update) to test these workers locally, or to deploy custom versions.
