package app.nexus.nexus_mobile

import android.Manifest
import android.app.Activity
import android.app.PendingIntent
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.widget.Toast
import java.io.File
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/** Bridges the Dart side to screen sharing (MediaProjection) and remote control (Accessibility). */
class MainActivity : FlutterActivity() {
    companion object {
        private const val CAPTURE_REQUEST = 4711
        private const val INSTALL_STATUS = "app.nexus.INSTALL_STATUS"
    }

    private var pending: MethodChannel.Result? = null

    // Sound from a PC: 16-bit stereo PCM played through an AudioTrack. Writes happen
    // one at a time on their own thread; if too many are waiting, extra ones are
    // dropped so the sound stays close to live.
    private val soundThread = java.util.concurrent.Executors.newSingleThreadExecutor()
    private val soundWaiting = java.util.concurrent.atomic.AtomicInteger(0)
    private var soundTrack: AudioTrack? = null

    private fun soundStopNow() {
        try {
            soundTrack?.stop()
        } catch (e: Exception) {
        }
        soundTrack?.release()
        soundTrack = null
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "nexus/screen").setMethodCallHandler { call, result ->
            when (call.method) {
                "startCapture" -> {
                    if (ScreenCaptureService.running) {
                        result.success(true)
                    } else {
                        pending?.success(false)
                        pending = result
                        ScreenCaptureService.maxWidth = call.argument<Int>("maxw") ?: 1280
                        val manager = getSystemService(MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                        @Suppress("DEPRECATION")
                        startActivityForResult(manager.createScreenCaptureIntent(), CAPTURE_REQUEST)
                    }
                }
                "stopCapture" -> {
                    stopService(Intent(this, ScreenCaptureService::class.java))
                    result.success(true)
                }
                "isCapturing" -> result.success(ScreenCaptureService.running)
                "frame" -> result.success(ScreenCaptureService.frame())
                "accessibilityEnabled" -> result.success(NexusAccessibilityService.instance != null)
                "openAccessibilitySettings" -> {
                    startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                    result.success(true)
                }
                "inject" -> {
                    val service = NexusAccessibilityService.instance
                    val args = call.arguments as? Map<*, *>
                    if (service != null && args != null) {
                        service.handle(args)
                        result.success(true)
                    } else {
                        result.success(false)
                    }
                }
                // When the clipboard last changed, without reading what's on it (reading
                // makes Android show a "pasted from your clipboard" notice).
                "clipStamp" -> {
                    val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                    val d = try { cm.primaryClipDescription } catch (e: Exception) { null }
                    result.success(
                        when {
                            d == null -> null
                            Build.VERSION.SDK_INT >= 26 -> d.timestamp
                            else -> System.identityHashCode(d).toLong()
                        }
                    )
                }
                // Sharing this phone's sound (see ScreenCaptureService.startSound).
                "soundCaptureStart" ->
                    result.success(ScreenCaptureService.instance?.startSound() ?: "Screen sharing is off on the phone")
                "soundCaptureRead" -> result.success(ScreenCaptureService.readSound())
                "soundCaptureStop" -> {
                    ScreenCaptureService.instance?.stopSound()
                    result.success(null)
                }
                "soundAllowed" -> result.success(
                    Build.VERSION.SDK_INT >= 29 &&
                        checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
                )
                "allowSound" -> {
                    if (Build.VERSION.SDK_INT >= 29) {
                        requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO), 7301)
                    }
                    result.success(null)
                }
                "audioStart" -> {
                    val rate = call.argument<Int>("rate") ?: 48000
                    soundThread.execute {
                        soundStopNow()
                        val min = AudioTrack.getMinBufferSize(
                            rate, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_16BIT
                        )
                        val track = AudioTrack.Builder()
                            .setAudioAttributes(
                                AudioAttributes.Builder()
                                    .setUsage(AudioAttributes.USAGE_MEDIA)
                                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                                    .build()
                            )
                            .setAudioFormat(
                                AudioFormat.Builder()
                                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                                    .setSampleRate(rate)
                                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                                    .build()
                            )
                            .setBufferSizeInBytes(maxOf(min * 2, rate))
                            .setTransferMode(AudioTrack.MODE_STREAM)
                            .build()
                        track.play()
                        soundTrack = track
                    }
                    result.success(true)
                }
                "audioWrite" -> {
                    val bytes = call.arguments as? ByteArray
                    if (bytes != null && soundWaiting.get() < 12) {
                        soundWaiting.incrementAndGet()
                        soundThread.execute {
                            try {
                                soundTrack?.write(bytes, 0, bytes.size)
                            } finally {
                                soundWaiting.decrementAndGet()
                            }
                        }
                    }
                    result.success(null)
                }
                "audioStop" -> {
                    soundThread.execute { soundStopNow() }
                    result.success(null)
                }
                "installApk" -> {
                    val path = call.argument<String>("path")
                    if (path == null) {
                        result.success("error")
                    } else {
                        result.success(installApk(File(path)))
                    }
                }
                else -> result.notImplemented()
            }
        }
    }

    /**
     * Installs a downloaded update with Android's package installer. Android shows
     * its own "Update this app?" confirmation. The first time, the user must also allow
     * Nexus to install apps ("Install unknown apps"), so we open that setting instead.
     */
    private fun installApk(apk: File): String {
        if (!apk.exists()) return "error"
        if (Build.VERSION.SDK_INT >= 26 && !packageManager.canRequestPackageInstalls()) {
            startActivity(
                Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$packageName"))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
            return "permission"
        }
        return try {
            val installer = packageManager.packageInstaller
            val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
            val sessionId = installer.createSession(params)
            installer.openSession(sessionId).use { session ->
                apk.inputStream().use { input ->
                    session.openWrite("nexus.apk", 0, apk.length()).use { out ->
                        input.copyTo(out)
                        session.fsync(out)
                    }
                }
                val flags = if (Build.VERSION.SDK_INT >= 31) {
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE
                } else {
                    PendingIntent.FLAG_UPDATE_CURRENT
                }
                val status = Intent(this, MainActivity::class.java)
                    .setAction(INSTALL_STATUS)
                    .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
                val pending = PendingIntent.getActivity(this, sessionId, status, flags)
                session.commit(pending.intentSender)
            }
            "started"
        } catch (e: Exception) {
            Toast.makeText(this, "Update failed: ${e.message}", Toast.LENGTH_LONG).show()
            "error"
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        if (intent.action != INSTALL_STATUS) return
        when (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, -999)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                val confirm: Intent? = if (Build.VERSION.SDK_INT >= 33) {
                    intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                } else {
                    @Suppress("DEPRECATION")
                    intent.getParcelableExtra(Intent.EXTRA_INTENT)
                }
                if (confirm != null) startActivity(confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            }
            PackageInstaller.STATUS_SUCCESS -> {}
            PackageInstaller.STATUS_FAILURE_CONFLICT, PackageInstaller.STATUS_FAILURE_INCOMPATIBLE -> Toast.makeText(
                this,
                "This update was signed differently from the installed app. Uninstall Nexus once, then install the new version.",
                Toast.LENGTH_LONG,
            ).show()
            else -> {
                val msg = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: "unknown reason"
                Toast.makeText(this, "Update not installed: $msg", Toast.LENGTH_LONG).show()
            }
        }
    }

    @Suppress("DEPRECATION")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != CAPTURE_REQUEST) return
        val result = pending
        pending = null
        if (resultCode == Activity.RESULT_OK && data != null) {
            val start = Intent(this, ScreenCaptureService::class.java)
                .putExtra("code", resultCode)
                .putExtra("data", data)
            if (Build.VERSION.SDK_INT >= 26) startForegroundService(start) else startService(start)
            result?.success(true)
        } else {
            result?.success(false)
        }
    }
}
