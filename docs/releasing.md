# Releasing

## How a release happens

1. Commits land on `main` in Conventional Commits form.
2. `release-plz.yml` opens or updates the release PR (`release-plz-*` branch):
   - one version bump for all 5 crates (version group `hex`);
   - below 1.0, `feat` and `!` bump the minor, `fix` bumps the patch;
   - new entries in the root `CHANGELOG.md`.
3. You merge the release PR.
4. `release-plz.yml` publishes the 5 crates to crates.io and pushes the tag
   `v<version>` (only hex-cli is tagged). release-plz creates no GitHub Release.
5. The tag starts `release.yml` (dist):
   - it re-runs `test.yml` on the tagged commit;
   - it builds the 4 targets (macOS arm64 + x86_64, Linux musl arm64 + x86_64);
   - it creates the GitHub Release with the archives and the shell installer.
6. dist publish jobs push the channels:
   - Homebrew: `k4black/homebrew-tap`, formula `hex-cli` (`brew install k4black/tap/hex-cli`);
   - npm: `@k4black/hex-cli`;
   - PyPI: `hex-cli` (`publish-pypi.yml`, maturin wheels + sdist).

**First release (0.1.0).** 0.1.0 is not on crates.io, so release-plz opens no
PR for it. The first push to `main` after the setup below publishes 0.1.0 and
tags `v0.1.0` directly. Finish the checklist before you merge the release
infra to `main`. If the run fails for a missing secret, re-run it after setup.

## Override the version

- Edit the version in the release PR before you merge it. A later push to
  `main` regenerates the PR, so edit it last.
- Or run `release-plz set-version` on a branch and merge it. Give all 5 crates
  the same version:
  `release-plz set-version hex-proto@0.3.0 hex-kernel@0.3.0 hex-worker@0.3.0 hex-runtime@0.3.0 hex-cli@0.3.0`

## One-time setup

### GitHub App (release token)

- Create a GitHub App under your account (Settings > Developer settings >
  GitHub Apps). No webhook.
- Repository permissions: Contents read and write, Pull requests read and
  write. Metadata read is automatic.
- Install it on `k4black/hex`.
- In `k4black/hex` > Settings > Secrets and variables > Actions, add:
  - `APP_ID`: the App ID;
  - `APP_PRIVATE_KEY`: a generated private key (the full `.pem`).
- The App token makes the tag trigger `release.yml` and the release PR
  trigger `test.yml`. `GITHUB_TOKEN` events trigger no workflows.

### Homebrew tap

- Create the empty public repo `k4black/homebrew-tap`.
- dist pushes the formula with the secret `HOMEBREW_TAP_TOKEN`, not the App
  token. Create a fine-grained PAT: repository `k4black/homebrew-tap` only,
  Contents read and write. Add it to `k4black/hex` as `HOMEBREW_TAP_TOKEN`.

### PyPI

- pypi.org > Account > Publishing > add a pending trusted publisher:
  - PyPI project name: `hex-cli`
  - Owner: `k4black`
  - Repository: `hex`
  - Workflow name: `release.yml` (the caller; `publish-pypi.yml` is a
    reusable workflow and PyPI checks the top-level one)
  - Environment: empty (the repo uses no environments; secrets live at repo level)

### crates.io

- Trusted publishing needs the crate to exist, so the first publish uses a token:
  - crates.io > Account Settings > API Tokens: scopes `publish-new` and
    `publish-update`, short expiry.
  - Add it to `k4black/hex` as `CARGO_REGISTRY_TOKEN`.
- After 0.1.0 is out, for each of `hex-proto`, `hex-kernel`, `hex-worker`,
  `hex-runtime`, `hex-cli`: crate Settings > Trusted Publishing > add GitHub:
  - Owner: `k4black`
  - Repository: `hex`
  - Workflow filename: `release-plz.yml`
  - Environment: empty
- Then delete the `CARGO_REGISTRY_TOKEN` secret and revoke the token.
  Without it, release-plz uses trusted publishing (OIDC).

### npm

- npmjs.com > Access Tokens > Generate a granular token:
  - Packages and scopes: read and write, scope `@k4black` (the package does not
    exist yet, so select the scope, not a package);
  - expiry 90 days.
- Add it to `k4black/hex` as `NPM_TOKEN`. Renew it before it expires.

### main-branch ruleset

- Settings > Rules > Rulesets > New branch ruleset, target `main`:
  - Require a pull request before merging.
  - Require status checks to pass, with these checks (from `test.yml`):
    - `lint`
    - `test (ubuntu-latest)`
    - `test (macos-latest)`
- Rename a job in `test.yml` and the ruleset blocks every PR until you update it.
