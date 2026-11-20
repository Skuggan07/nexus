package app.nexus.nexus_mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.MediaPlayer
import android.media.RingtoneManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.VibrationEffect
import android.os.Vibrator

/**
 * Find my phone: plays the alarm sound at full volume (alarms sound even on silent
 * and vibrate) and vibrates, until Stop is tapped, Nexus is opened, the PC says stop,
 * or two minutes pass.
 */
object Ringer {
    @Volatile
    var ringing = false
        private set

    private const val CHANNEL = "nexus_ring"
    private const val NOTE_ID = 4714
    private const val MAX_MS = 120_000L

    private val main = Handler(Looper.getMainLooper())
    private var app: Context? = null
    private var player: MediaPlayer? = null
    private var oldVolume = -1
    private val timeUp = Runnable { app?.let { stopNow(it) } }

    private val alarm: AudioAttributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_ALARM)
        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
        .build()

    fun start(ctx: Context) {
        val a = ctx.applicationContext
        main.post { startNow(a) }
    }

    fun stop(ctx: Context) {
        val a = ctx.applicationContext
        main.post { stopNow(a) }
    }

    private fun startNow(ctx: Context) {
        if (ringing) return
        ringing = true
        app = ctx
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        try {
            oldVolume = am.getStreamVolume(AudioManager.STREAM_ALARM)
            am.setStreamVolume(AudioManager.STREAM_ALARM, am.getStreamMaxVolume(AudioManager.STREAM_ALARM), 0)
        } catch (e: Exception) {
            oldVolume = -1
        }
        val sounds = listOfNotNull(
            RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM),
            RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE),
            RingtoneManager.getDefaultUri(RingtoneManager.TYPE_NOTIFICATION),
        )
        for (uri in sounds) {
            val p = MediaPlayer()
            try {
                p.setAudioAttributes(alarm)
                p.setDataSource(ctx, uri)
                p.isLooping = true
                p.prepare()
                p.start()
                player = p
                break
            } catch (e: Exception) {
                p.release()
            }
        }
        try {
            val v = ctx.getSystemService(Context.VIBRATOR_SERVICE) as Vibrator
            val pattern = longArrayOf(0, 800, 500)
            if (Build.VERSION.SDK_INT >= 26) {
                @Suppress("DEPRECATION")
                v.vibrate(VibrationEffect.createWaveform(pattern, 0), alarm)
            } else {
                @Suppress("DEPRECATION")
                v.vibrate(pattern, 0)
            }
        } catch (e: Exception) {
        }
        try {
            showNote(ctx)
        } catch (e: Exception) {
        }
        main.postDelayed(timeUp, MAX_MS)
    }

    private fun stopNow(ctx: Context) {
        if (!ringing) return
        ringing = false
        main.removeCallbacks(timeUp)
        try {
            player?.stop()
        } catch (e: Exception) {
        }
        player?.release()
        player = null
        try {
            (ctx.getSystemService(Context.VIBRATOR_SERVICE) as Vibrator).cancel()
        } catch (e: Exception) {
        }
        if (oldVolume >= 0) {
            try {
                (ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager)
                    .setStreamVolume(AudioManager.STREAM_ALARM, oldVolume, 0)
            } catch (e: Exception) {
            }
        }
        try {
            (ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).cancel(NOTE_ID)
        } catch (e: Exception) {
        }
    }

    private fun showNote(ctx: Context) {
        val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= 26) {
            val ch = NotificationChannel(CHANNEL, "Find my phone", NotificationManager.IMPORTANCE_HIGH)
            ch.setSound(null, null) // Nexus plays the alarm itself
            ch.enableVibration(false)
            nm.createNotificationChannel(ch)
        }
        val stop = PendingIntent.getBroadcast(
            ctx, 3, Intent(ctx, RingStopReceiver::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val b = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(ctx, CHANNEL) else {
            @Suppress("DEPRECATION")
            Notification.Builder(ctx).setPriority(Notification.PRIORITY_MAX)
        }
        val n = b.setContentTitle("Found it?")
            .setContentText("Your PC is ringing this phone. Tap Stop.")
            .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
            .setCategory(Notification.CATEGORY_ALARM)
            .setContentIntent(stop)
            .setDeleteIntent(stop)
            .setAutoCancel(true)
            .addAction(Notification.Action.Builder(null, "Stop", stop).build())
            .build()
        nm.notify(NOTE_ID, n)
    }
}

/** The Stop button on the Find my phone notification. */
class RingStopReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        Ringer.stop(context)
    }
}
