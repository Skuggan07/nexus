package app.nexus.nexus_mobile

import android.app.Activity
import android.content.Intent
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.provider.Settings
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/** Bridges the Dart side to screen sharing (MediaProjection) and remote control (Accessibility). */
class MainActivity : FlutterActivity() {
    companion object {
        private const val CAPTURE_REQUEST = 4711
    }

    private var pending: MethodChannel.Result? = null

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
                else -> result.notImplemented()
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
