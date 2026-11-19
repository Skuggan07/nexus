package app.nexus.nexus_mobile

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.widget.Toast

/**
 * Appears as "Nexus" in Android's Share menu. Copies the shared photos or files (or
 * text) for Nexus to send to the PCs, then opens Nexus. It has no screen of its own.
 */
class ShareActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val intent: Intent? = getIntent()
        if (intent == null) {
            finish()
            return
        }
        val uris = mutableListOf<Uri>()
        when (intent.action) {
            Intent.ACTION_SEND -> {
                val u: Uri? = if (Build.VERSION.SDK_INT >= 33) {
                    intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
                } else {
                    @Suppress("DEPRECATION")
                    intent.getParcelableExtra(Intent.EXTRA_STREAM)
                }
                if (u != null) uris.add(u)
            }
            Intent.ACTION_SEND_MULTIPLE -> {
                val list: List<Uri>? = if (Build.VERSION.SDK_INT >= 33) {
                    intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
                } else {
                    @Suppress("DEPRECATION")
                    intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM)
                }
                if (list != null) uris.addAll(list)
            }
        }
        val text = intent.getStringExtra(Intent.EXTRA_TEXT)
        if (uris.isEmpty()) {
            if (!text.isNullOrBlank()) {
                NexusFiles.pendingText = text
                openNexus()
            }
            finish()
            return
        }
        Toast.makeText(this, "Nexus: getting ${uris.size} file${if (uris.size == 1) "" else "s"} ready…", Toast.LENGTH_SHORT)
            .show()
        // Copy while this screen still holds permission to read the shared files.
        Thread {
            val files = NexusFiles.copyIn(applicationContext, uris)
            runOnUiThread {
                synchronized(NexusFiles.pending) { NexusFiles.pending.addAll(files) }
                if (files.isEmpty()) {
                    Toast.makeText(this, "Nexus could not read those files", Toast.LENGTH_LONG).show()
                } else {
                    openNexus()
                }
                finish()
            }
        }.start()
    }

    private fun openNexus() {
        startActivity(
            Intent(this, MainActivity::class.java)
                .setAction(MainActivity.SHARED)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        )
        MainActivity.notifyShared()
    }
}
