<p align="center">
  <img src="assets/SnenkBotIcon.svg" alt="SnenkBot logo" width="128" height="128" />
</p>

# SnenkBot

A desktop workflow app for streaming, built with Rust, Tauri and Svelte.

Connect Twitch, OBS Studio and VTube Studio, then combine triggers and actions into automations. SnenkBot supports chat commands, channel events, recording chapters, reusable workflows and Lua scripts, with a visual editor and run history.

## Download

Get the installers from [Releases](https://github.com/FaeyUmbrea/SnenkBot/releases).

On macOS, place **SnenkBot.app** in Applications. Public macOS builds are not yet Developer ID signed or notarized; Windows installers are not yet certificate signed.

## Getting started

Open the app and configure the integrations you use:

- **Streaming Services:** sign in to Twitch with your broadcaster account and, optionally, a separate bot account.
- **Broadcast Apps:** enable OBS Studio and enter its WebSocket address and password.
- **Devices & Tools:** configure and authorize VTube Studio if you use it.

Use **Automations** to create workflows and **Run History** to inspect their results. Credentials stay in your operating system's credential store; workflow data is saved locally.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Bugs and project work are tracked in [YouTrack](https://tasks.void.monster/projects/SNENKBOT).

The [maintenance reference](https://tasks.void.monster/articles/SNENKBOT-A-4) describes the current architecture, development and release steps, and unfinished Version 1 work.

## License

SnenkBot is licensed under [GPLv3](LICENSE). Inter Tight is included under the [SIL Open Font License](assets/fonts/OFL.txt). The shared Snenk dragon symbol is CC0.
