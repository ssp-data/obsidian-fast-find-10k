# Fast Find 10K — build, test, install into a vault, release.
#
#   make build                      wasm + plugin → plugin/dist/{main.js,manifest.json,styles.css}
#   make test                       Rust (unit, golden, typo recall) + plugin tests
#   make install VAULT=~/my-vault   symlink plugin/dist into <vault>/.obsidian/plugins/fast-find
#   make recall VAULT=~/my-vault    title / two-word / one-typo recall over that vault (ff10k recall)
#   make release-notes VERSION=0.1.1  draft RELEASE_NOTES.md from the commits since the last tag (Claude Code
#                                     when `claude` is installed, else a grouped commit list); review, commit
#   make release VERSION=0.1.1      requires committed RELEASE_NOTES.md newer than the last tag; bumps
#                                   manifest.json, versions.json, plugin/package.json; builds; commits; tags
#                                   and pushes — the Release workflow then builds, attests and publishes
#                                   with RELEASE_NOTES.md as the release text
#   make release-local VERSION=…    publish the local build with `gh` (no attestations)
#
# `make release` refuses on a dirty tree or when the tag exists. The tag has no `v` prefix, as
# Obsidian's community plugin list expects.

SHELL := /bin/bash
.DEFAULT_GOAL := build

PLUGIN_DIR := plugin
DIST       := $(PLUGIN_DIR)/dist
ASSETS     := $(DIST)/main.js $(DIST)/manifest.json $(DIST)/styles.css
CLI        := target/release/ff10k

.PHONY: build wasm plugin cli test test-rust test-plugin install uninstall recall release release-notes release-local check-clean clean

build: wasm plugin

wasm:
	bash scripts/build-wasm.sh
	node scripts/smoke-wasm.mjs

plugin:
	cd $(PLUGIN_DIR) && npm ci --silent && npm run build

cli:
	cargo build --release -p ff10k-cli

test: test-rust test-plugin

test-rust:
	cargo test --workspace
	cargo test -p ff10k-core --features golden --test golden --test typo_recall

test-plugin:
	cd $(PLUGIN_DIR) && npm test

# Symlinks keep the vault on the latest build: `make plugin` then reload the plugin in Obsidian.
install: build
	@test -n "$(VAULT)" || { echo "usage: make install VAULT=<path to vault>"; exit 1; }
	@test -d "$(VAULT)/.obsidian" || { echo "$(VAULT) has no .obsidian folder"; exit 1; }
	mkdir -p "$(VAULT)/.obsidian/plugins/fast-find"
	ln -sf "$(abspath $(DIST))/main.js" "$(abspath $(DIST))/manifest.json" "$(abspath $(DIST))/styles.css" "$(VAULT)/.obsidian/plugins/fast-find/"
	@echo "installed into $(VAULT)/.obsidian/plugins/fast-find — enable 'Fast Find 10K' under Community plugins"

uninstall:
	@test -n "$(VAULT)" || { echo "usage: make uninstall VAULT=<path to vault>"; exit 1; }
	rm -rf "$(VAULT)/.obsidian/plugins/fast-find"

recall: cli
	@test -n "$(VAULT)" || { echo "usage: make recall VAULT=<path to vault> [EVERY=10]"; exit 1; }
	$(CLI) recall "$(VAULT)" --every $(or $(EVERY),1)

check-clean:
	@test -z "$$(git status --porcelain)" || { echo "working tree not clean"; git status --short; exit 1; }

# Drafts RELEASE_NOTES.md for VERSION from the commits since the last tag (PREV=x.y.z overrides).
# With the `claude` CLI the notes are written in prose (scripts/release-notes-prompt.md); without
# it a grouped commit list is written for editing by hand. Review, then commit the file.
release-notes:
	@test -n "$(VERSION)" || { echo "usage: make release-notes VERSION=x.y.z [PREV=x.y.z]"; exit 1; }
	@PREV="$(PREV)"; test -n "$$PREV" || PREV=$$(git describe --tags --abbrev=0); \
	echo "Drafting RELEASE_NOTES.md for $(VERSION) (changes since $$PREV)…"; \
	if command -v claude >/dev/null; then \
	  sed -e "s/{{VERSION}}/$(VERSION)/g" -e "s/{{PREV}}/$$PREV/g" scripts/release-notes-prompt.md \
	    | env -u ANTHROPIC_API_KEY claude -p --allowedTools "Read,Write,Bash(git log:*),Bash(git diff:*),Bash(git describe:*),Bash(head:*)"; \
	else \
	  { echo "## What has changed?"; echo; echo "<intro>"; echo; \
	    for t in feat fix docs chore ci test; do \
	      lines=$$(git log --no-merges --format='%s' "$$PREV..HEAD" | grep -E "^$$t(\(.*\))?:" | sed -E "s/^$$t(\(.*\))?: /- /"); \
	      test -z "$$lines" || { echo "**$$t**"; echo; echo "$$lines"; echo; }; \
	    done; } > RELEASE_NOTES.md; \
	  echo "claude CLI not found: wrote a grouped commit list; edit it into prose"; \
	fi
	@echo; echo "Review RELEASE_NOTES.md, then: git add RELEASE_NOTES.md && git commit -m 'release notes $(VERSION)'"

# VERSION=x.y.z; MIN_APP defaults to the current minAppVersion in manifest.json.
release: check-clean
	@test -n "$(VERSION)" || { echo "usage: make release VERSION=x.y.z [MIN_APP=1.5.7]"; exit 1; }
	@! git rev-parse -q --verify "refs/tags/$(VERSION)" >/dev/null || { echo "tag $(VERSION) exists"; exit 1; }
	@PREV=$$(git describe --tags --abbrev=0 2>/dev/null); \
	if [ -n "$$PREV" ] && git cat-file -e "$$PREV:RELEASE_NOTES.md" 2>/dev/null && git diff --quiet "$$PREV" -- RELEASE_NOTES.md; then \
	  echo "RELEASE_NOTES.md is unchanged since $$PREV: run make release-notes VERSION=$(VERSION), review, commit"; exit 1; fi
	@test -s RELEASE_NOTES.md || { echo "RELEASE_NOTES.md is missing: run make release-notes VERSION=$(VERSION)"; exit 1; }
	node scripts/bump-version.mjs "$(VERSION)" "$(MIN_APP)"
	$(MAKE) build
	git add manifest.json versions.json $(PLUGIN_DIR)/package.json $(PLUGIN_DIR)/package-lock.json
	@git diff --quiet --cached && echo "version files already at $(VERSION), nothing to commit" || git commit -m "release $(VERSION)"
	git tag -a "$(VERSION)" -m "Fast Find 10K $(VERSION)"
	git push && git push origin "$(VERSION)"
	@echo "tag $(VERSION) pushed: .github/workflows/release.yml builds, attests and publishes the release (gh run watch to follow)"

# Fallback when CI is unavailable: publish the local build without attestations.
release-local: build
	@test -n "$(VERSION)" || { echo "usage: make release-local VERSION=x.y.z"; exit 1; }
	gh release create "$(VERSION)" $(ASSETS) --title "$(VERSION)" --notes "Fast Find 10K $(VERSION). Install with BRAT (ssp-data/obsidian-fast-find-10k) or copy main.js, manifest.json and styles.css into <vault>/.obsidian/plugins/fast-find/."

clean:
	rm -rf $(DIST) crates/ff10k-wasm/pkg
	cargo clean
