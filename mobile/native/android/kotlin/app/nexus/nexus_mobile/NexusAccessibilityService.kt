package app.nexus.nexus_mobile

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.content.Intent
import android.graphics.Path
import android.graphics.PointF
import android.hardware.display.DisplayManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.DisplayMetrics
import android.view.Display
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import kotlin.math.hypot

/**
 * Carries out remote-control events from a paired device: taps, long presses,
 * swipes, Back/Home/Recents and typing into the selected text box. The user turns
 * this on in Android's Accessibility settings. It reads screen content only to find
 * the selected text box when typing.
 *
 * Events use the same shape as on Windows (positions are fractions 0..1):
 *   button {b, down, x, y} … move {x, y} … button {down:false} → one gesture
 *   click {b: left|right, x, y} → tap / long press
 *   scroll {dy} → swipe up or down
 *   key {k: escape → Back, win → Home, tab + alt → Recents, enter, backspace}
 *   text {s} → typed into the selected text box
 */
class NexusAccessibilityService : AccessibilityService() {
    companion object {
        @Volatile var instance: NexusAccessibilityService? = null
    }

    private val main = Handler(Looper.getMainLooper())
    private val stroke = ArrayList<PointF>()
    private var strokeStart = 0L

    override fun onServiceConnected() {
        instance = this
    }

    override fun onUnbind(intent: Intent?): Boolean {
        instance = null
        return super.onUnbind(intent)
    }

    override fun onDestroy() {
        instance = null
        super.onDestroy()
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {}

    override fun onInterrupt() {}

    fun handle(m: Map<*, *>) {
        main.post {
            try {
                perform(m)
            } catch (e: Exception) {
            }
        }
    }

    private fun screen(): Pair<Float, Float> {
        val metrics = DisplayMetrics()
        val dm = getSystemService(DISPLAY_SERVICE) as DisplayManager
        @Suppress("DEPRECATION")
        dm.getDisplay(Display.DEFAULT_DISPLAY).getRealMetrics(metrics)
        return Pair(metrics.widthPixels.toFloat(), metrics.heightPixels.toFloat())
    }

    private fun point(m: Map<*, *>): PointF? {
        val x = (m["x"] as? Number)?.toFloat() ?: return null
        val y = (m["y"] as? Number)?.toFloat() ?: return null
        val (w, h) = screen()
        return PointF(x.coerceIn(0f, 1f) * (w - 1), y.coerceIn(0f, 1f) * (h - 1))
    }

    private fun perform(m: Map<*, *>) {
        when (m["op"] as? String) {
            "move" -> if (stroke.isNotEmpty()) point(m)?.let { stroke.add(it) }
            "button" -> {
                val down = m["down"] as? Boolean ?: true
                if (down) {
                    stroke.clear()
                    point(m)?.let { stroke.add(it) }
                    strokeStart = SystemClock.uptimeMillis()
                } else {
                    point(m)?.let { stroke.add(it) }
                    finishStroke(m["b"] as? String)
                }
            }
            "click" -> {
                val p = point(m) ?: return
                gesture(listOf(p), if (m["b"] == "right") 650L else 60L)
            }
            "scroll" -> {
                val dy = (m["dy"] as? Number)?.toFloat() ?: 0f
                if (dy != 0f) {
                    val (w, h) = screen()
                    val half = h * 0.15f
                    // Wheel up shows what is above, which on a phone is a downward swipe.
                    val from = PointF(w / 2, if (dy > 0) h / 2 - half else h / 2 + half)
                    val to = PointF(w / 2, if (dy > 0) h / 2 + half else h / 2 - half)
                    gesture(listOf(from, to), 250L)
                }
            }
            "key" -> {
                val mods = (m["mods"] as? List<*>)?.map { it.toString() } ?: emptyList()
                when ((m["k"] as? String ?: "").lowercase()) {
                    "escape", "esc", "back" -> performGlobalAction(GLOBAL_ACTION_BACK)
                    "win", "home" -> performGlobalAction(GLOBAL_ACTION_HOME)
                    "tab" -> if (mods.contains("alt")) performGlobalAction(GLOBAL_ACTION_RECENTS)
                    "backspace" -> editText { it.dropLast(1) }
                    "enter" -> enter()
                }
            }
            "text" -> {
                val s = m["s"] as? String ?: ""
                if (s == "\n") enter() else if (s.isNotEmpty()) editText { it + s }
            }
        }
    }

    private fun finishStroke(button: String?) {
        if (stroke.isEmpty()) return
        val points = ArrayList(stroke)
        stroke.clear()
        val held = (SystemClock.uptimeMillis() - strokeStart).coerceIn(40L, 5000L)
        val first = points[0]
        val moved = points.any { hypot((it.x - first.x).toDouble(), (it.y - first.y).toDouble()) > 24.0 }
        if (!moved) {
            gesture(listOf(first), if (button == "right" || held > 550) 650L else 60L)
        } else {
            gesture(points, held)
        }
    }

    private fun gesture(points: List<PointF>, durationMs: Long) {
        if (Build.VERSION.SDK_INT < 24 || points.isEmpty()) return
        val path = Path()
        path.moveTo(points[0].x, points[0].y)
        for (p in points.drop(1)) path.lineTo(p.x, p.y)
        val description = GestureDescription.Builder()
            .addStroke(GestureDescription.StrokeDescription(path, 0, durationMs.coerceAtLeast(1L)))
            .build()
        dispatchGesture(description, null, null)
    }

    private fun focused(): AccessibilityNodeInfo? =
        rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)

    private fun editText(change: (String) -> String) {
        val node = focused() ?: return
        if (!node.isEditable) return
        val showingHint = Build.VERSION.SDK_INT >= 26 && node.isShowingHintText
        val current = if (showingHint) "" else (node.text?.toString() ?: "")
        val args = Bundle()
        args.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, change(current))
        node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
    }

    private fun enter() {
        val node = focused() ?: return
        if (Build.VERSION.SDK_INT >= 30) {
            node.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.id)
        }
    }
}
