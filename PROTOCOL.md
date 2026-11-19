# Nexus protocol v2

The Windows app (`desktop/`, Rust) and the Android app (`mobile/`, Flutter) speak this protocol. Both apps check their code against the same test vectors from `tools/vectors.py`.

## Identity

Each device has a long-term X25519 key pair. Its **ID** is the first 8 bytes of SHA-256(public key), written as 16 hex characters. Its **fingerprint** is the same 8 bytes in uppercase, split into groups of 4.

## Discovery: UDP port 47800

Every 2 seconds, each device broadcasts this JSON message to `255.255.255.255` and to the broadcast address of each of its networks:

```json
{"app":"nexus","v":1,"id":"…","name":"Gaming PC","os":"Windows","public_key":"<64 hex>","tcp_port":47801,"macs":["aa:bb:cc:dd:ee:ff"]}
```

A receiver ignores the message if `id` doesn't match `public_key`. A device counts as present for 7 seconds after its last announcement. `macs` (optional) lists the device's MAC addresses; a paired device remembers them so it can send a Wake-on-LAN packet later. Phones send an empty list.

## Control channel: TCP port 47801

Each message is one line of JSON. The `t` field gives its type. Either side can send `{"t":"error","reason":"…"}` at any time.

`‖` below means joining fields, each one preceded by its length as a 4-byte big-endian number.

### Pairing (numeric comparison with a commitment)

A is the device where someone tapped **Pair**. B is the device with **Allow pairing** turned on. Pairing stays allowed for 2 minutes and accepts at most 3 requests.

| Step | Message |
|---|---|
| A → B | `pair_hello {v:2, id, name, os, public_key: PKa}` |
| B → A | `pair_hello_ack {v:2, id, name, os, public_key: PKb, commit: HMAC-SHA256(key=Nb, "nexus-commit-v2" ‖ PKb ‖ PKa)}` |
| A → B | `nonce {nonce: Na}` (32 random bytes) |
| B → A | `nonce {nonce: Nb}` (32 random bytes). A checks `commit`. |
| both | code = first 4 bytes of SHA-256("nexus-sas-v2" ‖ PKa ‖ PKb ‖ Na ‖ Nb), read as a big-endian number, mod 1 000 000, shown as 6 digits |
| A ↔ B | `decision {accept: true/false}` after each person compares the screens |

Each device saves the other only if both send `accept: true`.

B commits to Nb before it sees Na. That means a device listening in the middle can't make the two codes match, except by a 1-in-a-million chance on each try.

### Status check

| Step | Message |
|---|---|
| A → B | `ping {id: A's id, nonce}` (16 random bytes) |
| B → A | `pong {mac: HMAC-SHA256(key=X25519(B, PKa), "nexus-ping-v2" ‖ nonce ‖ B's id ‖ A's id)}` |

Only the real B can produce the right `mac`. A sends a ping to every paired device every 5 seconds. A result counts for 15 seconds.

A `pong` may also carry live stats, signed on their own so older versions can ignore them:
`stats` (compact JSON `{"cpu":…,"ram":…,"battery":…}`, each a percent or null, and on PCs `"wake"`: `ready` / `wifi` / `off` / `unknown` — whether that PC can be woken with Wake-on-LAN) and
`stats_mac: HMAC-SHA256(key=X25519(B, PKa), "nexus-stats-v2" ‖ nonce ‖ stats)`. A trusts the stats only if `stats_mac` checks out.

### Remote command (lock / sleep / restart / shut down)

| Step | Message |
|---|---|
| A → B | `command {id: A's id, nonce (16 bytes), action: "lock"\|"sleep"\|"restart"\|"shutdown", mac: HMAC-SHA256(key=X25519(A, PKb), "nexus-cmd-v2" ‖ nonce ‖ action ‖ B's id ‖ A's id)}` |
| B → A | `command_ack {ok, message, mac: HMAC-SHA256(key=X25519(B, PKa), "nexus-cmdack-v2" ‖ nonce ‖ message)}` |

B runs the action only if `mac` is valid and A is a paired device, so only a paired device can trigger it. B replies **before** acting, since restart and shut down end its process. Phones can't run these and reply with `ok:false`.

### Wake-on-LAN

To wake a device that is asleep or off, Nexus broadcasts a standard magic packet (6 bytes of `0xFF` followed by the target MAC repeated 16 times) to UDP port 9, to `255.255.255.255` and to each network's broadcast address. The target's MAC comes from the `macs` field it announced while it was on. Waking requires Wake-on-LAN to be enabled in the target's BIOS and network card.

### File transfer

A separate TCP connection carries file browsing and transfer. It opens with a three-message mutual handshake so each side proves it is the paired device (A is the side that connected):

| Step | Message |
|---|---|
| A → B | `fs_hello {id: A's id, nonce: Na}` (16 bytes) |
| B → A | `fs_ready {nonce: Nb, mac: HMAC-SHA256(key=X25519(B, PKa), "nexus-fs-v2" ‖ Na ‖ Nb ‖ B's id ‖ A's id)}` |
| A → B | `fs_auth {mac: HMAC-SHA256(key=X25519(A, PKb), "nexus-fs-v2" ‖ Nb ‖ Na ‖ A's id ‖ B's id)}` |

B refuses the connection unless A is a paired device and `mac` checks out. After the handshake, the connection is trusted and requests flow as plain JSON lines (no `t` field):

- `{op:"list", path}` → `{entries:[{name, dir, size}]}` — `path` is relative to the device's shared root (its home folder on a PC); `..` is refused.
- `{op:"get", path}` → `{size}`, then repeated `{data:"<hex>"}` frames (up to 256 KiB each) ending with `{eof:true}`.
- `{op:"put", name}` → `{ok:true}`; A then sends `{data:"<hex>"}` frames ending with `{end:true}`; B replies `{saved}`. Uploads land in a "Nexus Inbox" folder under the shared root.
- `{op:"bye"}` closes the connection.

Any request may be answered with `{error:"…"}`. File transfers are authenticated so only paired devices can reach them; the file bytes themselves are not yet encrypted on the wire (a planned addition). The maximum JSON line is raised to 8 MiB so a data frame (or a sharp screen frame) fits.

All binary values on the wire are lowercase hex.

### Screen viewing and remote control

A separate TCP connection authenticated exactly like file transfer, using `screen_hello` / `screen_ready` / `screen_auth` and the label `"nexus-screen-v2"`. After the handshake the viewer's first plain JSON line chooses the purpose:

- `{op:"start", fps, maxw, quality?, monitor?}` — the host streams `{op:"frame", w, h, seq, data:"<base64 JPEG>", mon, mons}` of monitor number `monitor` (0 = the main monitor, then the others from left to right; a missing monitor falls back to 0) until the viewer closes the connection. `mon` is the monitor shown and `mons` how many the host has. Control events may carry `m`, the monitor their `x`/`y` fractions refer to (default 0).
- `{op:"control"}` — the host answers `{op:"ready"}` (or `{error}` if "Allow remote control" is off) and then carries out input events from the viewer: `move {x,y}`, `button {b, down, x?, y?}`, `click {b, x, y, n?}`, `scroll {dy?, dx?}`, `text {s}`, `key {k, mods?}`. Positions are fractions 0–1 of the shared screen; `b` is `left`/`right`/`middle`; `mods` may contain `ctrl`, `alt`, `shift`, `win`. The viewer sends `{op:"ping"}` every few seconds while idle and `{op:"bye"}` when done.

- `{op:"monitor"}` — "apps and monitoring". The viewer sends `{op:"snapshot"}` (every ~2 s) and gets `{op:"snapshot", cpu?, ram_used?, ram_total?, uptime?, battery?, cores?, disks:[{name,total,free}], apps:[{name,cpu,mem,count,protected}], can_end, note?}`; `{op:"end", name}` ends every process of that program (only while "Allow remote control" is on; core Windows programs are refused) and is answered `{op:"ended", name, count}` or `{error}`. A phone answers with its battery only, because Android does not let apps see other running apps.
- `{op:"auto"}` — automations (rules a PC runs by itself). Requests: `{op:"list"}` → `{op:"rules", rules:[…], devices:[{id,name,os}], can_edit}`; `{op:"save", rules:[…]}` replaces the whole list (checked by the PC; answered with the new `rules` listing or `{error}`); `{op:"run", id}` runs one now → `{op:"ran", id}`. Saving and running need "Allow remote control". A rule is `{id, name, enabled, when, action, target}` where `when` is `{type:"time", hour, minute, days:[0=Mon…6=Sun]}` (empty days = every day), `{type:"idle", minutes}` or `{type:"startup"}`; `action` is `{type:"lock"|"sleep"|"restart"|"shutdown"|"wake"}`, `{type:"open", what}` or `{type:"end_app", name}`; `target` is a paired computer's id for power actions and wake, or null for the PC itself. Phones answer `{error}`.
- `{op:"clip", text}` — shared clipboard: puts `text` (at most 1,000,000 bytes) on the receiving device's clipboard. Answered `{op:"ok"}`, or `{error}` when shared clipboard is turned off there. Each device sends new copied text to its paired devices that are online; text that just arrived is not sent back. Android only lets the app in front read the clipboard, so a phone sends when Nexus is opened or when asked.
- `{op:"clipfiles", items:[{path, size, dir}]}` — files copied in File Explorer, PC to PC. `path` uses "/" between folders and must stay inside the copy (no "..", no drive letters). The receiver answers `{op:"ready"}` (or `{error}`), then for each item that is not a folder, in order, the sender sends `{data: base64}` chunks followed by `{next:true}`, and finally `{done:true}`. The receiver saves everything in a fresh folder inside its data folder, puts the top-level items on its clipboard as files (CF_HDROP), and answers `{op:"ok"}`. At most 4 GB and 10,000 items.
- `{op:"audio"}` — sound sharing: the host records what its speakers play and answers `{op:"audio", rate, channels:2}` (or `{error}`), then sends `{pcm:"<base64 16-bit little-endian stereo samples>"}` chunks, and `{op:"quiet"}` every half second while nothing plays, until the viewer closes the connection. A phone can share its sound too (Android 10+), but only while its screen is shared and the user has allowed audio recording; otherwise it answers `{error}`.

Like file transfer, these connections are authenticated but not yet encrypted on the wire.
