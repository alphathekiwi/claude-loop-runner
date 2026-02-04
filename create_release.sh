#!/bin/bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Get the directory where the script is located
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CARGO_TOML="$SCRIPT_DIR/Cargo.toml"

# Check if Cargo.toml exists
if [[ ! -f "$CARGO_TOML" ]]; then
    echo -e "${RED}Error: Cargo.toml not found at $CARGO_TOML${NC}"
    exit 1
fi

# Extract current version from Cargo.toml
CURRENT_VERSION=$(grep -m1 '^version = ' "$CARGO_TOML" | sed 's/version = "\(.*\)"/\1/')

if [[ -z "$CURRENT_VERSION" ]]; then
    echo -e "${RED}Error: Could not find version in Cargo.toml${NC}"
    exit 1
fi

echo -e "${YELLOW}Current version: ${NC}$CURRENT_VERSION"

# Parse version components
IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT_VERSION"

# Determine bump type (default to patch)
BUMP_TYPE="${1:-patch}"

case "$BUMP_TYPE" in
    major)
        MAJOR=$((MAJOR + 1))
        MINOR=0
        PATCH=0
        ;;
    minor)
        MINOR=$((MINOR + 1))
        PATCH=0
        ;;
    patch)
        PATCH=$((PATCH + 1))
        ;;
    *)
        echo -e "${RED}Error: Invalid bump type '$BUMP_TYPE'. Use: major, minor, or patch${NC}"
        exit 1
        ;;
esac

NEW_VERSION="$MAJOR.$MINOR.$PATCH"
echo -e "${GREEN}New version: ${NC}$NEW_VERSION"

# Confirm with user
read -p "Create release v$NEW_VERSION? (y/N) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 0
fi

# Update version in Cargo.toml
if [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS requires empty string for -i
    sed -i '' "s/^version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/" "$CARGO_TOML"
else
    # Linux
    sed -i "s/^version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/" "$CARGO_TOML"
fi

echo -e "${GREEN}Updated Cargo.toml${NC}"

# Update Cargo.lock by running cargo check
echo "Updating Cargo.lock..."
cargo check --quiet

# Commit the version bump
git add "$CARGO_TOML" "$SCRIPT_DIR/Cargo.lock"
git commit -m "chore: bump version to $NEW_VERSION"

echo -e "${GREEN}Committed version bump${NC}"

# Create and push tag
TAG="v$NEW_VERSION"
git tag "$TAG"
echo -e "${GREEN}Created tag $TAG${NC}"

# Push commit and tag
git push origin HEAD
git push origin "$TAG"

echo -e "${GREEN}Pushed to GitHub${NC}"
echo ""
echo -e "${GREEN}Release v$NEW_VERSION created!${NC}"
echo "GitHub Actions will now build and publish the release."
echo "Check progress at: https://github.com/$(git remote get-url origin | sed 's/.*github.com[:/]\(.*\)\.git/\1/')/actions"
