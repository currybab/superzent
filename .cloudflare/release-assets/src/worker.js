const REPOSITORY = "currybab/superzent";
const GITHUB_API_BASE_URL = `https://api.github.com/repos/${REPOSITORY}`;

function jsonResponse(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "cache-control": "public, max-age=300",
      "content-type": "application/json; charset=utf-8",
    },
  });
}

function normalizeVersion(value) {
  return value.replace(/^v/, "").split("+", 1)[0].replace(/-pre$/, "");
}

function tagForChannel(channel, version) {
  const normalizedVersion = normalizeVersion(version);
  if (!normalizedVersion) {
    return null;
  }

  switch (channel) {
    case "stable":
      return `v${normalizedVersion}`;
    default:
      return null;
  }
}

function releaseMatchesChannel(release, channel) {
  if (release.draft) {
    return false;
  }

  switch (channel) {
    case "stable":
      return !release.prerelease && !release.tag_name.endsWith("-pre");
    default:
      return false;
  }
}

function assetNameFromQuery(searchParams) {
  const asset = searchParams.get("asset");
  const os = searchParams.get("os");
  const arch = searchParams.get("arch");

  if (asset === "superzent") {
    if (os === "macos" && arch === "aarch64") {
      return "superzent-aarch64.dmg";
    }
  }

  if (asset === "superzent-remote-server") {
    if (os === "linux" && arch === "x86_64") {
      return "superzent-remote-server-linux-x86_64.gz";
    }

    if (os === "linux" && arch === "aarch64") {
      return "superzent-remote-server-linux-aarch64.gz";
    }
  }

  return null;
}

async function githubFetch(path, env) {
  const headers = {
    Accept: "application/vnd.github+json",
    "User-Agent": "superzent-release-assets-worker",
  };

  if (env.GITHUB_RELEASES_TOKEN) {
    headers.Authorization = `Bearer ${env.GITHUB_RELEASES_TOKEN}`;
  }

  const response = await fetch(`${GITHUB_API_BASE_URL}${path}`, { headers });
  if (!response.ok) {
    return null;
  }

  return response.json();
}

async function loadLatestRelease(channel, env) {
  const releases = await githubFetch("/releases?per_page=20", env);
  if (!releases) {
    return null;
  }

  return releases.find((release) => releaseMatchesChannel(release, channel)) ?? null;
}

async function loadRelease(channel, version, env) {
  if (version === "latest") {
    return loadLatestRelease(channel, env);
  }

  const tagName = tagForChannel(channel, version);
  if (!tagName) {
    return null;
  }

  const taggedRelease = await githubFetch(`/releases/tags/${tagName}`, env);
  if (taggedRelease) {
    return taggedRelease;
  }

  const releases = await githubFetch("/releases?per_page=50", env);
  if (!releases) {
    return null;
  }

  const normalizedVersion = normalizeVersion(version);
  return (
    releases.find(
      (release) =>
        releaseMatchesChannel(release, channel) &&
        normalizeVersion(release.tag_name) === normalizedVersion,
    ) ?? null
  );
}

function resolveAsset(release, searchParams) {
  const expectedAssetName = assetNameFromQuery(searchParams);
  if (!expectedAssetName) {
    return null;
  }

  return release.assets.find((asset) => asset.name === expectedAssetName) ?? null;
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const parts = url.pathname.split("/").filter(Boolean);

    if (parts[0] !== "releases" || parts.length < 3) {
      return jsonResponse({ error: "Not found" }, 404);
    }

    const [, channel, version, action] = parts;
    if (channel !== "stable") {
      return jsonResponse({ error: "Unsupported release channel" }, 404);
    }

    const release = await loadRelease(channel, version, env);
    if (!release) {
      return jsonResponse({ error: "Release not found" }, 404);
    }

    if (action === "asset") {
      const asset = resolveAsset(release, url.searchParams);
      if (!asset) {
        return jsonResponse({ error: "Asset not found" }, 404);
      }

      return jsonResponse({
        version: normalizeVersion(release.tag_name),
        url: asset.browser_download_url,
      });
    }

    if (action === "download") {
      const asset = resolveAsset(release, url.searchParams);
      if (!asset) {
        return jsonResponse({ error: "Asset not found" }, 404);
      }

      return Response.redirect(asset.browser_download_url, 302);
    }

    if (parts.length === 3) {
      return Response.redirect(release.html_url, 302);
    }

    return jsonResponse({ error: "Not found" }, 404);
  },
};
