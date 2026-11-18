package app.nexus.nexus_mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.Bitmap
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
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
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION)
        } else {
            startForeground(1, notification)
        }
    }

    override fun onDestroy() {
        running = false
        latest = null
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
