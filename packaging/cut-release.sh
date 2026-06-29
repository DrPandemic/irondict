#!/usr/bin/env bash
#
# Cut a new irondict release: bump the version, sign a GPG tag, and finalize the
# AUR packaging (PKGBUILD checksum + .SRCINFO). Uploads a detached PGP signature
# of the tarball as a GitHub release asset so makepkg can verify it.
#
# Interactive — proposes a minor bump and asks to confirm before touching
# anything. Run it from anywhere inside the repo:
#
#     packaging/cut-release.sh
#
# It performs two commits on `main`:
#   1. "vX.Y.Z"                                  — Cargo + PKGBUILD version bump
#   2. "Set vX.Y.Z release checksum ..."         — real tarball sha + .SRCINFO
# The release tarball only exists once the tag is pushed, so the checksum
# necessarily lands in the second commit (this mirrors the manual process).

set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }

# --- preflight ---------------------------------------------------------------

for tool in git cargo curl sha256sum makepkg gpg gh; do
    command -v "$tool" >/dev/null || die "missing required tool: $tool"
done

# Require a GPG signing key to be configured.
SIGNKEY=$(git config user.signingkey) || die "git config user.signingkey is not set"
[[ -n $SIGNKEY ]] || die "git config user.signingkey is empty"

ROOT=$(git rev-parse --show-toplevel) || die "not inside a git repository"
cd "$ROOT"

CARGO_TOML="Cargo.toml"
PKGBUILD="packaging/aur/PKGBUILD"
SRCINFO="packaging/aur/.SRCINFO"
[[ -f $CARGO_TOML && -f $PKGBUILD ]] || die "run from the irondict repo (Cargo.toml / PKGBUILD not found)"

[[ -z "$(git status --porcelain)" ]] || die "working tree is dirty — commit or stash first"

BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [[ $BRANCH != main ]]; then
    read -rp "Not on 'main' (on '$BRANCH'). Continue anyway? [y/N] " ans
    [[ ${ans:-} == [yY] ]] || die "aborted"
fi

# Owner/repo for the GitHub archive URL, derived from the origin remote.
SLUG=$(git remote get-url origin | sed -E 's#(git@github.com:|https://github.com/)##; s#\.git$##')
[[ $SLUG == */* ]] || die "could not derive owner/repo from origin remote"

# --- pick the new version ----------------------------------------------------

# Current version lives under [workspace.package] in Cargo.toml.
CUR=$(awk '/^\[workspace\.package\]/{f=1} f&&/^version[[:space:]]*=/{gsub(/[^0-9.]/,"");print;exit}' "$CARGO_TOML")
[[ $CUR =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "could not read current version (got '$CUR')"

IFS=. read -r MA MI _ <<< "$CUR"
PROPOSED="$MA.$((MI + 1)).0"

echo "Current version: $CUR"
read -rp "New version [$PROPOSED]: " NEW
NEW=${NEW:-$PROPOSED}
[[ $NEW =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "invalid version: '$NEW' (expected X.Y.Z)"
[[ $NEW != "$CUR" ]] || die "new version matches current version"

TAG="v$NEW"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && die "tag $TAG already exists"

echo
echo "About to cut $CUR -> $NEW:"
  echo "  • bump $CARGO_TOML + Cargo.lock + $PKGBUILD"
  echo "  • sign and push signed GPG tag $TAG"
  echo "  • fetch the $TAG tarball, set its sha256, .asc sign, upload to GitHub release"
read -rp "Proceed? [y/N] " ans
[[ ${ans:-} == [yY] ]] || die "aborted"

# --- bump (commit 1) ---------------------------------------------------------

# Cargo.toml: replace the version only inside the [workspace.package] section.
awk -v new="$NEW" '
    /^\[/ { sect = $0 }
    sect == "[workspace.package]" && /^version[[:space:]]*=/ { sub(/"[^"]*"/, "\"" new "\"") }
    { print }
' "$CARGO_TOML" > "$CARGO_TOML.tmp" && mv "$CARGO_TOML.tmp" "$CARGO_TOML"

# Refresh the workspace members' versions in the lockfile (no recompile needed).
cargo update --workspace --quiet

# PKGBUILD: new pkgver, and a placeholder checksum until the tarball exists.
sed -i "s/^pkgver=.*/pkgver=$NEW/" "$PKGBUILD"
sed -i "s/^sha256sums=.*/sha256sums=('SKIP' 'SKIP')/" "$PKGBUILD"
( cd packaging/aur && makepkg --printsrcinfo > .SRCINFO )

git add "$CARGO_TOML" Cargo.lock "$PKGBUILD" "$SRCINFO"
git commit -qm "$TAG"
git push origin "$BRANCH"

# --- tag ---------------------------------------------------------------------

git tag -s "$TAG" -m "$TAG"
git push origin "$TAG"

# --- checksum + .SRCINFO (commit 2) ------------------------------------------

URL="https://github.com/$SLUG/archive/refs/tags/$TAG.tar.gz"
TARBALL=$(mktemp --suffix=.tar.gz)
ASC="v${NEW}.tar.gz.asc"
trap 'rm -f "$TARBALL" "$ASC"' EXIT

echo "Fetching release tarball..."
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    if curl -fsSL "$URL" -o "$TARBALL"; then
        break
    fi
    [[ $attempt -lt 10 ]] || die "could not download $URL after the tag push"
    echo "  not ready yet, retrying ($attempt)..."
    sleep 3
done

SHA=$(sha256sum "$TARBALL" | cut -d' ' -f1)
echo "sha256: $SHA"

echo "Signing tarball with GPG..."
gpg --detach-sign --armor --local-user "$SIGNKEY" --output "$ASC" "$TARBALL"

echo "Creating GitHub release and uploading .asc signature..."
gh release create "$TAG" --title "$TAG" --notes "" "$ASC"

sed -i "s/^sha256sums=.*/sha256sums=('$SHA' 'SKIP')/" "$PKGBUILD"
( cd packaging/aur && makepkg --printsrcinfo > .SRCINFO )

git add "$PKGBUILD" "$SRCINFO"
git commit -qm "Set $TAG release checksum and regenerate .SRCINFO"
git push origin "$BRANCH"

echo
echo "Released $TAG (signed)."
echo "Build locally with: (cd packaging/aur && makepkg -si)"
