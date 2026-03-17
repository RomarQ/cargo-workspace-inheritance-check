#!/usr/bin/env bash
set -euo pipefail

# Manual release fallback for cargo-workspace-inheritance-check
#
# Preferred method: Use the GitHub Actions "Release" workflow (workflow_dispatch).
# This script is a fallback for publishing manually.
#
# Usage: ./scripts/release.sh <patch|minor|major>

BUMP="${1:?Usage: ./scripts/release.sh <patch|minor|major>}"

# Validate bump type
if [[ ! "$BUMP" =~ ^(patch|minor|major)$ ]]; then
  echo "error: bump type must be patch, minor, or major"
  exit 1
fi

# Ensure working directory is clean
if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working directory is not clean, commit or stash changes first"
  exit 1
fi

# Ensure on main branch
BRANCH="$(git branch --show-current)"
if [[ "$BRANCH" != "main" ]]; then
  echo "error: must be on main branch (currently on ${BRANCH})"
  exit 1
fi

# Calculate new version
CURRENT=$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"

case "$BUMP" in
  major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
  minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
  patch) PATCH=$((PATCH + 1)) ;;
esac

VERSION="${MAJOR}.${MINOR}.${PATCH}"
echo "Releasing v${VERSION} (was v${CURRENT})..."

# Run checks
echo "Running checks..."
cargo fmt --check
cargo clippy -- -D warnings
cargo test

# Update version in Cargo.toml
echo "Updating Cargo.toml version to ${VERSION}..."
sed -i.bak "s/^version = \"${CURRENT}\"/version = \"${VERSION}\"/" Cargo.toml
rm -f Cargo.toml.bak

# Verify it builds with new version
cargo build

# Commit, tag, and push
git add Cargo.toml Cargo.lock
git commit -S -m "release: v${VERSION}"
git tag -s "v${VERSION}" -m "v${VERSION}"
git push origin main "v${VERSION}"

# Publish to crates.io
echo "Publishing to crates.io..."
cargo publish

echo ""
echo "Done! v${VERSION} published."
