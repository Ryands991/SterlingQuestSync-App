# Sterling Quest Sync, app

**Status:** Still in heavy development and not finished yet. Things are mostly settled now, but big changes can still come with an update.

Sterling began as a little project for me and a few close friends, so we could keep our quests in step while we adventure together, even with different schedules. It's been a lot of fun to build, so I'm sharing it here in case anyone else finds it useful. It's shaped by how we like to play, and it gets updates whenever I can find the time. Ideas and bug reports are always welcome.

> Built by Kael with the help of Claude Opus, an AI model by Anthropic. The ideas, testing and decisions are mine; much of the code was written with Claude. Everything is tested in game with friends.

## What this is

A small desktop app that carries [Sterling Quest Tracker](https://github.com/SterlingAddons/SterlingQuest)'s sync between friends whose play times don't overlap. When you log off it uploads your latest copy to the [sync server](https://github.com/SterlingAddons/SterlingQuestSync-Server); your friend's app picks it up before they log in.

## What it can touch on your PC

Hard-coded, not settings, and covered by tests (`core/src/safety.rs`):

- Writes only to `Interface\AddOns\SterlingQuestSync`, to `Interface\AddOns\Sterling` during a Sterling update, and to its own data folder.
- **WTF is read-only.** The app reads the small `SterlingQuestSync.lua` save file and copies Sterling's saves into its own backups. It never writes, moves or deletes anything in WTF.
- Never touches any other add-on. An old Sterling is moved aside on update, never deleted.

## Layout

- `core/`: everything except the window (file safety, save reader, inbox writer, sync engine, server client, Sterling updater). `cargo test` runs it.
- `app/`: the Tauri window and the commands the page may call. The page has no file, shell or network access of its own.
- `app/companion/`: the tiny `SterlingQuestSync` add-on the app installs.

## License

MIT. See [LICENSE](LICENSE).
