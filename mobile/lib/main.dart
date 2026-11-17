import 'dart:math' as math;

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';

import 'node.dart';
import 'protocol.dart';

// The chosen look: black background, dark green boxes, teal buttons,
// dark orange gauges, white text. Simple mode by default, Detailed on request.
const bg = Color(0xFF000000);
const box = Color(0xFF0E3A2A);
const box2 = Color(0xFF145036);
const line = Color(0xFF1E6649);
const accent = Color(0xFF11A99A);
const online = Color(0xFF5EEAD4);
const ring = Color(0xFFC2410C);
const off = Color(0xFF6B7F75);
const bad = Color(0xFFF0736A);
const white = Colors.white;

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  final node = NexusNode()..start();
  runApp(NexusApp(node: node));
}

class NexusApp extends StatelessWidget {
  const NexusApp({super.key, required this.node});
  final NexusNode node;

  @override
  Widget build(BuildContext context) {
    final scheme = ColorScheme.fromSeed(
      seedColor: accent,
      brightness: Brightness.dark,
    ).copyWith(primary: accent, onPrimary: white, surface: box, onSurface: white);
    return MaterialApp(
      title: 'Nexus',
      debugShowCheckedModeBanner: false,
      themeMode: ThemeMode.dark,
      darkTheme: ThemeData(
        useMaterial3: true,
        colorScheme: scheme,
        scaffoldBackgroundColor: bg,
        textTheme: ThemeData.dark().textTheme.apply(bodyColor: white, displayColor: white),
      ),
      home: HomePage(node: node),
    );
  }
}

class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.node});
  final NexusNode node;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  final _ipField = TextEditingController();
  NexusNode get node => widget.node;

  @override
  void dispose() {
    _ipField.dispose();
    super.dispose();
  }

  List<PairedPeer> get _sorted =>
      [...node.paired]..sort((a, b) => a.name.toLowerCase().compareTo(b.name.toLowerCase()));

  int get _onlineCount => node.paired.where((p) {
        final k = node.statusOf(p).kind;
        return k == StatusKind.verified || k == StatusKind.checking;
      }).length;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: node,
      builder: (context, _) {
        if (!node.ready) {
          return const Scaffold(body: Center(child: CircularProgressIndicator(color: accent)));
        }
        return Stack(children: [
          Scaffold(
            body: SafeArea(
              child: ListView(
                padding: const EdgeInsets.fromLTRB(16, 12, 16, 32),
                children: [
                  if (node.startupError != null)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 12),
                      child: Text(node.startupError!, style: const TextStyle(color: bad)),
                    ),
                  ...(node.detailed ? _detailed(context) : _simple(context)),
                ],
              ),
            ),
          ),
          if (node.pair != null) _PairOverlay(node: node, session: node.pair!),
        ]);
      },
    );
  }

  /* ------------------------------------------------------------ shared */

  Widget _header(BuildContext context, {String? overline, required String title}) {
    return Row(children: [
      const _Logo(size: 28),
      const SizedBox(width: 12),
      Expanded(
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          if (overline != null)
            Text(overline, style: const TextStyle(fontSize: 11, letterSpacing: 2, fontWeight: FontWeight.w700)),
          Text(title, style: const TextStyle(fontSize: 22, fontWeight: FontWeight.w800, letterSpacing: 1.2)),
        ]),
      ),
      _ModeSwitch(detailed: node.detailed, onChanged: node.setDetailed),
    ]);
  }

  Widget _pairButton(String label) {
    if (node.windowOpen) {
      final left = node.windowLeft.inSeconds;
      return _Box(
        child: Row(children: [
          const Icon(Icons.wifi_tethering, color: online),
          const SizedBox(width: 10),
          Expanded(
            child: Text(
              'Pairing on · ${left ~/ 60}:${(left % 60).toString().padLeft(2, '0')}\nTap Pair on the other device',
              style: const TextStyle(fontWeight: FontWeight.w600),
            ),
          ),
          _SecondaryButton(label: 'Stop', onPressed: node.stopPairing),
        ]),
      );
    }
    return SizedBox(
      height: 52,
      child: FilledButton.icon(
        style: FilledButton.styleFrom(
          backgroundColor: accent,
          foregroundColor: white,
          shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(12)),
          textStyle: const TextStyle(fontWeight: FontWeight.w800, letterSpacing: 1.2, fontSize: 15),
        ),
        onPressed: node.allowPairing,
        icon: const Icon(Icons.add),
        label: Text(label),
      ),
    );
  }

  (Color, String) _statusLine(PeerStatus st) => switch (st.kind) {
        StatusKind.verified => (online, 'Online · ${st.rttMs} ms'),
        StatusKind.checking => (online, 'Online · checking'),
        StatusKind.notAnswering => (ring, 'Not answering'),
        StatusKind.identityChanged => (bad, 'Key changed · not trusted'),
        StatusKind.offline => (off, 'Offline'),
      };

  void _pair(Seen s) => node.pairWith(PairTarget(
        host: s.addr.address,
        port: s.tcpPort,
        name: s.name,
        expectedId: s.id,
        expectedKey: s.publicKey,
      ));

  /* ------------------------------------------------------------ simple */

  List<Widget> _simple(BuildContext context) {
    final peers = _sorted;
    return [
      _header(context, title: 'NEXUS'),
      const SizedBox(height: 18),
      _pairButton('ADD DEVICE'),
      const SizedBox(height: 18),
      Text('$_onlineCount of ${peers.length} devices online', style: const TextStyle(fontSize: 15)),
      const SizedBox(height: 10),
      if (peers.isEmpty)
        const _Box(
          child: Text(
            'No devices yet. Open Nexus on your PC, press “+ ADD DEVICE” on one of them, then tap “Pair” on the other.',
          ),
        ),
      for (final p in peers) ...[
        _Box(
          padding: const EdgeInsets.fromLTRB(18, 12, 12, 12),
          child: Row(children: [
            _Dot(color: _statusLine(node.statusOf(p)).$1),
            const SizedBox(width: 14),
            Expanded(
              child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                Text(p.name, style: const TextStyle(fontSize: 18, fontWeight: FontWeight.w700)),
                Text(_statusLine(node.statusOf(p)).$2),
              ]),
            ),
            _SecondaryButton(label: 'Details', onPressed: () => _showDetails(context, p)),
          ]),
        ),
        const SizedBox(height: 10),
      ],
      for (final s in node.nearby) ...[
        const SizedBox(height: 6),
        _NearbyBox(name: s.name, sub: '${s.os} · ${s.addr.address}', onPair: node.pair == null ? () => _pair(s) : null),
      ],
      const SizedBox(height: 24),
      Center(
        child: TextButton(
          onPressed: () => _rename(context),
          child: Text('This phone: ${node.name}', style: const TextStyle(color: white)),
        ),
      ),
    ];
  }

  /* ---------------------------------------------------------- detailed */

  List<Widget> _detailed(BuildContext context) {
    final peers = _sorted;
    return [
      _header(context, overline: 'CONTROL CENTER', title: 'MY DEVICES'),
      const SizedBox(height: 16),
      _pairButton('ALLOW PAIRING'),
      const SizedBox(height: 14),
      Row(children: [
        Expanded(child: _Tile(label: 'ONLINE', value: _onlineCount)),
        const SizedBox(width: 8),
        Expanded(child: _Tile(label: 'PAIRED', value: peers.length)),
        const SizedBox(width: 8),
        Expanded(child: _Tile(label: 'NEARBY', value: node.nearby.length)),
      ]),
      const SizedBox(height: 14),
      if (peers.isEmpty)
        const _Box(child: Text('No paired devices yet. Press “ALLOW PAIRING” on one device, then “Pair” on the other.')),
      for (final p in peers) ...[_deviceCard(context, p), const SizedBox(height: 12)],
      for (final s in node.nearby) ...[
        _NearbyBox(
          label: 'NEW SIGNAL',
          name: s.name,
          sub: '${s.os} · ${s.addr.address}',
          onPair: node.pair == null ? () => _pair(s) : null,
        ),
        const SizedBox(height: 10),
      ],
      const SizedBox(height: 6),
      _pairByIp(context),
      const SizedBox(height: 18),
      _activity(),
      const SizedBox(height: 12),
      _Box(
        child: Row(children: [
          Expanded(
            child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
              const Text('● THIS PHONE', style: TextStyle(color: online, fontSize: 11, letterSpacing: 1.5)),
              Text(node.name, style: const TextStyle(fontWeight: FontWeight.w700, fontSize: 16)),
              Text('key ${node.fingerprintText}', style: const TextStyle(fontFamily: 'monospace', fontSize: 12)),
            ]),
          ),
          _SecondaryButton(label: 'Rename', onPressed: () => _rename(context)),
        ]),
      ),
    ];
  }

  Widget _deviceCard(BuildContext context, PairedPeer p) {
    final st = node.statusOf(p);
    final (color, text) = _statusLine(st);
    final isOnline = st.kind == StatusKind.verified || st.kind == StatusKind.checking;
    final addr = node.addressOf(p);
    return GestureDetector(
      onTap: () => _showDetails(context, p),
      child: Container(
      decoration: BoxDecoration(
        color: box,
        borderRadius: BorderRadius.circular(12),
        border: Border(
          top: BorderSide(color: isOnline ? online : line, width: 3),
          left: const BorderSide(color: line),
          right: const BorderSide(color: line),
          bottom: const BorderSide(color: line),
        ),
      ),
      padding: const EdgeInsets.all(16),
      child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
        Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Expanded(
            child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text(p.name.toUpperCase(), style: const TextStyle(fontSize: 18, fontWeight: FontWeight.w800, letterSpacing: .5)),
              Text('${p.os}${addr != null ? ' · $addr' : ''}', style: const TextStyle(fontSize: 12.5)),
            ]),
          ),
          Text(text.toUpperCase(),
              style: TextStyle(color: color, fontSize: 11.5, fontWeight: FontWeight.w700, letterSpacing: 1)),
        ]),
        const SizedBox(height: 14),
        if (isOnline)
          Row(mainAxisAlignment: MainAxisAlignment.spaceAround, children: [
            _Gauge(value: st.stats?.cpu, label: 'CPU'),
            _Gauge(value: st.stats?.ram, label: 'RAM'),
            _Gauge(value: st.stats?.battery, label: 'BATTERY'),
          ])
        else
          Container(
            height: 76,
            alignment: Alignment.center,
            decoration: BoxDecoration(border: Border.all(color: line), borderRadius: BorderRadius.circular(8)),
            child: const Text('Not reachable right now'),
          ),
        const SizedBox(height: 12),
        Row(children: [
          Expanded(
            child: Text('key ${_fp(p.id)}', style: const TextStyle(fontFamily: 'monospace', fontSize: 11.5)),
          ),
          _SecondaryButton(label: 'Remove', onPressed: () => _confirmRemove(context, p)),
        ]),
      ]),
      ),
    );
  }

  /// The fingerprint is the device ID shown as `3F9A 0C21 77DE 51B0`, same as on the PC.
  String _fp(String id) => id.toUpperCase().replaceAllMapped(RegExp(r'.{4}'), (m) => '${m[0]} ').trim();

  Widget _pairByIp(BuildContext context) {
    void go() {
      final input = _ipField.text.trim();
      if (input.isEmpty || node.pair != null) return;
      var host = input;
      var port = defaultTcpPort;
      final i = input.lastIndexOf(':');
      if (i > 0) {
        host = input.substring(0, i);
        port = int.tryParse(input.substring(i + 1)) ?? defaultTcpPort;
      }
      FocusScope.of(context).unfocus();
      node.pairWith(PairTarget(host: host, port: port, name: host));
    }

    return Row(children: [
      Expanded(
        child: TextField(
          controller: _ipField,
          style: const TextStyle(color: white),
          keyboardType: const TextInputType.numberWithOptions(decimal: true),
          decoration: InputDecoration(
            labelText: 'Not listed? Pair by IP address',
            labelStyle: const TextStyle(color: white),
            hintText: '192.168.1.31',
            filled: true,
            fillColor: box,
            isDense: true,
            enabledBorder: OutlineInputBorder(borderSide: const BorderSide(color: line), borderRadius: BorderRadius.circular(10)),
            focusedBorder: OutlineInputBorder(borderSide: const BorderSide(color: accent), borderRadius: BorderRadius.circular(10)),
          ),
          onSubmitted: (_) => go(),
        ),
      ),
      const SizedBox(width: 8),
      _SecondaryButton(label: 'Pair', onPressed: go),
    ]);
  }

  Widget _activity() {
    return Container(
      decoration: BoxDecoration(color: box, borderRadius: BorderRadius.circular(12), border: Border.all(color: line)),
      child: Theme(
        data: Theme.of(context).copyWith(dividerColor: Colors.transparent),
        child: ExpansionTile(
          iconColor: white,
          collapsedIconColor: white,
          title: Text('ACTIVITY (${node.log.length})', style: const TextStyle(fontWeight: FontWeight.w700, letterSpacing: 1)),
          children: [
            for (final l in node.log.take(40))
              ListTile(
                dense: true,
                leading: Text(_ago(l.at), style: const TextStyle(fontFamily: 'monospace', color: white)),
                title: Text(l.text, style: const TextStyle(color: white)),
              ),
          ],
        ),
      ),
    );
  }

  /* ------------------------------------------------------------ dialogs */

  void _showDetails(BuildContext context, PairedPeer p) {
    showModalBottomSheet<void>(
      context: context,
      backgroundColor: box,
      showDragHandle: true,
      builder: (ctx) => ListenableBuilder(
        listenable: node,
        builder: (ctx, _) {
          if (!node.isPaired(p.id)) return const SizedBox(height: 80);
          final st = node.statusOf(p);
          final (color, text) = _statusLine(st);
          final addr = node.addressOf(p);
          return Padding(
            padding: const EdgeInsets.fromLTRB(20, 0, 20, 28),
            child: Column(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text(p.name.toUpperCase(), style: const TextStyle(fontSize: 20, fontWeight: FontWeight.w800)),
              const SizedBox(height: 4),
              Row(children: [_Dot(color: color), const SizedBox(width: 8), Text(text)]),
              const SizedBox(height: 18),
              Row(mainAxisAlignment: MainAxisAlignment.spaceAround, children: [
                _Gauge(value: st.stats?.cpu, label: 'CPU'),
                _Gauge(value: st.stats?.ram, label: 'RAM'),
                _Gauge(value: st.stats?.battery, label: 'BATTERY'),
              ]),
              const SizedBox(height: 18),
              Text('${p.os}${addr != null ? ' · $addr' : ''}'),
              Text('key ${_fp(p.id)}', style: const TextStyle(fontFamily: 'monospace', fontSize: 12)),
              const SizedBox(height: 16),
              ..._powerControls(context, p, st),
              if (st.kind == StatusKind.verified || st.kind == StatusKind.checking) ...[
                const SizedBox(height: 8),
                SizedBox(
                  width: double.infinity,
                  child: _SecondaryButton(
                    label: 'Browse files',
                    onPressed: () {
                      Navigator.pop(ctx);
                      Navigator.of(context).push(
                        MaterialPageRoute<void>(builder: (_) => FileBrowserPage(node: node, peer: p)),
                      );
                    },
                  ),
                ),
              ],
              const SizedBox(height: 8),
              _SecondaryButton(
                label: 'Remove device',
                onPressed: () async {
                  Navigator.pop(ctx);
                  await _confirmRemove(context, p);
                },
              ),
            ]),
          );
        },
      ),
    );
  }

  /// Lock / Restart / Shut down (when the computer is online) and Wake (when it's offline).
  /// Only shown for computers — a phone can't be controlled this way.
  List<Widget> _powerControls(BuildContext context, PairedPeer p, PeerStatus st) {
    if (!p.controllable) return const [];
    final isOnline = st.kind == StatusKind.verified || st.kind == StatusKind.checking;
    if (isOnline) {
      return [
        Row(children: [
          Expanded(child: _SecondaryButton(label: 'Lock', onPressed: () => node.sendCommand(p, 'lock'))),
          const SizedBox(width: 8),
          Expanded(child: _SecondaryButton(label: 'Restart', onPressed: () => _confirmPower(context, p, 'restart'))),
          const SizedBox(width: 8),
          Expanded(
            child: OutlinedButton(
              style: OutlinedButton.styleFrom(
                foregroundColor: white,
                backgroundColor: box2,
                side: const BorderSide(color: bad),
                minimumSize: const Size(0, 44),
                shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(10)),
                textStyle: const TextStyle(fontWeight: FontWeight.w700),
              ),
              onPressed: () => _confirmPower(context, p, 'shutdown'),
              child: const Text('Shut down'),
            ),
          ),
        ]),
      ];
    }
    if (p.macs.isNotEmpty) {
      return [
        SizedBox(
          width: double.infinity,
          height: 46,
          child: FilledButton.icon(
            style: FilledButton.styleFrom(
              backgroundColor: accent,
              foregroundColor: white,
              shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(10)),
              textStyle: const TextStyle(fontWeight: FontWeight.w800),
            ),
            onPressed: () => node.wake(p),
            icon: const Icon(Icons.power_settings_new),
            label: const Text('Wake'),
          ),
        ),
      ];
    }
    return const [];
  }

  Future<void> _confirmPower(BuildContext context, PairedPeer p, String action) async {
    final label = action == 'restart' ? 'Restart' : 'Shut down';
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: box,
        title: Text('$label ${p.name}?'),
        content: Text('This will $action the computer now.'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel', style: TextStyle(color: white))),
          FilledButton(
            style: FilledButton.styleFrom(backgroundColor: const Color(0xFFB4413A)),
            onPressed: () => Navigator.pop(ctx, true),
            child: Text('Yes, $label'),
          ),
        ],
      ),
    );
    if (ok == true) await node.sendCommand(p, action);
  }

  Future<void> _rename(BuildContext context) async {
    final ctl = TextEditingController(text: node.name);
    final result = await showDialog<String>(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: box,
        title: const Text('Name this phone'),
        content: TextField(
          controller: ctl,
          autofocus: true,
          maxLength: 40,
          decoration: const InputDecoration(helperText: 'Other devices see this name'),
          onSubmitted: (v) => Navigator.pop(ctx, v),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx), child: const Text('Cancel', style: TextStyle(color: white))),
          FilledButton(onPressed: () => Navigator.pop(ctx, ctl.text), child: const Text('Save')),
        ],
      ),
    );
    ctl.dispose();
    if (result != null) await node.rename(result);
  }

  Future<void> _confirmRemove(BuildContext context, PairedPeer p) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: box,
        title: Text('Remove ${p.name}?'),
        content: const Text('It will no longer be trusted by this phone. You can pair again later.'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel', style: TextStyle(color: white))),
          FilledButton(
            style: FilledButton.styleFrom(backgroundColor: const Color(0xFFB4413A)),
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('Remove'),
          ),
        ],
      ),
    );
    if (ok == true) await node.removePeer(p.id);
  }
}

String _ago(DateTime at) {
  final s = DateTime.now().difference(at).inSeconds;
  if (s < 60) return '${s}s';
  if (s < 3600) return '${s ~/ 60}m';
  return '${s ~/ 3600}h';
}

/* ------------------------------------------------------------ pieces */

class _Box extends StatelessWidget {
  const _Box({required this.child, this.padding = const EdgeInsets.all(16)});
  final Widget child;
  final EdgeInsets padding;

  @override
  Widget build(BuildContext context) => Container(
        width: double.infinity,
        padding: padding,
        decoration: BoxDecoration(color: box, borderRadius: BorderRadius.circular(12), border: Border.all(color: line)),
        child: DefaultTextStyle.merge(style: const TextStyle(color: white), child: child),
      );
}

class _Dot extends StatelessWidget {
  const _Dot({required this.color});
  final Color color;
  @override
  Widget build(BuildContext context) =>
      Container(width: 12, height: 12, decoration: BoxDecoration(color: color, shape: BoxShape.circle));
}

class _SecondaryButton extends StatelessWidget {
  const _SecondaryButton({required this.label, required this.onPressed});
  final String label;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) => OutlinedButton(
        style: OutlinedButton.styleFrom(
          foregroundColor: white,
          backgroundColor: box2,
          side: const BorderSide(color: line),
          minimumSize: const Size(0, 44),
          shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(10)),
          textStyle: const TextStyle(fontWeight: FontWeight.w700),
        ),
        onPressed: onPressed,
        child: Text(label),
      );
}

class _Tile extends StatelessWidget {
  const _Tile({required this.label, required this.value});
  final String label;
  final int value;

  @override
  Widget build(BuildContext context) => _Box(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text(label, style: const TextStyle(fontSize: 11, letterSpacing: 1.5, fontWeight: FontWeight.w700)),
          Text(value.toString().padLeft(2, '0'), style: const TextStyle(fontSize: 26, fontWeight: FontWeight.w800)),
        ]),
      );
}

class _NearbyBox extends StatelessWidget {
  const _NearbyBox({this.label = 'Found nearby', required this.name, required this.sub, required this.onPair});
  final String label, name, sub;
  final VoidCallback? onPair;

  @override
  Widget build(BuildContext context) => Container(
        padding: const EdgeInsets.fromLTRB(18, 12, 12, 12),
        decoration: BoxDecoration(borderRadius: BorderRadius.circular(12), border: Border.all(color: accent)),
        child: Row(children: [
          Expanded(
            child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text(label, style: const TextStyle(color: online, fontSize: 12, fontWeight: FontWeight.w700, letterSpacing: 1)),
              Text(name, style: const TextStyle(color: white, fontSize: 17, fontWeight: FontWeight.w700)),
              Text(sub, style: const TextStyle(color: white, fontSize: 12)),
            ]),
          ),
          FilledButton(
            style: FilledButton.styleFrom(
              backgroundColor: accent,
              foregroundColor: white,
              minimumSize: const Size(0, 44),
              shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(10)),
              textStyle: const TextStyle(fontWeight: FontWeight.w800),
            ),
            onPressed: onPair,
            child: const Text('Pair'),
          ),
        ]),
      );
}

class _ModeSwitch extends StatelessWidget {
  const _ModeSwitch({required this.detailed, required this.onChanged});
  final bool detailed;
  final ValueChanged<bool> onChanged;

  Widget _seg(String label, bool value) {
    final on = detailed == value;
    return GestureDetector(
      onTap: on ? null : () => onChanged(value),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        decoration: BoxDecoration(color: on ? accent : Colors.transparent, borderRadius: BorderRadius.circular(8)),
        child: Text(label, style: const TextStyle(color: white, fontWeight: FontWeight.w700, fontSize: 13)),
      ),
    );
  }

  @override
  Widget build(BuildContext context) => Semantics(
        label: 'Simple or detailed view',
        child: Container(
          padding: const EdgeInsets.all(3),
          decoration: BoxDecoration(color: box, borderRadius: BorderRadius.circular(10), border: Border.all(color: line)),
          child: Row(mainAxisSize: MainAxisSize.min, children: [_seg('Simple', false), _seg('Detailed', true)]),
        ),
      );
}

class _Logo extends StatelessWidget {
  const _Logo({required this.size});
  final double size;

  @override
  Widget build(BuildContext context) => SizedBox(
        width: size,
        height: size,
        child: Stack(alignment: Alignment.center, children: [
          Transform.rotate(
            angle: math.pi / 4,
            child: Container(
              width: size * 0.68,
              height: size * 0.68,
              decoration: BoxDecoration(border: Border.all(color: accent, width: 2)),
            ),
          ),
          Text('N', style: TextStyle(color: white, fontWeight: FontWeight.w800, fontSize: size * 0.4)),
        ]),
      );
}

class _Gauge extends StatelessWidget {
  const _Gauge({required this.value, required this.label});
  final double? value;
  final String label;

  @override
  Widget build(BuildContext context) => Column(children: [
        SizedBox(
          width: 66,
          height: 66,
          child: CustomPaint(
            painter: _GaugePainter(value),
            child: Center(
              child: Text(value == null ? '—' : '${value!.round()}%',
                  style: const TextStyle(color: white, fontWeight: FontWeight.w700, fontSize: 14)),
            ),
          ),
        ),
        const SizedBox(height: 4),
        Text(label, style: const TextStyle(color: white, fontSize: 11, letterSpacing: 1.2, fontWeight: FontWeight.w700)),
      ]);
}

class _GaugePainter extends CustomPainter {
  _GaugePainter(this.value);
  final double? value;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    final r = rect.deflate(4);
    final track = Paint()
      ..color = line
      ..style = PaintingStyle.stroke
      ..strokeWidth = 5;
    canvas.drawArc(r, 0, math.pi * 2, false, track);
    final v = value;
    if (v != null && v > 0) {
      final arc = Paint()
        ..color = ring
        ..style = PaintingStyle.stroke
        ..strokeWidth = 5
        ..strokeCap = StrokeCap.round;
      canvas.drawArc(r, -math.pi / 2, math.pi * 2 * (v.clamp(0, 100) / 100), false, arc);
    }
  }

  @override
  bool shouldRepaint(_GaugePainter old) => old.value != value;
}

/// Browse a paired device's files: navigate folders, download, and upload.
class FileBrowserPage extends StatefulWidget {
  const FileBrowserPage({super.key, required this.node, required this.peer});
  final NexusNode node;
  final PairedPeer peer;

  @override
  State<FileBrowserPage> createState() => _FileBrowserPageState();
}

class _FileBrowserPageState extends State<FileBrowserPage> {
  final List<String> _path = [];
  List<FsEntry> _entries = [];
  bool _loading = true;
  bool _busy = false;
  String? _error;
  String? _status;

  NexusNode get node => widget.node;
  PairedPeer get peer => widget.peer;
  String get _rel => _path.join('/');

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final list = await node.fsList(peer, _rel);
      if (!mounted) return;
      setState(() {
        _entries = list;
        _loading = false;
      });
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _entries = [];
        _loading = false;
        _error = e is NexusError ? e.message : '$e';
      });
    }
  }

  void _into(String name) {
    setState(() => _path.add(name));
    _load();
  }

  void _up() {
    if (_path.isEmpty) return;
    setState(() => _path.removeLast());
    _load();
  }

  String _child(String name) => _path.isEmpty ? name : '$_rel/$name';

  Future<void> _download(FsEntry e) async {
    setState(() {
      _busy = true;
      _status = 'Downloading ${e.name}…';
    });
    try {
      final saved = await node.fsGet(peer, _child(e.name), e.name);
      if (mounted) setState(() => _status = 'Saved to $saved');
    } catch (err) {
      if (mounted) setState(() => _status = 'Download failed: ${err is NexusError ? err.message : err}');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _upload() async {
    final picked = await FilePicker.platform.pickFiles();
    final path = picked?.files.single.path;
    if (path == null) return;
    setState(() {
      _busy = true;
      _status = 'Uploading…';
    });
    try {
      final saved = await node.fsPut(peer, path);
      if (mounted) setState(() => _status = 'Uploaded $saved to ${peer.name}’s “Nexus Inbox”');
    } catch (err) {
      if (mounted) setState(() => _status = 'Upload failed: ${err is NexusError ? err.message : err}');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      backgroundColor: bg,
      appBar: AppBar(
        backgroundColor: box,
        foregroundColor: white,
        title: Text('${peer.name} · files', style: const TextStyle(fontWeight: FontWeight.w700)),
        actions: [
          IconButton(onPressed: _loading ? null : _load, icon: const Icon(Icons.refresh)),
        ],
      ),
      floatingActionButton: FloatingActionButton.extended(
        backgroundColor: accent,
        foregroundColor: white,
        onPressed: _busy ? null : _upload,
        icon: const Icon(Icons.upload_file),
        label: const Text('Upload'),
      ),
      body: Column(
        children: [
          // Breadcrumb
          Container(
            width: double.infinity,
            color: box,
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
            child: Row(children: [
              const Icon(Icons.folder_open, size: 16, color: online),
              const SizedBox(width: 8),
              Expanded(
                child: Text('/${_path.join('/')}',
                    style: const TextStyle(color: white, fontFamily: 'monospace', fontSize: 13), overflow: TextOverflow.ellipsis),
              ),
              if (_path.isNotEmpty)
                TextButton(onPressed: _busy ? null : _up, child: const Text('Up', style: TextStyle(color: online))),
            ]),
          ),
          if (_busy) const LinearProgressIndicator(color: accent, backgroundColor: box2, minHeight: 2),
          if (_status != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
              child: Text(_status!, style: const TextStyle(color: online, fontSize: 13)),
            ),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
              child: Text(_error!, style: const TextStyle(color: bad)),
            ),
          Expanded(
            child: _loading
                ? const Center(child: CircularProgressIndicator(color: accent))
                : ListView(
                    children: [
                      for (final e in _entries)
                        ListTile(
                          leading: Icon(e.dir ? Icons.folder : Icons.insert_drive_file, color: e.dir ? online : white),
                          title: Text(e.name, style: const TextStyle(color: white)),
                          subtitle: e.dir ? null : Text(_humanSize(e.size), style: const TextStyle(color: off)),
                          trailing: e.dir
                              ? const Icon(Icons.chevron_right, color: white)
                              : IconButton(
                                  icon: const Icon(Icons.download, color: online),
                                  onPressed: _busy ? null : () => _download(e),
                                ),
                          onTap: e.dir ? () => _into(e.name) : null,
                        ),
                      if (_entries.isEmpty)
                        const Padding(
                          padding: EdgeInsets.all(24),
                          child: Center(child: Text('This folder is empty', style: TextStyle(color: off))),
                        ),
                    ],
                  ),
          ),
        ],
      ),
    );
  }
}

String _humanSize(int bytes) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  var v = bytes.toDouble();
  var i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return i == 0 ? '$bytes B' : '${v.toStringAsFixed(1)} ${units[i]}';
}

/// Covers the screen while a pairing is in progress.
class _PairOverlay extends StatelessWidget {
  const _PairOverlay({required this.node, required this.session});
  final NexusNode node;
  final PairSession session;

  @override
  Widget build(BuildContext context) {
    final s = session;
    final title = s.role == Role.outgoing ? 'Pair with ${s.peerName}' : '${s.peerName} wants to pair';
    final code = s.code ?? '';
    final cancel = Align(alignment: Alignment.centerRight, child: _SecondaryButton(label: 'Cancel', onPressed: node.closePair));

    final List<Widget> body = switch (s.phase) {
      Phase.connecting => [
          Row(children: [
            const SizedBox(width: 20, height: 20, child: CircularProgressIndicator(strokeWidth: 2, color: accent)),
            const SizedBox(width: 12),
            Expanded(child: Text('Connecting to ${s.peerName}…')),
          ]),
          const SizedBox(height: 16),
          cancel,
        ],
      Phase.compare || Phase.waiting => [
          Text('Check that ${s.peerName} shows the same number:'),
          const SizedBox(height: 14),
          Container(
            width: double.infinity,
            padding: const EdgeInsets.symmetric(vertical: 14),
            decoration: BoxDecoration(color: box2, borderRadius: BorderRadius.circular(12)),
            child: Text(
              code.length == 6 ? '${code.substring(0, 3)} ${code.substring(3)}' : code,
              textAlign: TextAlign.center,
              style: const TextStyle(fontSize: 42, fontWeight: FontWeight.w800, fontFamily: 'monospace', letterSpacing: 2),
            ),
          ),
          const SizedBox(height: 18),
          if (s.phase == Phase.compare)
            Row(children: [
              Expanded(child: _SecondaryButton(label: "Don't match", onPressed: () => s.answer(false))),
              const SizedBox(width: 10),
              Expanded(
                child: FilledButton(
                  style: FilledButton.styleFrom(
                    backgroundColor: accent,
                    foregroundColor: white,
                    minimumSize: const Size(0, 44),
                    textStyle: const TextStyle(fontWeight: FontWeight.w800),
                  ),
                  onPressed: () => s.answer(true),
                  child: const Text('Numbers match'),
                ),
              ),
            ])
          else ...[
            Row(children: [
              const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2, color: accent)),
              const SizedBox(width: 12),
              Expanded(child: Text('Waiting for ${s.peerName} to confirm…')),
            ]),
            const SizedBox(height: 10),
            cancel,
          ],
        ],
      Phase.done => [
          Row(children: [
            const Icon(Icons.check_circle, color: online),
            const SizedBox(width: 10),
            Expanded(child: Text('Paired with ${s.peerName}', style: const TextStyle(fontSize: 17, fontWeight: FontWeight.w700))),
          ]),
          const SizedBox(height: 6),
          const Text('It now shows in your device list.'),
          const SizedBox(height: 16),
          Align(
            alignment: Alignment.centerRight,
            child: FilledButton(
              style: FilledButton.styleFrom(backgroundColor: accent, foregroundColor: white),
              onPressed: node.closePair,
              child: const Text('Done'),
            ),
          ),
        ],
      Phase.failed => [
          Text(s.error ?? 'Pairing failed', style: const TextStyle(color: bad)),
          const SizedBox(height: 16),
          Align(alignment: Alignment.centerRight, child: _SecondaryButton(label: 'Close', onPressed: node.closePair)),
        ],
    };

    return Positioned.fill(
      child: Container(
        color: Colors.black87,
        alignment: Alignment.center,
        padding: const EdgeInsets.all(20),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: Material(
            color: box,
            shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14), side: const BorderSide(color: line)),
            child: Padding(
              padding: const EdgeInsets.all(20),
              child: DefaultTextStyle.merge(
                style: const TextStyle(color: white),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(title, style: const TextStyle(fontSize: 19, fontWeight: FontWeight.w800)),
                    const SizedBox(height: 14),
                    ...body,
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
