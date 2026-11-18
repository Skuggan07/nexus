// Nexus wire protocol v2: the same math as desktop/src/net.rs.
// Every function here is checked against the shared test vectors in
// test/protocol_test.dart, so phone and PC always agree.

import 'dart:convert';
import 'dart:math';
import 'dart:typed_data';

import 'package:cryptography/cryptography.dart';

const discoveryPort = 47800;
const defaultTcpPort = 47801;
const protocolVersion = 2;

final labelCommit = utf8.encode('nexus-commit-v2');
final labelSas = utf8.encode('nexus-sas-v2');
final labelPing = utf8.encode('nexus-ping-v2');
final labelStats = utf8.encode('nexus-stats-v2');
final labelCmd = utf8.encode('nexus-cmd-v2');
final labelCmdAck = utf8.encode('nexus-cmdack-v2');
final labelFs = utf8.encode('nexus-fs-v2');
final labelScreen = utf8.encode('nexus-screen-v2');

const wolPort = 9;
const fsChunk = 256 * 1024;

final _x25519 = X25519();
final _sha256 = Sha256();
final _hmac = Hmac.sha256();
final _random = Random.secure();

class NexusError implements Exception {
  NexusError(this.message);
  final String message;
  @override
  String toString() => message;
}

const unexpectedMessage = 'The other device sent something unexpected';

/// Each part is preceded by its length as a 4-byte big-endian number.
Uint8List lengthPrefixed(List<List<int>> parts) {
  final b = BytesBuilder();
  for (final p in parts) {
    final len = ByteData(4)..setUint32(0, p.length);
    b.add(len.buffer.asUint8List());
    b.add(p);
  }
  return b.toBytes();
}

Future<List<int>> sha256(List<int> data) async => (await _sha256.hash(data)).bytes;

Future<List<int>> hmac(List<int> key, List<List<int>> parts) async =>
    (await _hmac.calculateMac(lengthPrefixed(parts), secretKey: SecretKey(key))).bytes;

/// Constant-time comparison.
bool sameBytes(List<int> a, List<int> b) {
  if (a.length != b.length) return false;
  var diff = 0;
  for (var i = 0; i < a.length; i++) {
    diff |= a[i] ^ b[i];
  }
  return diff == 0;
}

String toHex(List<int> bytes) => bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();

List<int> fromHex(String s, [int? length]) {
  if (s.length.isOdd || (length != null && s.length != length * 2)) {
    throw NexusError(unexpectedMessage);
  }
  try {
    return [for (var i = 0; i < s.length; i += 2) int.parse(s.substring(i, i + 2), radix: 16)];
  } on FormatException {
    throw NexusError(unexpectedMessage);
  }
}

List<int> randomBytes(int n) => List<int>.generate(n, (_) => _random.nextInt(256));

/// Device ID: first 8 bytes of SHA-256(public key), as hex.
Future<String> idFor(List<int> publicKey) async => toHex((await sha256(publicKey)).sublist(0, 8));

/// Short, human-comparable fingerprint such as `3F9A 0C21 77DE 51B0`.
Future<String> fingerprint(List<int> publicKey) async {
  final h = toHex((await sha256(publicKey)).sublist(0, 8)).toUpperCase();
  return [for (var i = 0; i < 16; i += 4) h.substring(i, i + 4)].join(' ');
}

Future<List<int>> commitment(List<int> nb, List<int> pkb, List<int> pka) => hmac(nb, [labelCommit, pkb, pka]);

/// The 6-digit number both screens show. A is the device that tapped Pair.
Future<String> pairingCode(List<int> pka, List<int> pkb, List<int> na, List<int> nb) async {
  final h = await sha256(lengthPrefixed([labelSas, pka, pkb, na, nb]));
  final v = ByteData.sublistView(Uint8List.fromList(h.sublist(0, 4))).getUint32(0);
  return (v % 1000000).toString().padLeft(6, '0');
}

Future<List<int>> pingMac(List<int> sharedSecret, List<int> nonce, String responderId, String requesterId) =>
    hmac(sharedSecret, [labelPing, nonce, utf8.encode(responderId), utf8.encode(requesterId)]);

Future<List<int>> statsMac(List<int> sharedSecret, List<int> nonce, String statsJson) =>
    hmac(sharedSecret, [labelStats, nonce, utf8.encode(statsJson)]);

Future<List<int>> commandMac(
        List<int> ss, List<int> nonce, String action, String responderId, String requesterId) =>
    hmac(ss, [labelCmd, nonce, utf8.encode(action), utf8.encode(responderId), utf8.encode(requesterId)]);

Future<List<int>> commandAckMac(List<int> ss, List<int> nonce, String message) =>
    hmac(ss, [labelCmdAck, nonce, utf8.encode(message)]);

/// File-transfer handshake MAC: HMAC(ss, labelFs | n1 | n2 | idA | idB).
Future<List<int>> fsMac(List<int> ss, List<int> n1, List<int> n2, String a, String b) =>
    hmac(ss, [labelFs, n1, n2, utf8.encode(a), utf8.encode(b)]);

/// Screen-sharing handshake MAC: HMAC(ss, labelScreen | n1 | n2 | idA | idB).
Future<List<int>> screenMac(List<int> ss, List<int> n1, List<int> n2, String a, String b) =>
    hmac(ss, [labelScreen, n1, n2, utf8.encode(a), utf8.encode(b)]);

/// Parses "aa:bb:cc:dd:ee:ff" (any separators) into 6 bytes, or null if it is all zeros / malformed.
List<int>? parseMac(String s) {
  final hex = s.split('').where((c) => RegExp(r'[0-9a-fA-F]').hasMatch(c)).join();
  if (hex.length != 12) return null;
  try {
    final b = fromHex(hex, 6);
    return b.any((x) => x != 0) ? b : null;
  } catch (_) {
    return null;
  }
}

Future<SimpleKeyPair> keyPairFromSeed(List<int> seed) => _x25519.newKeyPairFromSeed(seed);

Future<List<int>> publicKeyOf(SimpleKeyPair kp) async => (await kp.extractPublicKey()).bytes;

Future<List<int>> sharedSecret(SimpleKeyPair kp, List<int> remotePublicKey) async {
  final k = await _x25519.sharedSecretKey(
    keyPair: kp,
    remotePublicKey: SimplePublicKey(remotePublicKey, type: KeyPairType.x25519),
  );
  return k.extractBytes();
}
