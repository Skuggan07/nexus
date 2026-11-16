import 'package:flutter/material.dart';

import 'node.dart';
import 'protocol.dart';

const accent = Color(0xFF2A55E5);
const good = Color(0xFF2EAA70);
const warn = Color(0xFFD69828);
const bad = Color(0xFFDE5246);
const grey = Color(0xFF8C94A2);

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
    return MaterialApp(
      title: 'Nexus',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(colorSchemeSeed: accent, useMaterial3: true, brightness: Brightness.light),
      darkTheme: ThemeData(colorSchemeSeed: accent, useMaterial3: true, brightness: Brightness.dark),
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

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: node,
      builder: (context, _) {
        if (!node.ready) {
          return const Scaffold(body: Center(child: CircularProgressIndicator()));
        }
        return Stack(children: [
          Scaffold(
            appBar: AppBar(
              title: const Text('Nexus', style: TextStyle(fontWeight: FontWeight.w700)),
              centerTitle: false,
            ),
            body: SafeArea(
              child: ListView(
                padding: const EdgeInsets.fromLTRB(16, 4, 16, 32),
                children: [
                  if (node.startupError != null)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 12),
                      child: Text(node.startupError!, style: const TextStyle(color: bad)),
                    ),
                  _thisDevice(context),
                  const SizedBox(height: 20),
                  _heading(context, 'My devices', node.paired.length),
                  if (node.paired.isEmpty)
                    _hint(context, 'No paired devices yet. Tap “Allow pairing” on one device, then “Pair” on the other.'),
                  for (final p in [...node.paired]..sort((a, b) => a.name.toLowerCase().compareTo(b.name.toLowerCase())))
                    _pairedTile(context, p),
                  const SizedBox(height: 20),
                  _heading(context, 'Nearby', node.nearby.length),
                  if (node.nearby.isEmpty)
                    Padding(
                      padding: const EdgeInsets.symmetric(vertical: 8),
                      child: Row(children: [
                        const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2)),
                        const SizedBox(width: 12),
                        Expanded(child: _hintText(context, 'Looking for devices running Nexus on this Wi-Fi…')),
                      ]),
                    ),
                  for (final s in node.nearby) _nearbyTile(context, s),
                  const SizedBox(height: 12),
                  _pairByIp(context),
                  const SizedBox(height: 20),
                  _activity(context),
                ],
              ),
            ),
          ),
          if (node.pair != null) _PairOverlay(node: node, session: node.pair!),
        ]);
      },
    );
  }

  Widget _thisDevice(BuildContext context) {
    final t = Theme.of(context).textTheme;
    final left = node.windowLeft.inSeconds;
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Row(children: [
            Expanded(child: Text(node.name, style: t.titleLarge?.copyWith(fontWeight: FontWeight.w600))),
            IconButton(
              tooltip: 'Rename this phone',
              icon: const Icon(Icons.edit_outlined),
              onPressed: () => _rename(context),
            ),
          ]),
          Text('This phone · $osLabel · key ${node.fingerprintText}', style: t.bodySmall?.copyWith(color: grey)),
          const SizedBox(height: 14),
          if (node.windowOpen)
            Row(children: [
              const Icon(Icons.check_circle, color: good, size: 20),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  'Pairing allowed · ${left ~/ 60}:${(left % 60).toString().padLeft(2, '0')} left\nTap Pair on the other device',
                  style: t.bodyMedium,
                ),
              ),
              TextButton(onPressed: node.stopPairing, child: const Text('Stop')),
            ])
          else
            SizedBox(
              width: double.infinity,
              child: FilledButton.icon(
                onPressed: node.allowPairing,
                icon: const Icon(Icons.link),
                label: const Text('Allow pairing'),
              ),
            ),
        ]),
      ),
    );
  }

  Widget _pairedTile(BuildContext context, PairedPeer p) {
    final st = node.statusOf(p);
    final (color, label) = switch (st.kind) {
      StatusKind.verified => (good, 'Online · verified · ${st.rttMs} ms'),
      StatusKind.checking => (warn, 'On the network · checking'),
      StatusKind.notAnswering => (warn, 'Not answering: ${st.reason ?? ''}'),
      StatusKind.identityChanged => (bad, 'Key changed · not trusted'),
      StatusKind.offline => (grey, 'Offline'),
    };
    final addr = node.addressOf(p);
    return Card(
      child: ListTile(
        leading: Icon(Icons.circle, color: color, size: 14),
        title: Text(p.name, style: const TextStyle(fontWeight: FontWeight.w600)),
        subtitle: Text('$label\n${p.os}${addr != null ? ' · $addr' : ''}'),
        isThreeLine: true,
        trailing: IconButton(
          tooltip: 'Remove',
          icon: const Icon(Icons.delete_outline),
          onPressed: () => _confirmRemove(context, p),
        ),
      ),
    );
  }

  Widget _nearbyTile(BuildContext context, Seen s) {
    return Card(
      child: ListTile(
        leading: const Icon(Icons.circle, color: accent, size: 14),
        title: Text(s.name, style: const TextStyle(fontWeight: FontWeight.w600)),
        subtitle: Text('${s.os} · ${s.addr.address}'),
        trailing: FilledButton.tonal(
          onPressed: node.pair == null
              ? () => node.pairWith(PairTarget(
                    host: s.addr.address,
                    port: s.tcpPort,
                    name: s.name,
                    expectedId: s.id,
                    expectedKey: s.publicKey,
                  ))
              : null,
          child: const Text('Pair'),
        ),
      ),
    );
  }

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
          keyboardType: const TextInputType.numberWithOptions(decimal: true),
          decoration: const InputDecoration(
            labelText: 'Not listed? Pair by IP address',
            hintText: '192.168.1.31',
            border: OutlineInputBorder(),
            isDense: true,
          ),
          onSubmitted: (_) => go(),
        ),
      ),
      const SizedBox(width: 8),
      OutlinedButton(onPressed: go, child: const Text('Pair')),
    ]);
  }

  Widget _activity(BuildContext context) {
    final t = Theme.of(context).textTheme;
    return Card(
      child: ExpansionTile(
        title: Text('Activity (${node.log.length})'),
        shape: const Border(),
        children: [
          for (final l in node.log.take(40))
            ListTile(
              dense: true,
              leading: Text(_ago(l.at), style: t.bodySmall?.copyWith(color: grey, fontFamily: 'monospace')),
              title: Text(l.text),
            ),
        ],
      ),
    );
  }

  Widget _heading(BuildContext context, String text, int count) => Padding(
        padding: const EdgeInsets.only(bottom: 6),
        child: Text('$text ($count)',
            style: Theme.of(context).textTheme.titleMedium?.copyWith(fontWeight: FontWeight.w700)),
      );

  Widget _hint(BuildContext context, String text) =>
      Padding(padding: const EdgeInsets.symmetric(vertical: 8), child: _hintText(context, text));

  Widget _hintText(BuildContext context, String text) =>
      Text(text, style: Theme.of(context).textTheme.bodyMedium?.copyWith(color: grey));

  Future<void> _rename(BuildContext context) async {
    final ctl = TextEditingController(text: node.name);
    final result = await showDialog<String>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Name this phone'),
        content: TextField(
          controller: ctl,
          autofocus: true,
          maxLength: 40,
          decoration: const InputDecoration(helperText: 'Other devices see this name'),
          onSubmitted: (v) => Navigator.pop(ctx, v),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx), child: const Text('Cancel')),
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
        title: Text('Remove ${p.name}?'),
        content: const Text('It will no longer be trusted by this phone. You can pair again later.'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
          FilledButton(
            style: FilledButton.styleFrom(backgroundColor: bad),
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

/// Covers the screen while a pairing is in progress.
class _PairOverlay extends StatelessWidget {
  const _PairOverlay({required this.node, required this.session});
  final NexusNode node;
  final PairSession session;

  @override
  Widget build(BuildContext context) {
    final t = Theme.of(context).textTheme;
    final s = session;
    final title = s.role == Role.outgoing ? 'Pair with ${s.peerName}' : '${s.peerName} wants to pair';
    final code = s.code ?? '';

    final List<Widget> body = switch (s.phase) {
      Phase.connecting => [
          Row(children: [
            const SizedBox(width: 20, height: 20, child: CircularProgressIndicator(strokeWidth: 2)),
            const SizedBox(width: 12),
            Expanded(child: Text('Connecting to ${s.peerName}…')),
          ]),
          const SizedBox(height: 16),
          Align(alignment: Alignment.centerRight, child: TextButton(onPressed: node.closePair, child: const Text('Cancel'))),
        ],
      Phase.compare || Phase.waiting => [
          Text('Check that ${s.peerName} shows the same number:'),
          const SizedBox(height: 16),
          Center(
            child: Text(
              code.length == 6 ? '${code.substring(0, 3)} ${code.substring(3)}' : code,
              style: t.displayMedium?.copyWith(fontWeight: FontWeight.w700, fontFamily: 'monospace', letterSpacing: 2),
            ),
          ),
          const SizedBox(height: 20),
          if (s.phase == Phase.compare)
            Row(children: [
              Expanded(
                child: OutlinedButton(onPressed: () => s.answer(false), child: const Text("Don't match")),
              ),
              const SizedBox(width: 12),
              Expanded(
                child: FilledButton(
                  onPressed: () => s.answer(true),
                  child: const Text('Numbers match'),
                ),
              ),
            ])
          else ...[
            Row(children: [
              const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2)),
              const SizedBox(width: 12),
              Expanded(child: Text('Waiting for ${s.peerName} to confirm…')),
            ]),
            Align(alignment: Alignment.centerRight, child: TextButton(onPressed: node.closePair, child: const Text('Cancel'))),
          ],
        ],
      Phase.done => [
          Row(children: [
            const Icon(Icons.check_circle, color: good),
            const SizedBox(width: 10),
            Expanded(child: Text('Paired with ${s.peerName}', style: t.titleMedium)),
          ]),
          const SizedBox(height: 6),
          const Text('It now appears under My devices.'),
          const SizedBox(height: 16),
          Align(alignment: Alignment.centerRight, child: FilledButton(onPressed: node.closePair, child: const Text('Done'))),
        ],
      Phase.failed => [
          Text(s.error ?? 'Pairing failed', style: const TextStyle(color: bad)),
          const SizedBox(height: 16),
          Align(alignment: Alignment.centerRight, child: TextButton(onPressed: node.closePair, child: const Text('Close'))),
        ],
    };

    return Positioned.fill(
      child: Container(
        color: Colors.black54,
        alignment: Alignment.center,
        padding: const EdgeInsets.all(20),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: Card(
            child: Padding(
              padding: const EdgeInsets.all(20),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(title, style: t.titleLarge?.copyWith(fontWeight: FontWeight.w600)),
                  const SizedBox(height: 14),
                  ...body,
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
