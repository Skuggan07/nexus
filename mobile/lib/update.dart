import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';

/// This app's build number, set by the GitHub build (0 for a local build).
const appBuild = int.fromEnvironment('NEXUS_BUILD');

const _latest = 'https://api.github.com/repos/Skuggan07/nexus/releases/latest';
const _asset = 'nexus-android.apk';
const _native = MethodChannel('nexus/screen');

enum UpdatePhase { idle, checking, upToDate, available, downloading, installing, failed }

/// Checks GitHub for a newer Nexus and installs it in place, so updates no longer
/// need an uninstall. Android still shows its own "Update?" confirmation.
class Updater extends ChangeNotifier {
  UpdatePhase phase = UpdatePhase.idle;
  int? newBuild;
  String? _url;
  int _size = 0;
  double progress = 0;
  String? message;

  bool get hasUpdate => phase == UpdatePhase.available || phase == UpdatePhase.downloading;

  void _set(UpdatePhase p, {String? msg}) {
    phase = p;
    message = msg;
    notifyListeners();
  }

  Future<void> check() async {
    if (phase == UpdatePhase.checking || phase == UpdatePhase.downloading) return;
    _set(UpdatePhase.checking);
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 10);
    try {
      final req = await client.getUrl(Uri.parse(_latest));
      req.headers.set(HttpHeaders.userAgentHeader, 'Nexus-updater');
      req.headers.set(HttpHeaders.acceptHeader, 'application/vnd.github+json');
      final res = await req.close().timeout(const Duration(seconds: 20));
      final body = await res.transform(utf8.decoder).join();
      final v = jsonDecode(body);
      final tag = v is Map ? '${v['tag_name'] ?? ''}' : '';
      final n = int.tryParse(tag.startsWith('build-') ? tag.substring(6) : '');
      Map? asset;
      if (v is Map && v['assets'] is List) {
        for (final a in v['assets'] as List) {
          if (a is Map && a['name'] == _asset) asset = a;
        }
      }
      if (n != null && asset != null && n > appBuild) {
        newBuild = n;
        _url = '${asset['browser_download_url']}';
        _size = (asset['size'] as num?)?.toInt() ?? 0;
        _set(UpdatePhase.available);
      } else {
        _set(UpdatePhase.upToDate);
      }
    } catch (e) {
      _set(UpdatePhase.failed, msg: 'Could not reach GitHub');
    } finally {
      client.close();
    }
  }

  /// Downloads the update and hands it to Android's installer.
  Future<void> install() async {
    final url = _url;
    if (url == null || phase == UpdatePhase.downloading) return;
    progress = 0;
    _set(UpdatePhase.downloading);
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 10);
    try {
      final dir = await getTemporaryDirectory();
      final file = File('${dir.path}/nexus-update.apk');
      final req = await client.getUrl(Uri.parse(url));
      req.headers.set(HttpHeaders.userAgentHeader, 'Nexus-updater');
      final res = await req.close();
      if (res.statusCode != 200) throw Exception('HTTP ${res.statusCode}');
      final total = res.contentLength > 0 ? res.contentLength : _size;
      final sink = file.openWrite();
      var done = 0;
      await for (final chunk in res) {
        sink.add(chunk);
        done += chunk.length;
        if (total > 0) {
          progress = done / total;
          notifyListeners();
        }
      }
      await sink.close();
      if (done < 1000000 || (total > 0 && done != total)) throw Exception('incomplete download');
      final r = await _native.invokeMethod<String>('installApk', {'path': file.path});
      if (r == 'permission') {
        _set(UpdatePhase.available,
            msg: 'Allow Nexus to install apps ("Install unknown apps"), then tap Update again.');
      } else if (r == 'started') {
        _set(UpdatePhase.installing, msg: 'Confirm "Update" in the Android window.');
      } else {
        _set(UpdatePhase.available, msg: 'Could not start the installer. Try again.');
      }
    } catch (e) {
      _set(UpdatePhase.available, msg: 'Download failed — try again.');
    } finally {
      client.close();
    }
  }
}
