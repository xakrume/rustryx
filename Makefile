# Heavy local checks share machine-wide slots with other checkouts.
# CI sets CI and runs cargo directly.

.PHONY: test clippy ui-check ui-build

test: ## Workspace tests
	heavy -- cargo test --workspace --locked

clippy: ## Clippy, warnings denied
	heavy -- cargo clippy --workspace --all-targets --locked -- -D warnings

ui-check: ## UI typecheck, lint, unit tests, license check
	heavy -- npm --prefix ui run check

ui-build: ## UI production build
	heavy -- npm --prefix ui run build
