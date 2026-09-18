# Landing contract

Every push to `main` has two deploy targets, and a landing is complete only when both hold.
This binds EVERY landing, test-only and doc-only changes included: the TV binary must equal
the release build of HEAD, not merely behave like it. Both or neither — a lagging TV binary
is an unfinished landing, not a done one.

1. The TV binary at `/home/a/lightherder/lightherder` equals `cargo build --release` of `main`
   HEAD (same sha256). `scripts/landing deploy` installs it by rename swap and launches
   nothing; `run-lightherder.sh` picks the binary up on its next start.
2. The `pages` workflow is green on that same sha. It runs on push; if it is red, say so
   rather than re-triggering blindly.
3. No code comments. Prose lives here or in the README.

`scripts/landing check` proves 1 and 2; it is the `landing` entry of `test-map.json`, which
the botq gate runs before it accepts a `done`.

## Boundaries

This lightherder repository names only its own components. Name another project only as a declared, versioned dependency, never through its internals. Give a needed shared service a neutral name owned by this project. Do not import the environment of machines running agents: hostnames, addresses, paths outside the repository, service or queue names, credentials, camera frames, or renders of private places. No person's name, schedule or presence enters the repository. Before landing, grep the diff for other projects' names and host details. Remove host details and undeclared project references; dependency declarations expose only the dependency's name and version.
