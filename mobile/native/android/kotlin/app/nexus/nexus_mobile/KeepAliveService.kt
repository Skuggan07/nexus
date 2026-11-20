package app.nexus.nexus_mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

/**
 * "Stay connected": keeps Nexus running in the background so paired PCs can reach the
 * phone with the screen off or Nexus swiped away (to ring it, send files, see its
 * battery). Android requires a notification for this; it's the quietest kind.
 *
 * It saves battery by doing nothing itself: no wake locks, no timers. Nexus just waits
 * for a PC to knock, and while it's in the background it checks in about once a minute
 * instead of every few seconds (see the Dart side's "resting" mode).
 */
class KeepAliveService : Service() {
    companion object {
        @Volatile
        var running = false

        private const val PREFS = "nexus"
        private const val KEY = "keep_alive"
        private const val CHANNEL = "nexus_connected"
        private const val NOTE_ID = 4713

        fun wanted(ctx: Context): Boolean =
            ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY, false)

        fun setWanted(ctx: Context, on: Boolean) {
            ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean(KEY, on).apply()
            if (on) start(ctx) else ctx.stopService(Intent(ctx, KeepAliveService::class.java))
        }

        fun start(ctx: Context) {
            val i = Intent(ctx, KeepAliveService::class.java)
            try {
                if (Build.VERSION.SDK_INT >= 26) ctx.startForegroundService(i) else ctx.startService(i)
            } catch (e: Exception) {
            }
        }
    }

    override fun onCreate() {
        super.onCreate()
        running = true
        // Starts Nexus if it isn't running yet (after a restart, or after being swiped away).
        NexusEngine.get(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (!wanted(this)) {
            stopSelf()
            return START_NOT_STICKY
        }
        try {
            val n = note()
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(NOTE_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
            } else {
                startForeground(NOTE_ID, n)
            }
        } catch (e: Exception) {
            stopSelf()
            return START_NOT_STICKY
        }
        return START_STICKY
    }

    private fun note(): Notification {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= 26) {
            val ch = NotificationChannel(CHANNEL, "Stay connected", NotificationManager.IMPORTANCE_MIN)
            ch.setShowBadge(false)
            ch.description = "Lets your PCs reach this phone while Nexus is closed"
            nm.createNotificationChannel(ch)
        }
        val open = PendingIntent.getActivity(
            this, 2, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val b = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL) else {
            @Suppress("DEPRECATION")
            Notification.Builder(this).setPriority(Notification.PRIORITY_MIN)
        }
        return b.setContentTitle("Nexus is connected")
            .setContentText("Your PCs can reach this phone")
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
            .setContentIntent(open)
            .setOngoing(true)
            .setShowWhen(false)
            .build()
    }

    override fun onDestroy() {
        running = false
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null
}

/** Starts "Stay connected" again after the phone restarts or Nexus is updated. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED, "android.intent.action.QUICKBOOT_POWERON" ->
                if (KeepAliveService.wanted(context)) KeepAliveService.start(context)
        }
    }
}
