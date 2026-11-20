package app.nexus.nexus_mobile

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.FlutterEngineCache
import io.flutter.embedding.engine.dart.DartExecutor
import io.flutter.plugin.common.MethodChannel

/**
 * The one Nexus engine for the whole app run, shared by the Nexus screen and the
 * "Stay connected" service. When the phone starts (or Nexus was swiped away) the
 * service starts it without any screen, so paired PCs can still reach the phone.
 *
 * Its "nexus/bg" channel only needs the app, never a screen, so it keeps working
 * while Nexus runs in the background: ringing, and the Stay connected switch.
 */
object NexusEngine {
    fun get(context: Context): FlutterEngine {
        val cache = FlutterEngineCache.getInstance()
        cache.get("nexus")?.let { return it }
        val app = context.applicationContext
        val engine = FlutterEngine(app)
        MethodChannel(engine.dartExecutor.binaryMessenger, "nexus/bg").setMethodCallHandler { call, result ->
            when (call.method) {
                "ring" -> {
                    Ringer.start(app)
                    result.success(true)
                }
                "stopRing" -> {
                    Ringer.stop(app)
                    result.success(true)
                }
                "ringing" -> result.success(Ringer.ringing)
                "keepAlive" -> result.success(KeepAliveService.wanted(app))
                "setKeepAlive" -> {
                    KeepAliveService.setWanted(app, call.arguments == true)
                    result.success(true)
                }
                "keepAliveRunning" -> result.success(KeepAliveService.running)
                "openAutostart" -> {
                    openAutostart(app)
                    result.success(true)
                }
                else -> result.notImplemented()
            }
        }
        engine.dartExecutor.executeDartEntrypoint(DartExecutor.DartEntrypoint.createDefault())
        cache.put("nexus", engine)
        return engine
    }

    /** Xiaomi's Autostart list (other phones: Nexus's app settings). */
    private fun openAutostart(ctx: Context) {
        val tries = listOf(
            Intent().setComponent(
                ComponentName("com.miui.securitycenter", "com.miui.permcenter.autostart.AutoStartManagementActivity")
            ),
            Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:${ctx.packageName}")),
        )
        for (i in tries) {
            try {
                ctx.startActivity(i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                return
            } catch (e: Exception) {
            }
        }
    }
}
