# Agent notes

## heavy

Tests, Clippy and the UI check take a machine-wide slot. Run them through this Makefile (`make test`, `make clippy`, `make ui-check`, `make ui-build`). A one-off command is `heavy -- <command>`.

`heavy` is installed with `brew install mokevnin/tap/heavy`. On CI the `CI` variable is set, so a wrapped command runs at once.

## easement

`easement.yml` is the architecture: one component per crate, plus `ui`, `ui-api` and `ui-mocks`. Routes, features and components import each other, so they stay one component. Use the easement skill before adding a file or an import, and whenever `easement check` fails.

After changing code, run `easement check`. It must exit 0.

Do not edit `easement.yml`, regenerate the baseline or add an `easement:disable` comment unless a human asks.
