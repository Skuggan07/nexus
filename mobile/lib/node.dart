// The phone side of Nexus: identity, discovery, pairing and status checks.
// Behaves exactly like the desktop app (desktop/src/*.rs), see PROTOCOL.md.

import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:battery_plus/battery_plus.dart';
import 'package:cryptography/cryptography.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';

import 'protocol.dart';

const onlineWindow = Duration(seconds: 7);
const checkValid = Duration(seconds: 15);
const pairWindow = Duration(seconds: 120);
const pairAttempts = 3;
const decideTimeout = Duration(seconds: 120);

String get osLabel => Platform.isAndroid ? 'Android' : Platform.operatingSystem;

class PairedPeer {
  PairedPeer({
    required this.id,
    required this.name,
    required this.os,
    required this.publicKey,
    required this.pairedAt,
    this.lastAddr,
    this.macs = const [],
  });

  final String id;
  final String name;
  final String os;
  final String publicKey;
  final int pairedAt;
  String? lastAddr; // "ip:port"
  List<String> macs; // learned from announcements, for Wake-on-LAN

  Map<String, Object?> toJson() => {
        'id': id,
        'name': name,
        'os': os,
        'public_key': publicKey,
        'paired_at': pairedAt,
        'last_addr': lastAddr,
        'macs': macs,
      };

  static PairedPeer fromJson(Map<String, dynamic> j) => PairedPeer(
        id: j['id'] as String,
        name: j['name'] as String,
        os: (j['os'] as String?) ?? '',
        publicKey: j['public_key'] as String,
        pairedAt: (j['paired_at'] as num?)?.toInt() ?? 0,
        lastAddr: j['last_addr'] as String?,
        macs: (j['macs'] as List<dynamic>?)?.map((e) => '$e').toList() ?? const [],
      );

  bool get controllable => os != 'Android' && os != 'iOS';
}

class Seen {
  Seen(this.id, this.name, this.os, this.publicKey, this.addr, this.tcpPort, this.lastSeen);
  final String id, name, os, publicKey;
  final InternetAddress addr;
  final int tcpPort;
  final DateTime lastSeen;
  bool get fresh => DateTime.now().difference(lastSeen) < onlineWindow;
}

/// A device's CPU, RAM and battery in percent; null when that device can't tell.
class Stats {
  const Stats({this.cpu, this.ram, this.battery});
  final double? cpu, ram, battery;

  static double? _num(Object? v) => v is num ? v.toDouble() : null;

  static Stats? tryParse(String json) {
    try {
      final m = jsonDecode(json);
      if (m is! Map<String, dynamic>) return null;
      return Stats(cpu: _num(m['cpu']), ram: _num(m['ram']), battery: _num(m['battery']));
    } catch (_) {
      return null;
    }
  }
}

class Check {
  Check.ok(this.rttMs, [this.stats])
      : reason = null,
        at = DateTime.now();
  Check.failed(this.reason)
      : rttMs = null,
        stats = null,
        at = DateTime.now();
  final int? rttMs;
  final Stats? stats;
  final String? reason;
  final DateTime at;
  bool get ok => rttMs != null;
  bool get valid => DateTime.now().difference(at) < checkValid;
}

/// One entry in a remote directory listing.
class FsEntry {
  FsEntry(this.name, this.dir, this.size);
  final String name;
  final bool dir;
  final int size;

  static FsEntry? fromJson(Object? j) {
    if (j is! Map) return null;
    final n = j['name'];
    if (n is! String) return null;
    return FsEntry(n, j['dir'] == true, (j['size'] as num?)?.toInt() ?? 0);
  }
}

/// An open remote-control link. Positions are fractions (0..1) of the shared screen.
class ScreenControl {
  ScreenControl._(this._c) {
    _ping = Timer.periodic(const Duration(seconds: 4), (_) => send({'op': 'ping'}));
  }

  final Conn _c;
  late final Timer _ping;
  bool _closed = false;

  /// Which monitor of the other PC positions are on (0 = its main monitor).
  int monitor = 0;

  bool get isOpen => !_closed;

  void send(Map<String, Object?> m) {
    if (_closed) return;
    try {
      _c.send({...m, 'm': monitor});
    } catch (_) {
      close();
    }
  }

  void move(double x, double y) => send({'op': 'move', 'x': x, 'y': y});
  void button(String b, bool down, [double? x, double? y]) =>
      send({'op': 'button', 'b': b, 'down': down, if (x != null) 'x': x, if (y != null) 'y': y});
  void click(String b, double x, double y) => send({'op': 'click', 'b': b, 'x': x, 'y': y});
  void scroll(int dy) => send({'op': 'scroll', 'dy': dy});
  void text(String s) => send({'op': 'text', 's': s});
  void key(String k, [List<String> mods = const []]) => send({'op': 'key', 'k': k, 'mods': mods});

  void close() {
    if (_closed) return;
    try {
      _c.send({'op': 'bye'});
    } catch (_) {}
    _closed = true;
    _ping.cancel();
    _c.close();
  }
}

/// An open "apps and monitoring" link. Requests are answered one at a time.
class MonitorLink {
  MonitorLink._(this._c);

  final Conn _c;
  Future<void> _queue = Future.value();
  bool _closed = false;

  Future<Map<String, dynamic>> _ask(Map<String, Object?> m) {
    final r = _queue.then((_) async {
      if (_closed) throw NexusError('The monitor was closed');
      _c.send(m);
      final v = await _c.recv(const Duration(seconds: 15));
      if (v['error'] != null) throw NexusError('${v['error']}');
      return v;
    });
    _queue = r.then((_) {}, onError: (_) {});
    return r;
  }

  Future<Map<String, dynamic>> snapshot() => _ask({'op': 'snapshot'});
  Future<Map<String, dynamic>> endApp(String name) => _ask({'op': 'end', 'name': name});

  void close() {
    if (_closed) return;
    _closed = true;
    try {
      _c.send({'op': 'bye'});
    } catch (_) {}
    _c.close();
  }
}

/// An open link to a computer's automations. Requests are answered one at a time.
class AutoLink {
  AutoLink._(this._c);

  final Conn _c;
  Future<void> _queue = Future.value();
  bool _closed = false;

  Future<Map<String, dynamic>> _ask(Map<String, Object?> m) {
    final r = _queue.then((_) async {
      if (_closed) throw NexusError('Automation was closed');
      _c.send(m);
      final v = await _c.recv(const Duration(seconds: 15));
      if (v['error'] != null) throw NexusError('${v['error']}');
      return v;
    });
    _queue = r.then((_) {}, onError: (_) {});
    return r;
  }

  /// The computer's rules, its paired devices and whether they may be changed.
  Future<Map<String, dynamic>> list() async {
    final v = await _ask({'op': 'list'});
    if (v['op'] != 'rules') {
      // An older Nexus on the PC doesn't know about automations.
      _closed = true;
      _c.close();
      throw NexusError('Update Nexus on that PC (build 18 or newer) to use Automation');
    }
    return v;
  }

  Future<Map<String, dynamic>> save(List<Map<String, dynamic>> rules) => _ask({'op': 'save', 'rules': rules});
  Future<Map<String, dynamic>> run(int id) => _ask({'op': 'run', 'id': id});

  void close() {
    if (_closed) return;
    _closed = true;
    try {
      _c.send({'op': 'bye'});
    } catch (_) {}
    _c.close();
  }
}

/// One decoded screen frame: its source size and the JPEG bytes.
class ScreenFrame {
  ScreenFrame(this.w, this.h, this.jpeg, {this.mon = 0, this.mons = 1});
  final int w;
  final int h;
  final Uint8List jpeg;

  /// Which monitor this is (0 = main) and how many the other device has.
  final int mon;
  final int mons;
}

enum StatusKind { verified, checking, notAnswering, identityChanged, offline }

class PeerStatus {
  const PeerStatus(this.kind, {this.rttMs, this.reason, this.stats});
  final StatusKind kind;
  final int? rttMs;
  final Stats? stats;
  final String? reason;
}

enum Role { outgoing, incoming }

enum Phase { connecting, compare, waiting, done, failed }

class PairSession {
  PairSession(this.id, this.role, this.peerName);
  final int id;
  final Role role;
  String peerName;
  Phase phase = Phase.connecting;
  String? code;
  String? error;
  final _decision = Completer<bool>();

  /// Called by the screen: true = numbers match, false = cancel.
  void answer(bool ok) {
    if (!_decision.isCompleted) _decision.complete(ok);
  }

  bool get answered => _decision.isCompleted;
}

class PairTarget {
  PairTarget({required this.host, required this.port, required this.name, this.expectedId, this.expectedKey});
  final String host;
  final int port;
  final String name;
  final String? expectedId;
  final String? expectedKey;
}

class LogLine {
  LogLine(this.text) : at = DateTime.now();
  final DateTime at;
  final String text;
}

/// Newline-delimited JSON over TCP.
class Conn {
  Conn(this.socket) : _lines = StreamIterator(utf8.decoder.bind(socket).transform(const LineSplitter())) {
    socket.done.then((_) {}, onError: (_) {}); // a reset connection is not a crash
  }

  final Socket socket;
  final StreamIterator<String> _lines;

  void send(Map<String, Object?> m) {
    try {
      socket.write('${jsonEncode(m)}\n');
    } catch (_) {
      throw NexusError('Connection lost');
    }
  }

  /// Reads one message. An `error` message from the other side is thrown as a NexusError.
  Future<Map<String, dynamic>> recv([Duration timeout = const Duration(seconds: 15)]) async {
    bool has;
    try {
      has = await _lines.moveNext().timeout(timeout);
    } on TimeoutException {
      throw NexusError('No answer from the other device');
    } catch (_) {
      throw NexusError('Connection lost');
    }
    if (!has) throw NexusError('The other device closed the connection');
    Object? m;
    try {
      m = jsonDecode(_lines.current);
    } catch (_) {
      throw NexusError('Garbled message');
    }
    if (m is! Map<String, dynamic>) throw NexusError('Garbled message');
    if (m['t'] == 'error') throw NexusError('${m['reason']}');
    return m;
  }

  void close() {
    _lines.cancel();
    socket.destroy();
  }
}

String _str(Map<String, dynamic> m, String key) {
  final v = m[key];
  if (v is String) return v;
  throw NexusError(unexpectedMessage);
}

class NexusNode extends ChangeNotifier {
  late Directory _dir;
  late SimpleKeyPair _keyPair;
  late List<int> publicKey;
  late String id;
  late String fingerprintText;
  String name = 'Android phone';
  int tcpPort = 0;
  bool ready = false;
  String? startupError;

  final seen = <String, Seen>{};
  final paired = <PairedPeer>[];
  final checks = <String, Check>{};
  final log = <LogLine>[];
  DateTime? _windowExpires;
  int attemptsLeft = 0;
  PairSession? pair;
  int _nextSession = 1;
  bool _checking = false;
  bool detailed = false;
  String? _statsJson;
  final _battery = Battery();
  final _timers = <Timer>[];
  ServerSocket? _server;
  RawDatagramSocket? _udp;
  Directory? _fsRoot; // the folder this phone shares with paired devices

  /* ------------------------------------------------------------ startup */

  Future<void> start() async {
    try {
      _dir = await getApplicationSupportDirectory();
      await _loadIdentity();
      await _loadPeers();
      try {
        _server = await ServerSocket.bind(InternetAddress.anyIPv4, defaultTcpPort);
      } catch (_) {
        _server = await ServerSocket.bind(InternetAddress.anyIPv4, 0);
      }
      tcpPort = _server!.port;
      _server!.listen(_handle, onError: (_) {});
      try {
        await _startDiscovery();
      } catch (e) {
        startupError = 'Discovery could not start: $e';
      }
      try {
        _fsRoot = (await getExternalStorageDirectory()) ?? _dir;
        await _fsRoot!.create(recursive: true);
      } catch (_) {
        _fsRoot = _dir;
      }
      await _loadPrefs();
      await _sampleStats();
      _timers.add(Timer.periodic(const Duration(seconds: 5), (_) => _sampleStats()));
      _timers.add(Timer.periodic(const Duration(seconds: 5), (_) => _checkAll()));
      _timers.add(Timer.periodic(const Duration(seconds: 1), (_) => notifyListeners()));
      addLog('Nexus started as $name');
    } catch (e) {
      startupError = 'Nexus could not start: $e';
    }
    ready = true;
    notifyListeners();
  }

  @override
  void dispose() {
    for (final t in _timers) {
      t.cancel();
    }
    _server?.close();
    _udp?.close();
    super.dispose();
  }

  Future<void> _loadIdentity() async {
    final f = File('${_dir.path}/identity.json');
    List<int>? seed;
    if (await f.exists()) {
      try {
        final j = jsonDecode(await f.readAsString()) as Map<String, dynamic>;
        seed = fromHex(j['secret'] as String, 32);
        name = (j['name'] as String?) ?? name;
      } catch (_) {
        seed = null;
      }
    }
    if (seed == null) {
      seed = randomBytes(32);
      await _saveIdentity(seed);
    }
    _seed = seed;
    _keyPair = await keyPairFromSeed(seed);
    publicKey = await publicKeyOf(_keyPair);
    id = await idFor(publicKey);
    fingerprintText = await fingerprint(publicKey);
  }

  late List<int> _seed;

  Future<void> _saveIdentity(List<int> seed) async {
    await _dir.create(recursive: true);
    await File('${_dir.path}/identity.json').writeAsString(jsonEncode({'secret': toHex(seed), 'name': name}));
  }

  Future<void> rename(String newName) async {
    final n = newName.trim();
    if (n.isEmpty) return;
    name = n.length > 40 ? n.substring(0, 40) : n;
    await _saveIdentity(_seed);
    addLog('This phone is now called $name');
    notifyListeners();
  }

  Future<void> _loadPeers() async {
    final f = File('${_dir.path}/peers.json');
    if (!await f.exists()) return;
    try {
      final list = jsonDecode(await f.readAsString()) as List<dynamic>;
      paired.addAll(list.map((e) => PairedPeer.fromJson(e as Map<String, dynamic>)));
    } catch (_) {}
  }

  Future<void> _savePeers() async {
    final tmp = File('${_dir.path}/peers.json.tmp');
    await tmp.writeAsString(jsonEncode(paired.map((p) => p.toJson()).toList()));
    await tmp.rename('${_dir.path}/peers.json');
  }

  Future<void> _loadPrefs() async {
    try {
      final f = File('${_dir.path}/prefs.json');
      if (await f.exists()) {
        final m = jsonDecode(await f.readAsString()) as Map<String, dynamic>;
        detailed = m['detailed'] == true;
        shareClipboard = m['share_clipboard'] != false;
      }
    } catch (_) {}
  }

  Future<void> _savePrefs() async {
    try {
      await File('${_dir.path}/prefs.json')
          .writeAsString(jsonEncode({'detailed': detailed, 'share_clipboard': shareClipboard}));
    } catch (_) {}
  }

  Future<void> setDetailed(bool value) async {
    detailed = value;
    notifyListeners();
    await _savePrefs();
  }

  /* ---------------------------------------------- shared clipboard */

  /// Whether copied text is shared with paired devices.
  bool shareClipboard = true;

  /// The last text that arrived from another device, for the home screen.
  String? lastClipText;
  String? lastClipFrom;
  String? _clipLast;
  int? _clipStamp;

  Future<void> setShareClipboard(bool value) async {
    shareClipboard = value;
    notifyListeners();
    await _savePrefs();
  }

  Future<int?> _clipboardStamp() async {
    final v = await _native('clipStamp');
    return v is int ? v : null;
  }

  /// Called when Nexus comes to the front: if something new was copied while it was
  /// in the background, send it. (Android only lets the app in front read the clipboard.)
  Future<void> checkClipboard() async {
    if (!shareClipboard) return;
    final stamp = await _clipboardStamp();
    if (stamp == null) return;
    final first = _clipStamp == null;
    if (stamp == _clipStamp) return;
    _clipStamp = stamp;
    // On the first look, only remember where things are — don't send old text.
    if (first) return;
    await sendClipboard(auto: true);
  }

  /// Sends this phone's clipboard text to the paired devices that are online.
  /// Returns how many took it.
  Future<int> sendClipboard({bool auto = false}) async {
    String text = '';
    try {
      text = (await Clipboard.getData(Clipboard.kTextPlain))?.text ?? '';
    } catch (_) {}
    if (text.trim().isEmpty) {
      if (!auto) {
        addLog('The clipboard is empty');
        notifyListeners();
      }
      return 0;
    }
    if (auto && text == _clipLast) return 0;
    _clipLast = text;
    final names = <String>[];
    for (final p in paired) {
      final k = statusOf(p).kind;
      if (k != StatusKind.verified && k != StatusKind.checking) continue;
      Conn? c;
      try {
        c = await _screenOpen(p);
        c.send({'op': 'clip', 'text': text});
        final v = await c.recv(const Duration(seconds: 8));
        if (v['error'] == null) {
          names.add(p.name);
        } else {
          addLog('${p.name}: ${v['error']}');
        }
      } catch (e) {
        addLog('Could not send the clipboard to ${p.name}: $e');
      } finally {
        c?.close();
      }
    }
    if (names.isNotEmpty) {
      addLog('Copied text sent to ${names.join(', ')}');
    } else if (!auto) {
      addLog('No device is online to send the clipboard to');
    }
    notifyListeners();
    return names.length;
  }

  /// A paired device sent text for this phone's clipboard.
  Future<void> _clipServe(Conn c, Map<String, dynamic> m, String who) async {
    final text = '${m['text'] ?? ''}';
    if (!shareClipboard) {
      c.send({'error': 'Shared clipboard is turned off on $name'});
    } else if (text.isEmpty || text.length > 1000000) {
      c.send({'error': 'Nothing to paste'});
    } else {
      _clipLast = text;
      try {
        await Clipboard.setData(ClipboardData(text: text));
        _clipStamp = await _clipboardStamp();
        lastClipText = text;
        lastClipFrom = who;
        addLog('Clipboard from $who');
        notifyListeners();
        c.send({'op': 'ok'});
      } catch (e) {
        c.send({'error': 'The phone did not accept the text'});
      }
    }
    c.close();
  }

  /// This phone's RAM (from /proc/meminfo) and battery. Android does not let apps read CPU use.
  Future<void> _sampleStats() async {
    double? ram, battery;
    try {
      final lines = await File('/proc/meminfo').readAsLines();
      int? total, avail;
      for (final l in lines) {
        final parts = l.split(RegExp(r'\s+'));
        if (parts.length < 2) continue;
        if (parts[0] == 'MemTotal:') total = int.tryParse(parts[1]);
        if (parts[0] == 'MemAvailable:') avail = int.tryParse(parts[1]);
      }
      if (total != null && avail != null && total > 0) {
        ram = ((total - avail) / total * 1000).round() / 10;
      }
    } catch (_) {}
    try {
      battery = (await _battery.batteryLevel).toDouble();
    } catch (_) {}
    _statsJson = jsonEncode({'cpu': null, 'ram': ram, 'battery': battery});
  }

  void addLog(String text) {
    log.insert(0, LogLine(text));
    if (log.length > 200) log.removeLast();
  }

  /* ----------------------------------------------------------- queries */

  bool isPaired(String peerId) => paired.any((p) => p.id == peerId);

  bool get windowOpen =>
      _windowExpires != null && DateTime.now().isBefore(_windowExpires!) && attemptsLeft > 0;

  Duration get windowLeft =>
      windowOpen ? _windowExpires!.difference(DateTime.now()) : Duration.zero;

  List<Seen> get nearby =>
      seen.values.where((s) => s.fresh && !isPaired(s.id)).toList()..sort((a, b) => a.name.toLowerCase().compareTo(b.name.toLowerCase()));

  PeerStatus statusOf(PairedPeer p) {
    final s = seen[p.id];
    final fresh = s != null && s.fresh;
    if (fresh && s!.publicKey != p.publicKey) return const PeerStatus(StatusKind.identityChanged);
    final c = checks[p.id];
    if (c != null && c.valid && c.ok) return PeerStatus(StatusKind.verified, rttMs: c.rttMs, stats: c.stats);
    if (c != null && c.valid && !c.ok && fresh) return PeerStatus(StatusKind.notAnswering, reason: c.reason);
    return fresh ? const PeerStatus(StatusKind.checking) : const PeerStatus(StatusKind.offline);
  }

  String? addressOf(PairedPeer p) {
    final s = seen[p.id];
    if (s != null && s.fresh) return s.addr.address;
    final a = p.lastAddr;
    if (a == null) return null;
    final i = a.lastIndexOf(':');
    return i > 0 ? a.substring(0, i) : a;
  }

  /* ----------------------------------------------------------- actions */

  void allowPairing() {
    _windowExpires = DateTime.now().add(pairWindow);
    attemptsLeft = pairAttempts;
    addLog('Pairing allowed for 2 minutes');
    notifyListeners();
  }

  void stopPairing() {
    _windowExpires = null;
    addLog('Pairing no longer allowed');
    notifyListeners();
  }

  Future<void> removePeer(String peerId) async {
    final i = paired.indexWhere((p) => p.id == peerId);
    if (i < 0) return;
    final p = paired.removeAt(i);
    checks.remove(peerId);
    await _savePeers();
    addLog('Removed ${p.name}. It can no longer verify with this phone.');
    notifyListeners();
  }

  /// Resolves a peer's current host/port, from a fresh announcement or its last address.
  (String, int)? _addressFor(PairedPeer p) {
    final s = seen[p.id];
    if (s != null && s.fresh) return (s.addr.address, s.tcpPort);
    final a = p.lastAddr;
    if (a == null) return null;
    final i = a.lastIndexOf(':');
    if (i <= 0) return null;
    final port = int.tryParse(a.substring(i + 1));
    return port == null ? null : (a.substring(0, i), port);
  }

  /// Asks a paired computer to lock, sleep, restart or shut down.
  Future<void> sendCommand(PairedPeer p, String action) async {
    final addr = _addressFor(p);
    if (addr == null) {
      addLog('${p.name} is not reachable right now');
      notifyListeners();
      return;
    }
    Conn? c;
    try {
      final sock = await Socket.connect(addr.$1, addr.$2, timeout: const Duration(seconds: 3));
      c = Conn(sock);
      final nonce = randomBytes(16);
      final ss = await sharedSecret(_keyPair, fromHex(p.publicKey, 32));
      c.send({'t': 'command', 'id': id, 'nonce': toHex(nonce), 'action': action, 'mac': toHex(await commandMac(ss, nonce, action, p.id, id))});
      final m = await c.recv(const Duration(seconds: 5));
      if (m['t'] != 'command_ack') throw NexusError(unexpectedMessage);
      final msg = '${m['message'] ?? ''}';
      if (!sameBytes(await commandAckMac(ss, nonce, msg), fromHex(_str(m, 'mac')))) {
        throw NexusError('The reply failed its security check');
      }
      addLog(m['ok'] == true ? 'Asked ${p.name} to $action' : 'Could not $action ${p.name}: $msg');
    } on NexusError catch (e) {
      addLog('Could not $action ${p.name}: ${e.message}');
    } catch (e) {
      addLog('Could not $action ${p.name}: $e');
    } finally {
      c?.close();
      notifyListeners();
    }
  }

  /// Sends a Wake-on-LAN magic packet for each known MAC of a device.
  Future<void> wake(PairedPeer p) async {
    final macs = p.macs.map(parseMac).whereType<List<int>>().toList();
    if (macs.isEmpty) {
      addLog('No network address on file for ${p.name} yet. Connect it once while it is on.');
      notifyListeners();
      return;
    }
    RawDatagramSocket? sock;
    try {
      sock = await RawDatagramSocket.bind(InternetAddress.anyIPv4, 0);
      sock.broadcastEnabled = true;
      final targets = await _broadcastTargets();
      for (final mac in macs) {
        final packet = <int>[...List.filled(6, 0xFF), for (var i = 0; i < 16; i++) ...mac];
        for (final t in targets) {
          try {
            sock.send(packet, t, wolPort);
          } catch (_) {}
        }
      }
      addLog('Sent a wake signal to ${p.name}');
    } catch (e) {
      addLog('Could not wake ${p.name}: $e');
    } finally {
      sock?.close();
      notifyListeners();
    }
  }

  /// Opens an authenticated file-transfer connection to a paired device.
  Future<Conn> _fsOpen(PairedPeer p) async {
    final addr = _addressFor(p);
    if (addr == null) throw NexusError('${p.name} is not reachable right now');
    final sock = await Socket.connect(addr.$1, addr.$2, timeout: const Duration(seconds: 4));
    final c = Conn(sock);
    try {
      final nonceC = randomBytes(16);
      final ss = await sharedSecret(_keyPair, fromHex(p.publicKey, 32));
      c.send({'t': 'fs_hello', 'id': id, 'nonce': toHex(nonceC)});
      final ready = await c.recv();
      if (ready['t'] != 'fs_ready') throw NexusError(unexpectedMessage);
      final nonceS = fromHex(_str(ready, 'nonce'));
      if (!sameBytes(await fsMac(ss, nonceC, nonceS, p.id, id), fromHex(_str(ready, 'mac')))) {
        throw NexusError('The device failed the file-transfer security check');
      }
      c.send({'t': 'fs_auth', 'mac': toHex(await fsMac(ss, nonceS, nonceC, id, p.id))});
      return c;
    } catch (e) {
      c.close();
      rethrow;
    }
  }

  /// Opens an authenticated screen-sharing connection to a paired device (we view it).
  Future<Conn> _screenOpen(PairedPeer p) async {
    final addr = _addressFor(p);
    if (addr == null) throw NexusError('${p.name} is not reachable right now');
    final sock = await Socket.connect(addr.$1, addr.$2, timeout: const Duration(seconds: 4));
    final c = Conn(sock);
    try {
      final nonceC = randomBytes(16);
      final ss = await sharedSecret(_keyPair, fromHex(p.publicKey, 32));
      c.send({'t': 'screen_hello', 'id': id, 'nonce': toHex(nonceC)});
      final ready = await c.recv();
      if (ready['t'] != 'screen_ready') throw NexusError(unexpectedMessage);
      final nonceS = fromHex(_str(ready, 'nonce'));
      if (!sameBytes(await screenMac(ss, nonceC, nonceS, p.id, id), fromHex(_str(ready, 'mac')))) {
        throw NexusError('The device failed the screen-sharing security check');
      }
      c.send({'t': 'screen_auth', 'mac': toHex(await screenMac(ss, nonceS, nonceC, id, p.id))});
      return c;
    } catch (e) {
      c.close();
      rethrow;
    }
  }

  /// Streams a paired device's screen as JPEG frames. Closing the subscription
  /// tears the connection down, which stops the other device capturing.
  Stream<ScreenFrame> screenStream(PairedPeer p,
      {int fps = 12, int maxW = 1920, int quality = 75, int monitor = 0}) async* {
    final c = await _screenOpen(p);
    try {
      c.send({'op': 'start', 'fps': fps, 'maxw': maxW, 'quality': quality, 'monitor': monitor});
      while (true) {
        final v = await c.recv(const Duration(seconds: 20));
        if (v['error'] != null) throw NexusError('${v['error']}');
        final data = v['data'];
        if (data is! String) continue;
        yield ScreenFrame(
          (v['w'] as num?)?.toInt() ?? 0,
          (v['h'] as num?)?.toInt() ?? 0,
          base64Decode(data),
          mon: (v['mon'] as num?)?.toInt() ?? 0,
          mons: ((v['mons'] as num?)?.toInt() ?? 1).clamp(1, 16),
        );
      }
    } finally {
      try {
        c.send({'op': 'bye'});
      } catch (_) {}
      c.close();
    }
  }

  /// Streams a paired PC's sound: (sample rate, 16-bit stereo PCM bytes) chunks.
  /// Cancelling the subscription closes the link, which stops the PC recording.
  Stream<(int, Uint8List)> soundStream(PairedPeer p) async* {
    final c = await _screenOpen(p);
    try {
      c.send({'op': 'audio'});
      final head = await c.recv(const Duration(seconds: 8));
      if (head['error'] != null) throw NexusError('${head['error']}');
      if (head['op'] != 'audio') throw NexusError('Update Nexus on that PC to hear its sound');
      final rate = (head['rate'] as num?)?.toInt() ?? 48000;
      while (true) {
        final v = await c.recv(const Duration(seconds: 10));
        if (v['error'] != null) throw NexusError('${v['error']}');
        final d = v['pcm'];
        if (d is String) yield (rate, base64Decode(d));
      }
    } finally {
      try {
        c.send({'op': 'bye'});
      } catch (_) {}
      c.close();
    }
  }

  /// Plays PC sound on this phone's speaker (see [soundStream]).
  Future<void> soundPlay(int rate) async => _native('audioStart', {'rate': rate});
  void soundWrite(Uint8List pcm) => _native('audioWrite', pcm);
  Future<void> soundStop() async => _native('audioStop');

  /// Opens a remote-control link to a paired device: mouse and keyboard events
  /// sent on it are carried out there. Fails if that device has control turned off.
  Future<ScreenControl> screenControl(PairedPeer p) async {
    final c = await _screenOpen(p);
    try {
      c.send({'op': 'control'});
      final r = await c.recv(const Duration(seconds: 8));
      if (r['error'] != null) throw NexusError('${r['error']}');
      return ScreenControl._(c);
    } catch (e) {
      c.close();
      rethrow;
    }
  }

  /// Opens an "apps and monitoring" link to a paired device.
  Future<MonitorLink> monitorOpen(PairedPeer p) async {
    final c = await _screenOpen(p);
    c.send({'op': 'monitor'});
    return MonitorLink._(c);
  }

  /// Opens a link to a paired computer's automations.
  Future<AutoLink> autoOpen(PairedPeer p) async {
    final c = await _screenOpen(p);
    c.send({'op': 'auto'});
    return AutoLink._(c);
  }

  /// Lists a folder on a paired device.
  Future<List<FsEntry>> fsList(PairedPeer p, String path) async {
    final c = await _fsOpen(p);
    try {
      c.send({'op': 'list', 'path': path});
      final v = await c.recv();
      if (v['error'] != null) throw NexusError('${v['error']}');
      final list = (v['entries'] as List?) ?? const [];
      final out = <FsEntry>[];
      for (final e in list) {
        final fe = FsEntry.fromJson(e);
        if (fe != null) out.add(fe);
      }
      c.send({'op': 'bye'});
      return out;
    } finally {
      c.close();
    }
  }

  /// Downloads a file from a paired device into this phone's "Nexus Downloads" folder.
  Future<String> fsGet(PairedPeer p, String path, String fileName) async {
    final c = await _fsOpen(p);
    try {
      c.send({'op': 'get', 'path': path});
      final head = await c.recv();
      if (head['error'] != null) throw NexusError('${head['error']}');
      final dir = Directory('${_fsRoot!.path}/Nexus Downloads');
      await dir.create(recursive: true);
      final dest = File('${dir.path}/$fileName');
      final sink = dest.openWrite();
      try {
        while (true) {
          final frame = await c.recv();
          if (frame['error'] != null) throw NexusError('${frame['error']}');
          if (frame['eof'] == true) break;
          final d = frame['data'];
          if (d is String) sink.add(fromHex(d));
        }
      } finally {
        await sink.close();
      }
      c.send({'op': 'bye'});
      return dest.path;
    } finally {
      c.close();
    }
  }

  /// Uploads a local file to a paired device's "Nexus Inbox" folder.
  Future<String> fsPut(PairedPeer p, String localPath) async {
    final c = await _fsOpen(p);
    try {
      final f = File(localPath);
      final name = localPath.split('/').last.split('\\').last;
      c.send({'op': 'put', 'name': name});
      final ack = await c.recv();
      if (ack['error'] != null) throw NexusError('${ack['error']}');
      final raf = await f.open();
      try {
        while (true) {
          final chunk = await raf.read(fsChunk);
          if (chunk.isEmpty) break;
          c.send({'data': toHex(chunk)});
        }
      } finally {
        await raf.close();
      }
      c.send({'end': true});
      final done = await c.recv();
      if (done['error'] != null) throw NexusError('${done['error']}');
      return '${done['saved'] ?? name}';
    } finally {
      c.close();
    }
  }

  /// Closes the pairing dialog; a pairing still in progress is cancelled.
  void closePair() {
    pair?.answer(false);
    pair = null;
    notifyListeners();
  }

  PairSession? _begin(Role role, String peerName) {
    final p = pair;
    if (p != null && p.phase != Phase.done && p.phase != Phase.failed) return null;
    final s = PairSession(_nextSession++, role, peerName);
    pair = s;
    notifyListeners();
    return s;
  }

  void _finish(PairSession s, {String? error}) {
    if (error != null) {
      addLog('Pairing with ${s.peerName} failed: $error');
      s.error = error;
      s.phase = Phase.failed;
    } else {
      s.phase = Phase.done;
    }
    notifyListeners();
  }

  Future<void> _savePeer(PairedPeer p) async {
    paired.removeWhere((x) => x.id == p.id);
    paired.add(p);
    try {
      await _savePeers();
    } catch (e) {
      addLog('Could not save the paired list: $e');
    }
    addLog('Paired with ${p.name}');
  }

  /* --------------------------------------------------------- discovery */

  Future<void> _startDiscovery() async {
    final sock = await RawDatagramSocket.bind(InternetAddress.anyIPv4, discoveryPort, reuseAddress: true);
    sock.broadcastEnabled = true;
    _udp = sock;
    sock.listen((ev) {
      if (ev != RawSocketEvent.read) return;
      Datagram? d;
      while ((d = sock.receive()) != null) {
        _onAnnounce(d!);
      }
    }, onError: (_) {});
    Future<void> announce() async {
      final payload = utf8.encode(jsonEncode({
        'app': 'nexus',
        'v': 1,
        'id': id,
        'name': name,
        'os': osLabel,
        'public_key': toHex(publicKey),
        'tcp_port': tcpPort,
        'macs': const <String>[], // Android does not expose the phone's own MAC
      }));
      for (final t in await _broadcastTargets()) {
        try {
          sock.send(payload, t, discoveryPort);
        } catch (_) {}
      }
    }

    await announce();
    _timers.add(Timer.periodic(const Duration(seconds: 2), (_) => announce()));
  }

  /// 255.255.255.255 plus x.y.z.255 for every IPv4 address (Dart cannot read netmasks; /24 is typical).
  Future<List<InternetAddress>> _broadcastTargets() async {
    final out = <String, InternetAddress>{'255.255.255.255': InternetAddress('255.255.255.255')};
    try {
      for (final i in await NetworkInterface.list(type: InternetAddressType.IPv4)) {
        for (final a in i.addresses) {
          if (a.isLoopback) continue;
          final r = a.rawAddress;
          final b = InternetAddress.fromRawAddress(Uint8List.fromList([r[0], r[1], r[2], 255]));
          out[b.address] = b;
        }
      }
    } catch (_) {}
    return out.values.toList();
  }

  Future<void> _onAnnounce(Datagram d) async {
    Map<String, dynamic> a;
    try {
      a = jsonDecode(utf8.decode(d.data)) as Map<String, dynamic>;
    } catch (_) {
      return;
    }
    if (a['app'] != 'nexus' || a['id'] == id) return;
    final peerId = a['id'], key = a['public_key'], peerName = a['name'], port = a['tcp_port'];
    if (peerId is! String || key is! String || peerName is! String || port is! int) return;
    try {
      if (await idFor(fromHex(key, 32)) != peerId) return; // ID must belong to the key
    } catch (_) {
      return;
    }
    final prev = seen[peerId];
    if (prev == null || !prev.fresh) {
      addLog('$peerName ${isPaired(peerId) ? 'is back on the network' : 'found nearby'} (${d.address.address})');
    }
    // Remember a paired device's MAC addresses so it can be woken later.
    final rawMacs = a['macs'];
    if (rawMacs is List && rawMacs.isNotEmpty) {
      final macs = rawMacs.map((e) => '$e').toList();
      for (final p in paired) {
        if (p.id == peerId && !listEquals(p.macs, macs)) {
          p.macs = macs;
          await _savePeers();
        }
      }
    }
    seen[peerId] = Seen(peerId, peerName, '${a['os'] ?? ''}', key, d.address, port, DateTime.now());
  }

  /* ------------------------------------------------------------ server */

  Future<void> _handle(Socket s) async {
    final c = Conn(s);
    try {
      final m = await c.recv();
      switch (m['t']) {
        case 'ping':
          await _answerPing(c, m);
        case 'command':
          await _answerCommand(c, m);
        case 'fs_hello':
          await _fsServe(c, m);
        case 'screen_hello':
          await _screenServe(c, m);
        case 'pair_hello':
          await _hostPair(c, m);
      }
    } catch (_) {
    } finally {
      c.close();
    }
  }

  Future<void> _answerPing(Conn c, Map<String, dynamic> m) async {
    final requester = _str(m, 'id');
    final nonce = fromHex(_str(m, 'nonce'));
    PairedPeer? peer;
    for (final p in paired) {
      if (p.id == requester) peer = p;
    }
    if (peer == null) {
      c.send({'t': 'error', 'reason': '$name no longer trusts this device. Pair again.'});
      return;
    }
    final ss = await sharedSecret(_keyPair, fromHex(peer.publicKey, 32));
    final stats = _statsJson;
    c.send({
      't': 'pong',
      'mac': toHex(await pingMac(ss, nonce, id, requester)),
      if (stats != null) 'stats': stats,
      if (stats != null) 'stats_mac': toHex(await statsMac(ss, nonce, stats)),
    });
  }

  /// Android apps can't lock, restart or shut down the phone, so Nexus declines these — but still
  /// verifies the request came from a paired device before answering.
  Future<void> _answerCommand(Conn c, Map<String, dynamic> m) async {
    final requester = _str(m, 'id');
    PairedPeer? peer;
    for (final p in paired) {
      if (p.id == requester) peer = p;
    }
    if (peer == null) {
      c.send({'t': 'error', 'reason': '$name no longer trusts this device. Pair again.'});
      return;
    }
    final nonce = fromHex(_str(m, 'nonce'));
    final action = _str(m, 'action');
    final ss = await sharedSecret(_keyPair, fromHex(peer.publicKey, 32));
    if (!sameBytes(await commandMac(ss, nonce, action, id, requester), fromHex(_str(m, 'mac')))) {
      c.send({'t': 'error', 'reason': 'Command failed its security check'});
      return;
    }
    const message = 'A phone cannot run that remotely';
    c.send({'t': 'command_ack', 'ok': false, 'message': message, 'mac': toHex(await commandAckMac(ss, nonce, message))});
  }

  /// Joins a client-supplied relative path under the shared root, refusing anything that escapes it.
  String? _fsResolve(String rel) {
    final root = _fsRoot;
    if (root == null) return null;
    final parts = rel.split('/').where((p) => p.isNotEmpty && p != '.').toList();
    if (parts.contains('..')) return null;
    return parts.isEmpty ? root.path : '${root.path}/${parts.join('/')}';
  }

  /// Serves file requests to a paired device over one authenticated connection.
  /* ---------------------------------------------- sharing this phone */

  static const _screenChannel = MethodChannel('nexus/screen');

  Future<Object?> _native(String method, [Object? args]) async {
    try {
      return await _screenChannel.invokeMethod<Object?>(method, args);
    } catch (_) {
      return null; // not on Android, or the native side is unavailable
    }
  }

  /// Asks Android for permission to share this phone's screen (shows the system prompt).
  Future<bool> startScreenShare() async => await _native('startCapture', {'maxw': 1920}) == true;
  Future<void> stopScreenShare() async => _native('stopCapture');
  Future<bool> isScreenShared() async => await _native('isCapturing') == true;

  /// Whether the Nexus Accessibility service is on, so paired devices can tap and type here.
  Future<bool> controlEnabled() async => await _native('accessibilityEnabled') == true;
  Future<void> openControlSettings() async => _native('openAccessibilitySettings');

  /// A paired device wants to view or control this phone's screen.
  Future<void> _screenServe(Conn c, Map<String, dynamic> m) async {
    final requester = _str(m, 'id');
    PairedPeer? peer;
    for (final p in paired) {
      if (p.id == requester) peer = p;
    }
    if (peer == null) {
      c.send({'t': 'error', 'reason': '$name no longer trusts this device. Pair again.'});
      return;
    }
    final nonceC = fromHex(_str(m, 'nonce'));
    final ss = await sharedSecret(_keyPair, fromHex(peer.publicKey, 32));
    final nonceS = randomBytes(16);
    c.send({
      't': 'screen_ready',
      'nonce': toHex(nonceS),
      'mac': toHex(await screenMac(ss, nonceC, nonceS, id, requester)),
    });
    final auth = await c.recv();
    if (auth['t'] != 'screen_auth' ||
        !sameBytes(await screenMac(ss, nonceS, nonceC, requester, id), fromHex(_str(auth, 'mac')))) {
      c.send({'t': 'error', 'reason': 'Screen-sharing security check failed'});
      return;
    }
    final first = await c.recv();
    if (first['op'] == 'control') {
      await _controlServe(c, peer.name);
    } else if (first['op'] == 'monitor') {
      await _monitorServe(c, peer.name);
    } else if (first['op'] == 'clip') {
      await _clipServe(c, first, peer.name);
    } else if (first['op'] == 'auto') {
      c.send({'error': 'Automations run on computers, not on phones'});
      c.close();
    } else {
      await _streamServe(c, first, peer.name);
    }
  }

  Future<void> _streamServe(Conn c, Map<String, dynamic> start, String who) async {
    if (!await isScreenShared()) {
      c.send({
        'error': 'Screen sharing is off on $name. On the phone, open Nexus and turn on "Share this phone\'s screen".'
      });
      return;
    }
    final fps = ((start['fps'] as num?)?.toInt() ?? 8).clamp(1, 15);
    var closed = false;
    // The viewer sends nothing more; any line or a closed socket ends the stream.
    unawaited(c.recv(const Duration(days: 1)).then((_) {}, onError: (_) {}).whenComplete(() => closed = true));
    addLog('$who is viewing this phone\'s screen');
    notifyListeners();
    var lastSeq = -1;
    var lastSent = DateTime.fromMillisecondsSinceEpoch(0);
    while (!closed) {
      final f = await _native('frame');
      if (f is Map) {
        final seq = (f['seq'] as num?)?.toInt() ?? 0;
        final now = DateTime.now();
        // Re-send the same picture every 2 s while nothing changes so the viewer stays connected.
        if (seq != lastSeq || now.difference(lastSent).inMilliseconds >= 2000) {
          final jpeg = f['jpeg'];
          if (jpeg is Uint8List) {
            c.send({'op': 'frame', 'w': f['w'], 'h': f['h'], 'seq': seq, 'data': base64Encode(jpeg)});
            lastSeq = seq;
            lastSent = now;
          }
        }
      } else if (!await isScreenShared()) {
        c.send({'error': 'Screen sharing was turned off on $name'});
        break;
      }
      await Future<void>.delayed(Duration(milliseconds: 1000 ~/ fps));
    }
    addLog('$who stopped viewing this phone\'s screen');
    notifyListeners();
  }

  /// A paired device opened "apps and monitoring" for this phone. Android does not
  /// let apps see which other apps are running, so the battery is all we can share.
  Future<void> _monitorServe(Conn c, String who) async {
    addLog('$who opened the monitor for this phone');
    notifyListeners();
    while (true) {
      final v = await c
          .recv(const Duration(seconds: 30))
          .then<Map<String, dynamic>?>((x) => x, onError: (Object _) => null);
      if (v == null || v['op'] == 'bye') break;
      if (v['op'] == 'snapshot') {
        double? battery;
        try {
          battery = (await _battery.batteryLevel).toDouble();
        } catch (_) {}
        c.send({
          'op': 'snapshot',
          'battery': battery,
          'apps': <Object>[],
          'can_end': false,
          'note': "Android doesn't let apps see which other apps are running, so only the battery is shown for a phone.",
        });
      } else if (v['op'] == 'end') {
        c.send({'error': "Apps on a phone can't be ended from Nexus"});
      }
    }
  }

  Future<void> _controlServe(Conn c, String who) async {
    if (!await controlEnabled()) {
      c.send({
        'error': 'Remote control is off on $name. On the phone, open Nexus and turn on "Let paired devices control this phone".'
      });
      return;
    }
    c.send({'op': 'ready'});
    addLog('$who is controlling this phone');
    notifyListeners();
    while (true) {
      final v = await c
          .recv(const Duration(seconds: 20))
          .then<Map<String, dynamic>?>((x) => x, onError: (Object _) => null);
      if (v == null) break;
      final op = v['op'];
      if (op == 'bye') break;
      if (op == null || op == 'ping') continue;
      await _native('inject', v);
    }
    addLog('$who stopped controlling this phone');
    notifyListeners();
  }

  Future<void> _fsServe(Conn c, Map<String, dynamic> m) async {
    final requester = _str(m, 'id');
    PairedPeer? peer;
    for (final p in paired) {
      if (p.id == requester) peer = p;
    }
    if (peer == null) {
      c.send({'t': 'error', 'reason': '$name no longer trusts this device. Pair again.'});
      return;
    }
    final nonceC = fromHex(_str(m, 'nonce'));
    final ss = await sharedSecret(_keyPair, fromHex(peer.publicKey, 32));
    final nonceS = randomBytes(16);
    c.send({'t': 'fs_ready', 'nonce': toHex(nonceS), 'mac': toHex(await fsMac(ss, nonceC, nonceS, id, requester))});
    final auth = await c.recv();
    if (auth['t'] != 'fs_auth' ||
        !sameBytes(await fsMac(ss, nonceS, nonceC, requester, id), fromHex(_str(auth, 'mac')))) {
      c.send({'t': 'error', 'reason': 'File-transfer security check failed'});
      return;
    }
    addLog('${peer.name} opened a file connection');
    notifyListeners();
    while (true) {
      final v = await c.recv();
      final op = v['t'] == null ? v['op'] : null;
      try {
        if (op == 'list') {
          await _fsList(c, '${v['path'] ?? ''}');
        } else if (op == 'get') {
          await _fsGet(c, '${v['path'] ?? ''}');
        } else if (op == 'put') {
          await _fsPut(c, '${v['name'] ?? ''}');
        } else if (op == 'bye') {
          return;
        } else {
          c.send({'error': 'Unknown request'});
        }
      } on NexusError catch (e) {
        c.send({'error': e.message});
      } catch (e) {
        c.send({'error': '$e'});
      }
    }
  }

  Future<void> _fsList(Conn c, String rel) async {
    final path = _fsResolve(rel);
    if (path == null) throw NexusError('That path is not allowed');
    final dir = Directory(path);
    final entries = <Map<String, Object?>>[];
    await for (final e in dir.list(followLinks: false)) {
      final isDir = e is Directory;
      int size = 0;
      try {
        if (!isDir) size = await File(e.path).length();
      } catch (_) {}
      entries.add({'name': e.path.split('/').last, 'dir': isDir, 'size': size});
    }
    entries.sort((a, b) {
      final ad = a['dir'] == true, bd = b['dir'] == true;
      if (ad != bd) return ad ? -1 : 1;
      return '${a['name']}'.toLowerCase().compareTo('${b['name']}'.toLowerCase());
    });
    c.send({'entries': entries});
  }

  Future<void> _fsGet(Conn c, String rel) async {
    final path = _fsResolve(rel);
    if (path == null) throw NexusError('That path is not allowed');
    final f = File(path);
    final size = await f.length();
    c.send({'size': size});
    final raf = await f.open();
    try {
      while (true) {
        final chunk = await raf.read(fsChunk);
        if (chunk.isEmpty) break;
        c.send({'data': toHex(chunk)});
      }
    } finally {
      await raf.close();
    }
    c.send({'eof': true});
  }

  Future<void> _fsPut(Conn c, String name) async {
    final base = name.split('/').last.split('\\').last;
    if (base.isEmpty) throw NexusError('Bad file name');
    final inbox = Directory('${_fsRoot!.path}/Nexus Inbox');
    await inbox.create(recursive: true);
    final dest = File('${inbox.path}/$base');
    final sink = dest.openWrite();
    c.send({'ok': true});
    try {
      while (true) {
        final frame = await c.recv();
        if (frame['end'] == true) break;
        final d = frame['data'];
        if (d is String) sink.add(fromHex(d));
      }
    } finally {
      await sink.close();
    }
    c.send({'saved': base});
  }

  Future<void> _hostPair(Conn c, Map<String, dynamic> m) async {
    final peerName = (m['name'] is String) ? m['name'] as String : 'Unknown device';
    String? early;
    late List<int> pka;
    late String peerId;
    try {
      peerId = _str(m, 'id');
      pka = fromHex(_str(m, 'public_key'), 32);
      if (m['v'] != protocolVersion) {
        early = '$name runs a different Nexus version. Update both devices.';
      } else if (await idFor(pka) != peerId || peerId == id) {
        early = "The device's key does not match its ID";
      } else if (!windowOpen) {
        early = '$name is not accepting pairing. Tap “Allow pairing” on it first.';
      }
    } on NexusError catch (e) {
      early = e.message;
    }
    final session = early == null ? _begin(Role.incoming, peerName) : null;
    if (early == null && session == null) early = '$name is busy pairing with another device';
    if (early != null || session == null) {
      c.send({'t': 'error', 'reason': early});
      addLog('Turned down a pairing request from $peerName: $early');
      notifyListeners();
      return;
    }
    attemptsLeft--;
    addLog('$peerName asked to pair');
    try {
      final nb = randomBytes(32);
      c.send({
        't': 'pair_hello_ack',
        'v': protocolVersion,
        'id': id,
        'name': name,
        'os': osLabel,
        'public_key': toHex(publicKey),
        'commit': toHex(await commitment(nb, publicKey, pka)),
      });
      final n = await c.recv();
      if (n['t'] != 'nonce') throw NexusError(unexpectedMessage);
      final na = fromHex(_str(n, 'nonce'), 32);
      c.send({'t': 'nonce', 'nonce': toHex(nb)});
      final code = await pairingCode(pka, publicKey, na, nb);
      await _agree(session, c, code);
      final s = seen[peerId];
      await _savePeer(PairedPeer(
        id: peerId,
        name: peerName,
        os: '${m['os'] ?? ''}',
        publicKey: toHex(pka),
        pairedAt: DateTime.now().millisecondsSinceEpoch ~/ 1000,
        lastAddr: s == null ? null : '${c.socket.remoteAddress.address}:${s.tcpPort}',
      ));
      _windowExpires = null;
      _finish(session);
    } on NexusError catch (e) {
      try {
        c.send({'t': 'error', 'reason': e.message});
      } catch (_) {}
      _finish(session, error: e.message);
    } catch (e) {
      _finish(session, error: 'Pairing failed: $e');
    }
  }

  /// Shows the code, then waits until both people confirm.
  Future<void> _agree(PairSession s, Conn c, String code) async {
    s.code = code;
    s.phase = Phase.compare;
    notifyListeners();

    bool? remote;
    Object? remoteError;
    final remoteDone = c.recv(decideTimeout + const Duration(seconds: 10)).then((m) {
      if (m['t'] != 'decision') throw NexusError(unexpectedMessage);
      remote = m['accept'] == true;
      if (remote == false) s.answer(false);
    }).catchError((Object e) {
      remoteError = e;
      s.answer(false);
    });

    final local = await s._decision.future.timeout(decideTimeout, onTimeout: () => false);
    if (remote == false) throw NexusError("${s.peerName} said the numbers don't match, or cancelled");
    final earlyError = remoteError;
    if (earlyError != null) throw earlyError is NexusError ? earlyError : NexusError('Connection lost');

    c.send({'t': 'decision', 'accept': local});
    if (!local) throw NexusError('You cancelled the pairing');
    if (remote == null) {
      s.phase = Phase.waiting;
      notifyListeners();
    }
    await remoteDone.timeout(decideTimeout, onTimeout: () {
      remoteError ??= NexusError('Nobody confirmed in time');
    });
    final lateError = remoteError;
    if (lateError != null) throw lateError is NexusError ? lateError : NexusError('Connection lost');
    if (remote != true) throw NexusError("${s.peerName} said the numbers don't match, or cancelled");
  }

  /* ------------------------------------------------------------ client */

  Future<void> pairWith(PairTarget t) async {
    final session = _begin(Role.outgoing, t.name);
    if (session == null) return;
    Conn? c;
    try {
      Socket sock;
      try {
        sock = await Socket.connect(t.host, t.port, timeout: const Duration(seconds: 4));
      } catch (_) {
        throw NexusError('Could not reach ${t.host}. Check that Nexus is open there and allowed through its firewall.');
      }
      final conn = c = Conn(sock);
      conn.send({'t': 'pair_hello', 'v': protocolVersion, 'id': id, 'name': name, 'os': osLabel, 'public_key': toHex(publicKey)});
      final a = await conn.recv();
      if (a['t'] != 'pair_hello_ack') throw NexusError(unexpectedMessage);
      final peerName = _str(a, 'name'), peerId = _str(a, 'id'), pkbHex = _str(a, 'public_key');
      if (a['v'] != protocolVersion) throw NexusError('$peerName runs a different Nexus version. Update both devices.');
      final pkb = fromHex(pkbHex, 32);
      if (await idFor(pkb) != peerId) throw NexusError("$peerName's key does not match its ID");
      if (t.expectedId != null && t.expectedId != peerId) throw NexusError('A different device answered at that address');
      if (t.expectedKey != null && t.expectedKey != pkbHex) {
        throw NexusError("$peerName's key differs from the one it announced");
      }
      session.peerName = peerName;
      notifyListeners();

      final na = randomBytes(32);
      conn.send({'t': 'nonce', 'nonce': toHex(na)});
      final n = await conn.recv();
      if (n['t'] != 'nonce') throw NexusError(unexpectedMessage);
      final nb = fromHex(_str(n, 'nonce'), 32);
      if (!sameBytes(await commitment(nb, pkb, publicKey), fromHex(_str(a, 'commit'), 32))) {
        conn.send({'t': 'error', 'reason': 'Commitment check failed'});
        throw NexusError('$peerName failed a security check. Pairing stopped.');
      }
      final code = await pairingCode(publicKey, pkb, na, nb);
      await _agree(session, conn, code);
      await _savePeer(PairedPeer(
        id: peerId,
        name: peerName,
        os: '${a['os'] ?? ''}',
        publicKey: pkbHex,
        pairedAt: DateTime.now().millisecondsSinceEpoch ~/ 1000,
        lastAddr: '${t.host}:${t.port}',
      ));
      _finish(session);
    } on NexusError catch (e) {
      _finish(session, error: e.message);
    } catch (e) {
      _finish(session, error: 'Pairing failed: $e');
    } finally {
      c?.close();
    }
  }

  /* ------------------------------------------------------------ status */

  Future<(int, Stats?)> _ping(PairedPeer p, String host, int port) async {
    final sw = Stopwatch()..start();
    Socket sock;
    try {
      sock = await Socket.connect(host, port, timeout: const Duration(seconds: 2));
    } catch (_) {
      throw NexusError('Not reachable');
    }
    final c = Conn(sock);
    try {
      final nonce = randomBytes(16);
      c.send({'t': 'ping', 'id': id, 'nonce': toHex(nonce)});
      final m = await c.recv(const Duration(seconds: 3));
      if (m['t'] != 'pong') throw NexusError(unexpectedMessage);
      final ss = await sharedSecret(_keyPair, fromHex(p.publicKey, 32));
      final rtt = sw.elapsedMilliseconds;
      if (!sameBytes(await pingMac(ss, nonce, p.id, id), fromHex(_str(m, 'mac')))) {
        throw NexusError('Answered, but failed the identity check');
      }
      // Stats are only trusted when their own signature checks out.
      Stats? stats;
      final sj = m['stats'], sm = m['stats_mac'];
      if (sj is String && sm is String) {
        try {
          if (sameBytes(await statsMac(ss, nonce, sj), fromHex(sm))) stats = Stats.tryParse(sj);
        } catch (_) {}
      }
      return (rtt, stats);
    } finally {
      c.close();
    }
  }

  Future<void> _checkAll() async {
    if (_checking) return;
    _checking = true;
    try {
      for (final p in List.of(paired)) {
        String? host;
        int? port;
        final s = seen[p.id];
        if (s != null && s.fresh) {
          host = s.addr.address;
          port = s.tcpPort;
        } else if (p.lastAddr != null) {
          final i = p.lastAddr!.lastIndexOf(':');
          if (i > 0) {
            host = p.lastAddr!.substring(0, i);
            port = int.tryParse(p.lastAddr!.substring(i + 1));
          }
        }
        if (host == null || port == null) continue;
        final wasOk = checks[p.id]?.ok ?? false;
        try {
          final (rtt, stats) = await _ping(p, host, port);
          if (!wasOk) addLog('${p.name} is online and verified ($rtt ms)');
          checks[p.id] = Check.ok(rtt, stats);
          final addr = '$host:$port';
          if (p.lastAddr != addr) {
            p.lastAddr = addr;
            await _savePeers();
          }
        } on NexusError catch (e) {
          if (wasOk) addLog('${p.name} stopped answering');
          checks[p.id] = Check.failed(e.message);
        } catch (e) {
          checks[p.id] = Check.failed('$e');
        }
      }
    } finally {
      _checking = false;
      notifyListeners();
    }
  }
}
