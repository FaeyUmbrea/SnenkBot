# Overlay API

SnenkBot's optional HTTP API serves named overlays and other local clients. It is disabled by default. Open **Settings → General → API server** to set the IP host and port and enable the listener; the default address when enabled is `http://127.0.0.1:38764`. The server settings stay available even before you enable them. Select the **Consumers** section and choose **Add consumer** for each client, grant only the needed event channels and provider operations, and copy the bearer token when it is shown. New consumer tokens are kept in the operating system credential store; portable configuration contains their digest and a credential reference. You can edit a consumer's name, permissions, and allowed origins without changing its token.

If a token is lost, edit that consumer and choose **Regenerate token**. Confirm the replacement, then update the clients and hosting URLs that used the old token. The consumer keeps its identity, permissions, origins, hosted directory, settings and secrets. The old token stops authenticating immediately; regeneration does not recreate the consumer or reset its data. It does not update external clients automatically.

The consumer editor uses the same settings controls as the connection and provider screens. The **Consumers** section loads when opened; switching back to **API server** keeps an unsaved consumer draft in place. Select channels and provider operations from the available choices; event, lookup and origin sections show their selected count and stay collapsed until you open them. Larger lookup catalogues can be searched by service or operation. Add allowed browser origins and readable or writable secret names as individual entries. Leave either secret list empty unless the client needs it. OBS and provider credentials have separate fields beside their settings and do not become portable configuration values.

Local files (opaque `null` origins), OBS local sources (`http://absolute`) and pages hosted by this SnenkBot API work automatically without origin entries. Every request still requires its consumer's bearer token and permissions.

Register only additional websites in the consumer's allowed browser origins, including the scheme and nondefault port with no path. Other browser origins are rejected. Nonbrowser clients may omit `Origin`. API requests use the Authorization header; file-hosting URLs carry the consumer token in their path so browsers can navigate to them.

## Events

Connect with `GET /api/v1/events?channels=twitch.chat,twitch.support,twitch.channel` and `Authorization: Bearer <token>`. Request only channels granted to that consumer. The response is a `text/event-stream` whose `data:` lines contain JSON frames:

```json
{"kind":"snapshot","channel":"twitch.chat","cursor":"opaque:0","data":[]}
```

Every frame has `kind`, `channel`, `cursor`, and `data`. `snapshot` supplies current state, `event` adds one item, and `reset` replaces a channel's state after a cursor gap, restart, or broadcaster change. The API may also send `status` frames. Chat and support snapshots or resets are arrays; their event frames contain one item. The channel snapshot or reset is the current game object `{ "id": "...", "name": "...", "broadcasterId": "..." }`, or `null` until known. A channel event contains the same object. Keep each channel's latest cursor and reconnect with a URL-encoded JSON map in `cursors`, for example `{"twitch.chat":"...","twitch.support":"..."}`. Treat cursors as opaque; apply a reset instead of trying to fill its gap. The stream sends keepalives during quiet periods.

The chat item contains `id`, `userId`, `login`, `user`, `color`, and `parts`. Each part contains text and, for a native Twitch emote, an image URL. The support item contains `id`, `channelId`, `kind` (`sub`, `resub`, `gift`, or `bits`), `user`, `timestamp`, and optional `tier`, `amount`, and `message`. It comes from Twitch's dedicated [subscription, gift, and cheer EventSub types](https://dev.twitch.tv/docs/eventsub/eventsub-subscription-types/). Gift deliveries are omitted in favor of the aggregate gift event, so one gift is not counted twice.

EventSub subscriptions follow permitted client and workflow demand, with standing demand for support history. The API shares SnenkBot's existing Twitch connection with workflow triggers. Chat keeps the latest 40 messages in memory. Support recording retains up to the configured number of events per broadcaster on disk (500 by default); support events are recorded automatically for the configured broadcaster, even with no connected consumers or with the HTTP listener disabled. A broadcaster account with older Twitch scopes may need reconnection for subscription and cheer events. Reconnect it in **Streaming Services** to grant `channel:read:subscriptions` and `bits:read`; existing authorized workflows continue while those scopes are unavailable.

### Inspecting and replaying received events

The desktop **Events** tab keeps the latest 1,000 received Twitch, OBS Studio, and VTube Studio events, excluding chat messages. This list is separate from both the overlay API stream and **Run History**. It contains events SnenkBot actually received through its active integrations; it does not subscribe to extra feeds just to fill the list. Events without a replayable payload remain visible with replay disabled.

Select **Replay** to send a recorded event through the current matching automation triggers. Applicable Twitch support and channel updates also reach their enabled API channels. Select **Replay to…** to choose specific named automations and overlay consumers eligible for that event. OBS Studio and VTube Studio events have automation targets only. Consumer delivery respects each consumer's current channel grants and reaches only that named consumer; a reconnect with its prior cursor can receive the replay while it remains buffered. Replaying support events does not add another entry to durable support history.

### Sending synthetic and custom events

For a quick test, open **General → API server → Send test event…**, select the consumer and event type, and choose **Send**. The fields use registered defaults and can be edited; the dialog stays open for repeated sends. Only senders supported by the selected consumer’s current channel grants are available. No automation needs to be created.

The same senders remain available as API workflow actions and through the existing Lua action interface:

| Action | Delivery |
| --- | --- |
| `api.send_chat` | A sample chat message on `twitch.chat` |
| `api.send_support` | A subscription, resubscription, gift or bits event on `twitch.support` |
| `api.send_game` | A game change on `twitch.channel` |
| `api.send_custom` | A named JSON payload on `api.custom` |

Sample values are supplied; only the recipients need selecting for a basic test. Consumer choices store stable IDs and show their names. Every selected consumer must currently grant the relevant channel. A missing or revoked recipient rejects the action before anything is sent. A disabled or unavailable listener fails the action immediately.

These deliveries go only to the selected consumers. They do not change the real Twitch category, enter received-event or support history, or activate other automations. A sender run as an automation action appears in ordinary Run History; a direct test does not create an automation run. A connected consumer can resume buffered deliveries after a brief disconnect using its existing cursor. Synthetic events are not durable history.

Custom event data has `{ "id": "...", "name": "confetti", "payload": { "color": "gold" } }`. Enable **Custom events** for that consumer and subscribe to `api.custom`. Names and JSON payloads are application-defined; the transport channel remains `api.custom`. The stream-overlay client opts in with `customEvents: true` in its config and dispatches `snenkbot:event` on `window`; its README shows a component listener. Snapshots and resets do not trigger custom effects again. Payloads are limited to 256 KiB.

The Rust API module owns action definitions, choices and delivery. To add another native sender, declare its configuration with `ConfigSchema` and reuse `ApiService::send_event_to_consumers`; keep service-specific normalization in that sender. Register output-only test eligibility alongside its action definition with `Engine::register_event_test_action`; the desktop test dialog discovers its fields and defaults without a frontend event list. The reusable JSON input and collection choices belong to the common builder.

## Hosting static files with automatic reload

In **General → Consumers**, edit a consumer and use **Browse…** beside **Static folder** to choose an existing folder on the SnenkBot machine, or enter its absolute path. The picker changes the draft; choose **Save changes** to persist it. Canceling the picker leaves the draft unchanged. Leave the field unset to keep hosting disabled. Enable the API server, then choose **Copy hosting URL** in that consumer's settings and paste it into OBS. The URL has this form:

```text
http://<SnenkBot-host>:<port>/files/<consumer-token>/index.html
```

This is the existing API listener, not another service. Treat the hosting URL like the consumer token. The copy button reads the retained token from native credential storage and writes the complete URL to the clipboard. Opening settings does not read credentials. Older consumers whose tokens were never retained can still authenticate; regenerate their token to enable repeatable URL copying. Copying never rotates a token automatically. Revoking the consumer immediately blocks new file requests and ends its reload stream.

Consumer buttons come from the `actions` declaration on `CreateApiConsumer`; their labels and confirmation copy are native metadata consumed by the shared collection editor. The `directory` field annotation selects the shared path input and Browse control. Both commands validate the live owning settings session before copying credentials, changing a token or opening a picker. Other configuration types can use these same controls without adding integration-specific frontend screens.

For stream-overlay, serve its `build-local/` folder and keep its adjacent `config.js`. Pages hosted by SnenkBot are accepted automatically; no browser-origin entry is needed. The configured API address must be reachable from OBS. The folder is served under a URL prefix, so other static sites must use relative asset URLs or be built for that prefix.

Changes to files in the served folder automatically reload connected pages. A native watcher groups rapid writes into one refresh and reconnecting pages detect changes made while disconnected. Source edits still need your project's normal build step; SnenkBot serves the resulting files and does not run a compiler. Use a development watch build if you want source edits to produce output automatically. Automatic reload is available to pages that permit the injected same-origin script.

Hosting is read-only, has no directory listings, and confines file and index resolution to the selected folder. Files and reload streams require the current consumer token on every request. Responses prevent caching and credential-bearing referrers. Other consumers do not gain access to this folder. Old consumer records load with hosting disabled; their registry version advances on the next ordinary save without changing their tokens or grants.

## Consumer settings and secrets

Settings belong to the authenticated consumer. `GET /api/v1/settings/{key}` returns `{ "value": null, "revision": 0 }` when absent, or the stored JSON value and revision. `PUT` accepts `{ "value": <JSON>, "expectedRevision": <number> }` and returns the new value and revision. A stale revision returns HTTP 409, so read again before retrying. Consumer settings are isolated even when two consumers use the same key.

Secrets belong to the authenticated consumer and are held in the operating system credential manager. Grant each readable and writable secret name explicitly in the desktop UI. `GET /api/v1/secrets/{key}` returns `{ "value": <string-or-null> }` only with read permission. `PUT` accepts `{ "value": <string-or-null> }` only with write permission; `null` deletes the secret. Ordinary overlays do not need secret permissions. Provider credentials are administered separately under **Settings → Devices & Tools** and are never exposed through consumer secret routes.

## Lookup providers

Under **Settings → Devices & Tools**, open a provider to configure it. Its settings stay available even before you enable it. FrankerFaceZ and BetterTTV have independent enable settings; the compatible `emotes/resolve` operation combines whichever sources are enabled. Each provider has its own enable setting; SteamGridDB also needs its own API key. Existing combined provider settings continue to load when valid. Saving an individual provider writes its own settings while leaving the older combined document unchanged. If one provider's saved settings cannot be read, its entry shows an error; providers with readable individual settings remain available. Repair or restore the affected document, then retry its settings. A consumer also needs an explicit grant for each operation. Call `POST /api/v1/providers/{provider}/{operation}` with JSON `{ "arguments": { ... } }`; successful responses are the operation's JSON result directly. Available operations are `steamgriddb/search`, `steamgriddb/icons`, `steamgriddb/resolve`, `ffz/resolve`, `bttv/resolve`, `emotes/resolve`, and `pronouns/resolve`. For example, `steamgriddb/resolve` accepts `{"arguments":{"name":"A game"}}`, `emotes/resolve` accepts `{"arguments":{"channelId":"123"}}`, and `pronouns/resolve` accepts `{"arguments":{"login":"viewer"}}`. Provider errors return an HTTP status, including 429 when capacity is full and 503 when a provider is disabled or needs configuration.

## Existing support history and recovery

To retain a previous stream-overlay support ticker, open **Events → Import history…** and select the old overlay's `data/events.json`. The file must have `{ "version": 1, "events": [...] }`, be no larger than 2 MB, and contain valid support events. SnenkBot validates the selection before writing, deduplicates event IDs, partitions events by broadcaster, and applies the configured retention. The original file remains unchanged. The import is optional and is never performed automatically.

If an API configuration, consumer registry, or support-history document is corrupt or from an unsupported version, SnenkBot reports it instead of overwriting it. The affected API feature stays unavailable until the file is repaired or restored; unrelated workflows and provider settings remain available. After repairing a settings document, choose **Retry settings** to open it again. A listener bind error appears in the app's connection status. Check the configured host and port before changing the file or recreating a consumer.
