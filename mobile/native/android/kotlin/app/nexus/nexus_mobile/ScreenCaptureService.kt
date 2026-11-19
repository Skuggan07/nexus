package app.nexus.nexus_mobile

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.graphics.Bitmap
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.ImageReader
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.os.SystemClock
import android.util.DisplayMetrics
import android.view.Display
import java.io.ByteArrayOutputStream
import java.util.concurrent.ConcurrentLinkedQueue

/**
 * Shares this phone's screen. Runs as a foreground service (Android requires a
 * visible notification while the screen is being captured) and keeps the newest
 * frame as a JPEG that the Dart side picks up and sends to the paired viewer.
 */
class ScreenCaptureService : Service() {
    companion object {
        const val CHANNEL = "nexus_screen"
        @Volatile var running = false
        @Volatile var maxWidth = 1920
        @Volatile private var latest: ByteArray? = null
        @Volatile private var latestW = 0
        @Volatile private var latestH = 0
        @Volatile private var seq = 0L

        /** The running service, while the screen is shared. */
        @Volatile var instance: ScreenCaptureService? = null

        /** The screen recording Android gave us; sound is recorded from it too. */
        @Volatile var activeProjection: MediaProjection? = null

        /** True while the service also runs as a "microphone" service. Android hands
         *  over silence instead of the real sound while Nexus is in the background otherwise. */
        @Volatile var micMode = false

        private val soundLock = Any()
        private var recorder: AudioRecord? = null
        private var listeners = 0

        fun soundPermitted(ctx: android.content.Context) = Build.VERSION.SDK_INT >= 29 &&
            ctx.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED

        /**
         * Starts recording what this phone plays (Android 10+, needs the screen to be
         * shared and the "record audio" permission). Returns a problem, or null when it
         * works. Several listeners share one recording.
         */
        @android.annotation.SuppressLint("NewApi") // checked at the top
        fun startSound(ctx: android.content.Context): String? {
          synchronized(soundLock) {
            if (Build.VERSION.SDK_INT < 29) return "Sharing the phone's sound needs Android 10 or newer"
            if (!soundPermitted(ctx)) {
                return "On the phone, open Nexus and tap Allow next to \"Let paired devices hear this phone\""
            }
            val mp = activeProjection ?: return if (running) {
                "The phone shares its screen but Android gave Nexus no recording (A). On the phone, turn screen sharing off and on again"
            } else {
                "Screen sharing is off on the phone (B)"
            }
            if (!micMode && Build.VERSION.SDK_INT >= 30) {
                return "On the phone, turn \"Share this phone's screen\" off and on again, then turn on sound (C)"
            }
            if (recorder == null) {
                val cfg = AudioPlaybackCaptureConfiguration.Builder(mp)
                    .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
                    .addMatchingUsage(AudioAttributes.USAGE_GAME)
                    .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
                    .build()
                val format = AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(48000)
                    .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                    .build()
                val rec = try {
                    AudioRecord.Builder()
                        .setAudioFormat(format)
                        .setBufferSizeInBytes(48000 * 4 / 5)
                        .setAudioPlaybackCaptureConfig(cfg)
                        .build()
                } catch (e: Throwable) {
                    return "Could not record the phone's sound (D): ${e.message}"
                }
                if (rec.state != AudioRecord.STATE_INITIALIZED) {
                    rec.release()
                    return "Android would not let Nexus record the phone's sound (E)"
                }
                try {
                    rec.startRecording()
                } catch (e: Throwable) {
                    rec.release()
                    return "Could not record the phone's sound (F): ${e.message}"
                }
                sound.clear()
                recorder = rec
                Thread {
                    val buf = ByteArray(48000 * 4 / 25) // 40 ms
                    while (recorder === rec) {
                        val n = try { rec.read(buf, 0, buf.size) } catch (e: Throwable) { -1 }
                        if (n < 0) break
                        if (n > 0) {
                            sound.add(buf.copyOf(n))
                            while (sound.size > 25) sound.poll() // keep it close to live
                        }
                    }
                }.start()
            }
            listeners++
            return null
          }
        }

        fun stopSound() {
            synchronized(soundLock) {
                listeners = (listeners - 1).coerceAtLeast(0)
                if (listeners == 0) stopRecorder()
            }
        }

        private fun stopRecorder() {
            val rec = recorder ?: return
            recorder = null
            try { rec.stop() } catch (e: Throwable) {}
            rec.release()
            sound.clear()
        }

        fun stopAllSound() {
            synchronized(soundLock) {
                listeners = 0
                stopRecorder()
            }
        }

        /** Recorded sound (16-bit stereo, 48 kHz) waiting to be sent. */
        private val sound = ConcurrentLinkedQueue<ByteArray>()

        /** Everything recorded since the last call, or null if nothing. */
        fun readSound(): ByteArray? {
            val out = ByteArrayOutputStream()
            while (true) {
                val b = sound.poll() ?: break
                out.write(b)
            }
            return if (out.size() > 0) out.toByteArray() else null
        }

        /** The newest frame, or null if none yet. */
        fun frame(): Map<String, Any>? {
            val jpeg = latest ?: return null
            return mapOf("jpeg" to jpeg, "w" to latestW, "h" to latestH, "seq" to seq)
        }
    }

    private var projection: MediaProjection? = null
    private var display: VirtualDisplay? = null
    private var reader: ImageReader? = null
    private var worker: HandlerThread? = null
    private var lastFrameAt = 0L
    /** Called right after the user allows sound (Nexus is on screen then, which Android requires). */
    fun enableSound() {
        if (micMode || !soundPermitted(this)) return
        try {
            startAsForeground()
        } catch (e: Exception) {
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == "stop") {
            stopSelf()
            return START_NOT_STICKY
        }
        if (projection != null) return START_NOT_STICKY
        // Android requires the service to be in the foreground before the projection starts.
        startAsForeground()
        val code = intent?.getIntExtra("code", 0) ?: 0
        val data: Intent? = if (Build.VERSION.SDK_INT >= 33) {
            intent?.getParcelableExtra("data", Intent::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent?.getParcelableExtra("data")
        }
        if (data == null) {
            stopSelf()
            return START_NOT_STICKY
        }
        val manager = getSystemService(MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        val mp = manager.getMediaProjection(code, data)
        if (mp == null) {
            stopSelf()
            return START_NOT_STICKY
        }
        projection = mp
        activeProjection = mp
        instance = this
        val thread = HandlerThread("nexus-capture").also { it.start() }
        worker = thread
        val handler = Handler(thread.looper)
        mp.registerCallback(object : MediaProjection.Callback() {
            override fun onStop() {
                stopSelf()
            }
        }, handler)

        val metrics = DisplayMetrics()
        val dm = getSystemService(DISPLAY_SERVICE) as DisplayManager
        @Suppress("DEPRECATION")
        dm.getDisplay(Display.DEFAULT_DISPLAY).getRealMetrics(metrics)
        val scale = if (metrics.widthPixels > maxWidth) maxWidth.toFloat() / metrics.widthPixels else 1f
        val w = ((metrics.widthPixels * scale).toInt() / 2) * 2
        val h = ((metrics.heightPixels * scale).toInt() / 2) * 2

        val r = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 2)
        reader = r
        r.setOnImageAvailableListener({ ir ->
            val img = ir.acquireLatestImage() ?: return@setOnImageAvailableListener
            try {
                val now = SystemClock.uptimeMillis()
                if (now - lastFrameAt >= 90) { // at most ~11 frames per second
                    lastFrameAt = now
                    val plane = img.planes[0]
                    val pixelStride = plane.pixelStride
                    val rowPadding = plane.rowStride - pixelStride * w
                    val full = Bitmap.createBitmap(w + rowPadding / pixelStride, h, Bitmap.Config.ARGB_8888)
                    full.copyPixelsFromBuffer(plane.buffer)
                    val picture = if (rowPadding != 0) Bitmap.createBitmap(full, 0, 0, w, h) else full
                    val out = ByteArrayOutputStream()
                    picture.compress(Bitmap.CompressFormat.JPEG, 78, out)
                    if (picture !== full) picture.recycle()
                    full.recycle()
                    latest = out.toByteArray()
                    latestW = w
                    latestH = h
                    seq++
                }
            } catch (e: Exception) {
            } finally {
                img.close()
            }
        }, handler)
        display = mp.createVirtualDisplay(
            "nexus-screen", w, h, metrics.densityDpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, r.surface, null, handler,
        )
        running = true
        return START_NOT_STICKY
    }

    private fun startAsForeground() {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= 26) {
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL, "Screen sharing", NotificationManager.IMPORTANCE_LOW),
            )
        }
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, ScreenCaptureService::class.java).setAction("stop"), PendingIntent.FLAG_IMMUTABLE,
        )
        val builder = if (Build.VERSION.SDK_INT >= 26) {
            Notification.Builder(this, CHANNEL)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        @Suppress("DEPRECATION")
        val notification = builder
            .setContentTitle("Nexus is sharing this screen")
            .setContentText("Paired devices can see your screen. Tap Stop to end.")
            .setSmallIcon(android.R.drawable.ic_menu_view)
            .setContentIntent(open)
            .setOngoing(true)
            .addAction(android.R.drawable.ic_menu_close_clear_cancel, "Stop", stop)
            .build()
        if (Build.VERSION.SDK_INT >= 30 && soundPermitted(this)) {
            // Also a "microphone" service, so Android gives Nexus the real sound (not
            // silence) while another app is in front.
            try {
                startForeground(
                    1, notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE,
                )
                micMode = true
                return
            } catch (e: Exception) {
                // Fall back to screen only.
            }
        }
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION)
        } else {
            startForeground(1, notification)
        }
    }

    override fun onDestroy() {
        running = false
        latest = null
        // Only forget the recording if it is still ours (a new one may have started already).
        if (activeProjection === projection) {
            activeProjection = null
            micMode = false
            stopAllSound()
        }
        if (instance === this) instance = null
        try { display?.release() } catch (e: Exception) {}
        try { reader?.close() } catch (e: Exception) {}
        try { projection?.stop() } catch (e: Exception) {}
        worker?.quitSafely()
        display = null
        reader = null
        projection = null
        super.onDestroy()
    }
}
