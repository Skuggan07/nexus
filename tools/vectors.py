# Reference implementation of the Nexus v2 pairing/ping math, used to make test vectors.
import hashlib, hmac, struct
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
from cryptography.hazmat.primitives import serialization as s

def lp(parts): return b"".join(struct.pack(">I", len(p)) + p for p in parts)
def mac(key, parts): return hmac.new(key, lp(parts), hashlib.sha256).digest()
def pub(sk): return sk.public_key().public_bytes(s.Encoding.Raw, s.PublicFormat.Raw)
def dev_id(pk): return hashlib.sha256(pk).digest()[:8].hex()

ska, skb = X25519PrivateKey.from_private_bytes(b"\x01"*32), X25519PrivateKey.from_private_bytes(b"\x02"*32)
pka, pkb = pub(ska), pub(skb)
na, nb = bytes(range(32)), bytes(range(32, 64))
commit = mac(nb, [b"nexus-commit-v2", pkb, pka])
sas = hashlib.sha256(lp([b"nexus-sas-v2", pka, pkb, na, nb])).digest()
code = "%06d" % (struct.unpack(">I", sas[:4])[0] % 1_000_000)
dh = ska.exchange(X25519PublicKey := None) if False else ska.exchange(skb.public_key())
nonce = bytes(range(16))
ping = mac(dh, [b"nexus-ping-v2", nonce, dev_id(pkb).encode(), dev_id(pka).encode()])
for k, v in [("pka", pka.hex()), ("pkb", pkb.hex()), ("id_a", dev_id(pka)), ("id_b", dev_id(pkb)), ("commit", commit.hex()), ("code", code), ("dh", dh.hex()), ("ping", ping.hex())]:
    print(f"{k} = {v}")
stats_mac = mac(dh, [b"nexus-stats-v2", nonce, b'{"cpu":12.5,"ram":40.0,"battery":null}'])
print(f"stats_mac = {stats_mac.hex()}")
cmd_restart = mac(dh, [b"nexus-cmd-v2", nonce, b"restart", dev_id(pkb).encode(), dev_id(pka).encode()])
cmd_ack = mac(dh, [b"nexus-cmdack-v2", nonce, b"ok"])
print(f"cmd_restart = {cmd_restart.hex()}")
print(f"cmd_ack_ok = {cmd_ack.hex()}")
nonce_s = bytes(range(16, 32))
fs_mac_s = mac(dh, [b"nexus-fs-v2", nonce, nonce_s, dev_id(pkb).encode(), dev_id(pka).encode()])
fs_mac_c = mac(dh, [b"nexus-fs-v2", nonce_s, nonce, dev_id(pka).encode(), dev_id(pkb).encode()])
print(f"fs_mac_s = {fs_mac_s.hex()}")
print(f"fs_mac_c = {fs_mac_c.hex()}")
