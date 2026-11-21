package app.nexus.nexus_mobile

import android.app.AppOpsManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Process
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
                "notify" -> {
                    notify(app, call.argument<String>("title") ?: "Nexus", call.argument<String>("text") ?: "")
                    result.success(true)
                }
                "autostart" -> result.success(autostart(app))
                // "All files access": lets paired devices browse this phone's photos and
                // downloads (Android 11 and newer).
                "allFiles" -> result.success(
                    Build.VERSION.SDK_INT >= 30 && android.os.Environment.isExternalStorageManager()
                )
                "askAllFiles" -> {
                    if (Build.VERSION.SDK_INT >= 30) {
                        val i = try {
                            Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:${app.packageName}"))
                        } catch (e: Exception) {
                            Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)
                        }
                        try {
                            app.startActivity(i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                        } catch (e: Exception) {
                            try {
                                app.startActivity(
                                    Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                                )
                            } catch (e2: Exception) {
                            }
                        }
                    }
                    result.success(Build.VERSION.SDK_INT >= 30)
                }
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

    /**
     * Whether Xiaomi's Autostart is on for Nexus: true/false, or null when the phone
     * doesn't say (not a Xiaomi phone, or the check isn't available). Xiaomi keeps it
     * as a hidden app permission (number 10008), which only this roundabout way can read.
     */
    private fun autostart(ctx: Context): Boolean? {
        if (!Build.MANUFACTURER.equals("Xiaomi", ignoreCase = true)) return null
        return try {
            val ops = ctx.getSystemService(Context.APP_OPS_SERVICE) as AppOpsManager
            val check = AppOpsManager::class.java.getMethod(
                "checkOpNoThrow", Int::class.javaPrimitiveType, Int::class.javaPrimitiveType, String::class.java,
            )
            (check.invoke(ops, 10008, Process.myUid(), ctx.packageName) as Int) == AppOpsManager.MODE_ALLOWED
        } catch (e: Throwable) {
            null
        }
    }

    /** A plain notification that opens Nexus when tapped. */
    private fun notify(ctx: Context, title: String, text: String) {
        try {
            val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as android.app.NotificationManager
            if (Build.VERSION.SDK_INT >= 26) {
                nm.createNotificationChannel(
                    android.app.NotificationChannel("nexus_updates", "Updates", android.app.NotificationManager.IMPORTANCE_DEFAULT)
                )
            }
            val open = android.app.PendingIntent.getActivity(
                ctx, 5, Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                android.app.PendingIntent.FLAG_IMMUTABLE,
            )
            val b = if (Build.VERSION.SDK_INT >= 26) android.app.Notification.Builder(ctx, "nexus_updates") else {
                @Suppress("DEPRECATION")
                android.app.Notification.Builder(ctx)
            }
            nm.notify(
                4715,
                b.setContentTitle(title).setContentText(text).setSmallIcon(android.R.drawable.stat_sys_download_done)
                    .setContentIntent(open).setAutoCancel(true).build(),
            )
        } catch (e: Exception) {
        }
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
