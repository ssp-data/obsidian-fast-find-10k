# Fast Find 10K — build, test, install into a vault, release.
#
#   make build                      wasm + plugin → plugin/dist/{main.js,manifest.json,styles.css}
#   make test                       Rust (unit, golden, typo recall) + plugin tests
#   make install VAULT=~/my-vault   symlink plugin/dist into <vault>/.obsidian/plugins/fast-find
#   make recall VAULT=~/my-vault    title / two-word / one-typo recall over that vault (ff10k recall)
#   make release VERSION=0.1.1      bump manifest.json, versions.json, plugin/package.json; build; commit;
#                                   tag; GitHub release with the three plugin files (needs `gh`)
#
# `make release` refuses on a dirty tree or when the tag exists. The tag has no `v` prefix, as
# Obsidian's community plugin list expects.

SHELL := /bin/bash
.DEFAULT_GOAL := build

PLUGIN_DIR := plugin
DIST       := $(PLUGIN_DIR)/dist
ASSETS     := $(DIST)/main.js $(DIST)/manifest.json $(DIST)/styles.css
CLI        := target/release/ff10k

.PHONY: build wasm plugin cli test test-rust test-plugin install uninstall recall release check-clean clean

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

# VERSION=x.y.z; MIN_APP defaults to the current minAppVersion in manifest.json.
release: check-clean
	@test -n "$(VERSION)" || { echo "usage: make release VERSION=x.y.z [MIN_APP=1.5.7]"; exit 1; }
	@! git rev-parse -q --verify "refs/tags/$(VERSION)" >/dev/null || { echo "tag $(VERSION) exists"; exit 1; }
	@command -v gh >/dev/null || { echo "gh (GitHub CLI) is needed"; exit 1; }
	node scripts/bump-version.mjs "$(VERSION)" "$(MIN_APP)"
	$(MAKE) build
	git add manifest.json versions.json $(PLUGIN_DIR)/package.json $(PLUGIN_DIR)/package-lock.json
	git commit -m "release $(VERSION)"
	git tag -a "$(VERSION)" -m "Fast Find 10K $(VERSION)"
	git push && git push origin "$(VERSION)"
	gh release create "$(VERSION)" $(ASSETS) --title "$(VERSION)" --notes "Fast Find 10K $(VERSION). Install with BRAT (ssp-data/obsidian-fast-find-10k) or copy main.js, manifest.json and styles.css into <vault>/.obsidian/plugins/fast-find/."
	@echo "released $(VERSION)"

clean:
	rm -rf $(DIST) crates/ff10k-wasm/pkg
	cargo clean
