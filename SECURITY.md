# Security

Found a way someone could read, change or wipe other people's data, get code into the game, or touch files the app shouldn't? Please report it privately:

- **[Report a security problem](https://github.com/SterlingAddons/SterlingQuestSync-App/security/advisories/new)** (GitHub private report, only the owner sees it).

Please don't open a public issue for these. Expect a reply within a few days; this is a hobby project.

For someone misusing the app (unwanted syncing, a misused pairing code), use the **Report abuse** issue form instead.

## How the app is kept safe

- The app only ever writes to its own add-on folder, Sterling's folder (during an update) and its own data folder. The game's `WTF` folder is read-only, checked after resolving links, junctions, short names and letter case. Tests for this run on Windows on every build.
- `Inbox.lua` is built from a fixed template, and every value is checked against a strict pattern before it's written.
- App updates are signed, and each signature names its version.
- The sync server only passes quest data between two accounts that paired with a code one of them handed the other.

The latest security audit is summarised in the release notes and the project docs.
