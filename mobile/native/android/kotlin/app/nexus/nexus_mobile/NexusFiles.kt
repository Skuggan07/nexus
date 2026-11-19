package app.nexus.nexus_mobile

import android.content.ClipData
import android.content.ClipboardManager
import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import java.io.File

/**
 * Copying files and pictures between this phone and the PCs.
 *
 * Outgoing: files picked with "Share → Nexus", or pictures on the clipboard, are copied
 * into Nexus's cache so the Dart side can read and send them.
 *
 * Incoming: files copied on a PC are saved where people look for them (pictures in
 * Pictures/Nexus, videos in Movies/Nexus, everything else in Download/Nexus) and put
 * on the phone's clipboard so they can be pasted into apps that accept them.
 */
object NexusFiles {
    /** Files shared to Nexus that the Dart side hasn't picked up yet. */
    val pending = mutableListOf<Map<String, Any>>()

    /** Text shared to Nexus (a link, say) that the Dart side hasn't picked up yet. */
    var pendingText: String? = null

    private fun displayName(ctx: Context, uri: Uri): String {
        try {
            ctx.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
                if (c.moveToFirst()) {
                    val i = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (i >= 0) {
                        val n = c.getString(i)
                        if (!n.isNullOrBlank()) return n
                    }
                }
            }
        } catch (e: Exception) {
        }
        uri.lastPathSegment?.substringAfterLast('/')?.let { if (it.isNotBlank() && it.contains('.')) return it }
        val ext = ctx.contentResolver.getType(uri)?.let { MimeTypeMap.getSingleton().getExtensionFromMimeType(it) }
        return if (ext != null) "file.$ext" else "file"
    }

    /** A name without folder separators or other characters Windows refuses. */
    private fun safeName(name: String): String {
        val cleaned = name.replace(Regex("[\\\\/:*?\"<>|\\x00-\\x1f]"), "_").trim().trimEnd('.')
        return cleaned.ifEmpty { "file" }
    }

    private fun unique(name: String, used: MutableSet<String>): String {
        var n = name
        var i = 2
        val dot = name.lastIndexOf('.')
        val (base, ext) = if (dot > 0) name.substring(0, dot) to name.substring(dot) else name to ""
        while (!used.add(n.lowercase())) {
            n = "$base ($i)$ext"
            i++
        }
        return n
    }

    /** Copies shared or copied files into the cache. Returns [{path, name, size}]. */
    fun copyIn(ctx: Context, uris: List<Uri>): List<Map<String, Any>> {
        val root = File(ctx.cacheDir, "outgoing")
        // Leftovers from earlier sends (anything older than a day).
        root.listFiles()?.forEach {
            if (it.lastModified() < System.currentTimeMillis() - 24 * 3600_000L) it.deleteRecursively()
        }
        val dir = File(root, System.currentTimeMillis().toString())
        dir.mkdirs()
        val used = mutableSetOf<String>()
        val out = mutableListOf<Map<String, Any>>()
        for (uri in uris) {
            try {
                val name = unique(safeName(displayName(ctx, uri)), used)
                val f = File(dir, name)
                val input = ctx.contentResolver.openInputStream(uri) ?: continue
                input.use { i -> f.outputStream().use { o -> i.copyTo(o) } }
                out.add(mapOf("path" to f.absolutePath, "name" to name, "size" to f.length()))
            } catch (e: Exception) {
            }
        }
        return out
    }

    /** Pictures or files on the clipboard (only readable while Nexus is in front). */
    fun clipboardUris(ctx: Context): List<Uri> {
        val cm = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val clip = try {
            cm.primaryClip
        } catch (e: Exception) {
            null
        }
        if (clip == null) return emptyList()
        val out = mutableListOf<Uri>()
        for (i in 0 until clip.itemCount) {
            clip.getItemAt(i).uri?.let { if (it.scheme == "content") out.add(it) }
        }
        return out
    }

    private fun mimeOf(name: String): String {
        val ext = name.substringAfterLast('.', "").lowercase()
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext) ?: "application/octet-stream"
    }

    /**
     * Saves files that arrived from a PC (in [stage], at the paths in [rels]).
     * Returns {pictures, files, where} for the message the user sees.
     */
    fun saveIncoming(ctx: Context, stage: File, rels: List<String>): Map<String, Any> {
        val uris = ArrayList<Uri>()
        var pictures = 0
        var files = 0
        var failed = 0
        for (rel in rels) {
            val src = File(stage, rel)
            if (!src.isFile) continue
            val name = src.name
            val sub = rel.substringBeforeLast('/', "")
            val mime = mimeOf(name)
            // Loose pictures and videos go to the Gallery; anything inside a copied folder
            // keeps its folder, in Downloads.
            val image = sub.isEmpty() && mime.startsWith("image/")
            val video = sub.isEmpty() && mime.startsWith("video/")
            try {
                if (Build.VERSION.SDK_INT >= 29) {
                    val volume = MediaStore.VOLUME_EXTERNAL_PRIMARY
                    val (collection, base) = when {
                        image -> MediaStore.Images.Media.getContentUri(volume) to "${Environment.DIRECTORY_PICTURES}/Nexus"
                        video -> MediaStore.Video.Media.getContentUri(volume) to "${Environment.DIRECTORY_MOVIES}/Nexus"
                        else -> MediaStore.Downloads.getContentUri(volume) to "${Environment.DIRECTORY_DOWNLOADS}/Nexus"
                    }
                    val values = ContentValues().apply {
                        put(MediaStore.MediaColumns.DISPLAY_NAME, name)
                        put(MediaStore.MediaColumns.MIME_TYPE, mime)
                        put(MediaStore.MediaColumns.RELATIVE_PATH, if (sub.isEmpty()) base else "$base/$sub")
                        put(MediaStore.MediaColumns.IS_PENDING, 1)
                    }
                    val resolver = ctx.contentResolver
                    val uri = resolver.insert(collection, values) ?: throw Exception("not saved")
                    try {
                        val out = resolver.openOutputStream(uri) ?: throw Exception("not saved")
                        out.use { o -> src.inputStream().use { it.copyTo(o) } }
                        val done = ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }
                        resolver.update(uri, done, null, null)
                    } catch (e: Exception) {
                        resolver.delete(uri, null, null)
                        throw e
                    }
                    uris.add(uri)
                } else {
                    // Android 9 and older: Nexus's own Downloads folder (no permission needed).
                    val base = ctx.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: throw Exception("no storage")
                    val dir = File(base, if (sub.isEmpty()) "Nexus" else "Nexus/$sub")
                    dir.mkdirs()
                    val taken = mutableSetOf<String>()
                    dir.list()?.forEach { taken.add(it.lowercase()) }
                    src.copyTo(File(dir, unique(name, taken)))
                }
                if (image || video) pictures++ else files++
            } catch (e: Exception) {
                failed++
            }
            src.delete()
        }
        stage.deleteRecursively()
        if (uris.isNotEmpty()) {
            try {
                val cm = ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                val clip = ClipData.newUri(ctx.contentResolver, "Nexus", uris[0])
                for (u in uris.drop(1)) clip.addItem(ClipData.Item(u))
                cm.setPrimaryClip(clip)
            } catch (e: Exception) {
            }
        }
        return mapOf("pictures" to pictures, "files" to files, "failed" to failed)
    }
}
