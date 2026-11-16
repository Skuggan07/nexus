# Nexus protocol v2

The Windows app (`desktop/`, Rust) and the Android app (`mobile/`, Flutter) speak this protocol. Both apps check their code against the same test vectors from `tools/vectors.py`.

## Identity

Each device has a long-term X25519 key pair. Its **ID** is the first 8 bytes of SHA-256(public key), written as 16 hex characters. Its **fingerprint** is the same 8 bytes in uppercase, split into groups of 4.

## Discovery: UDP port 47800

Every 2 seconds, each device broadcasts this JSON message to `255.255.255.255` and to the broadcast address of each of its networks:

```json
{"app":"nexus","v":1,"id":"…","name":"Gaming PC","os":"Windows","public_key":"<64 hex>","tcp_port":47801}
```

A receiver ignores the message if `id` doesn't match `public_key`. A device counts as present for 7 seconds after its last announcement.

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

All binary values on the wire are lowercase hex.
