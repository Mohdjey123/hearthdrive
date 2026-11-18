package app.nexuscloud.desktop

import android.Manifest
import android.content.ContentResolver
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.OpenableColumns
import android.provider.Settings
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import java.io.File

/**
 * The app's only screen. Besides hosting the web view it:
 *
 * - asks for permission to read photos and videos, for backup, through a
 *   small `window.NexusAndroid` bridge the page can call;
 * - receives files shared to NEXUS from other apps (the share menu) and
 *   copies them into `<cache>/shared`, where the node sends them from once
 *   the person picks a computer and folder;
 * - hands an update the node received and verified to Android's installer.
 */
class MainActivity : TauriActivity() {
  private var webView: WebView? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    // The app draws its own dark background under the system bars, so the
    // status and navigation bar icons are always light.
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
      navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT)
    )
    super.onCreate(savedInstanceState)
    // A recreated activity (after rotation, say) already took its share.
    if (savedInstanceState == null) receiveShare(intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    receiveShare(intent)
  }

  override fun onWebViewCreate(webView: WebView) {
    this.webView = webView
    webView.addJavascriptInterface(Bridge(), "NexusAndroid")
  }

  /** Tells the page something changed, as a DOM event on `window`. */
  private fun notifyPage(event: String) {
    runOnUiThread { webView?.evaluateJavascript("window.dispatchEvent(new Event('$event'))", null) }
  }

  // -- Photos and videos ---------------------------------------------------

  private fun mediaPermissions(): Array<String> =
    if (Build.VERSION.SDK_INT >= 33) {
      arrayOf(Manifest.permission.READ_MEDIA_IMAGES, Manifest.permission.READ_MEDIA_VIDEO)
    } else {
      arrayOf(Manifest.permission.READ_EXTERNAL_STORAGE)
    }

  private fun hasMediaAccess(): Boolean =
    mediaPermissions().all { ContextCompat.checkSelfPermission(this, it) == PackageManager.PERMISSION_GRANTED }

  inner class Bridge {
    @JavascriptInterface
    fun hasMediaAccess(): Boolean = this@MainActivity.hasMediaAccess()

    /** Shows Android's permission prompt; the page hears `nexus-media-access` after. */
    @JavascriptInterface
    fun requestMediaAccess() {
      runOnUiThread {
        this@MainActivity.requestPermissions(mediaPermissions()) { notifyPage("nexus-media-access") }
      }
    }

    /**
     * Opens Android's installer for the update package at [path], which the
     * node verified (signature, size and hash) before keeping it. Only the
     * node's own update package is accepted. Returns `started`,
     * `permission` (NEXUS must first be allowed to install apps; the
     * setting is opened) or `missing`.
     */
    @JavascriptInterface
    fun installUpdate(path: String): String = this@MainActivity.installUpdate(path)

    /** Opens this app's page in Android settings (when the prompt was turned off). */
    @JavascriptInterface
    fun openAppSettings() {
      runOnUiThread {
        startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", packageName, null)))
      }
    }
  }

  // -- Updates -------------------------------------------------------------

  private fun installUpdate(path: String): String {
    val file = try { File(path).canonicalFile } catch (e: Exception) { return "missing" }
    val root = File(applicationInfo.dataDir).canonicalFile
    val expected = "updates" + File.separator + "android-aarch64" + File.separator + "package.apk"
    if (!file.path.startsWith(root.path + File.separator) || !file.path.endsWith(File.separator + expected) || !file.isFile) {
      return "missing"
    }
    if (Build.VERSION.SDK_INT >= 26 && !packageManager.canRequestPackageInstalls()) {
      runOnUiThread {
        startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$packageName")))
      }
      return "permission"
    }
    // The installer reads it through the app's file provider, which shares
    // only the cache folder.
    val dir = File(cacheDir, "update").apply { mkdirs() }
    val copy = File(dir, "NEXUS-Cloud.apk")
    file.copyTo(copy, overwrite = true)
    val uri = FileProvider.getUriForFile(this, "$packageName.fileprovider", copy)
    val intent = Intent(Intent.ACTION_VIEW)
      .setDataAndType(uri, "application/vnd.android.package-archive")
      .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
    runOnUiThread { startActivity(intent) }
    return "started"
  }

  // -- Share menu ----------------------------------------------------------

  private fun receiveShare(intent: Intent?) {
    if (intent == null) return
    val action = intent.action
    if (action != Intent.ACTION_SEND && action != Intent.ACTION_SEND_MULTIPLE) return
    val uris = mutableListOf<Uri>()
    if (action == Intent.ACTION_SEND) {
      streamExtra(intent)?.let { uris.add(it) }
    } else {
      streamListExtra(intent)?.let { uris.addAll(it) }
    }
    val text = if (uris.isEmpty()) intent.getStringExtra(Intent.EXTRA_TEXT) else null
    val subject = intent.getStringExtra(Intent.EXTRA_SUBJECT)
    // Handle each share once.
    intent.action = null
    val inbox = File(cacheDir, "shared").apply { mkdirs() }
    Thread {
      for (uri in uris) {
        try {
          copyIn(uri, inbox)
        } catch (e: Exception) {
          Logger.warn("Could not receive a shared file: $e")
        }
      }
      if (!text.isNullOrBlank()) {
        try {
          val name = uniqueName(inbox, safeName((subject?.take(60) ?: "Shared text") + ".txt"))
          File(inbox, name).writeText(text)
        } catch (e: Exception) {
          Logger.warn("Could not receive shared text: $e")
        }
      }
      notifyPage("nexus-shared")
    }.start()
  }

  @Suppress("DEPRECATION")
  private fun streamExtra(intent: Intent): Uri? =
    if (Build.VERSION.SDK_INT >= 33) intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
    else intent.getParcelableExtra(Intent.EXTRA_STREAM)

  @Suppress("DEPRECATION")
  private fun streamListExtra(intent: Intent): List<Uri>? =
    if (Build.VERSION.SDK_INT >= 33) intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
    else intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM)

  /**
   * Copies one shared item into the inbox. Only content from other apps is
   * accepted: never `file://` paths or this app's own files, which another
   * app could otherwise trick NEXUS into sending.
   */
  private fun copyIn(uri: Uri, inbox: File) {
    if (uri.scheme != ContentResolver.SCHEME_CONTENT) return
    if (uri.authority == "$packageName.fileprovider") return
    var name: String? = null
    contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
      if (c.moveToFirst()) name = c.getString(0)
    }
    val finalName = uniqueName(inbox, safeName(name ?: uri.lastPathSegment ?: "Shared file"))
    val partial = File(inbox, "$finalName.part")
    contentResolver.openInputStream(uri)?.use { input ->
      partial.outputStream().use { output -> input.copyTo(output, 256 * 1024) }
    } ?: return
    partial.renameTo(File(inbox, finalName))
  }

  /** A plain file name: no folders, control characters or leading dots. */
  private fun safeName(name: String): String {
    val cleaned = name
      .map { c -> if (c.isISOControl() || c in "/\\:*?\"<>|") '_' else c }
      .joinToString("")
      .trim()
      .trimStart('.')
      .take(180)
    return cleaned.ifEmpty { "Shared file" }
  }

  /** `name.ext`, or `name (2).ext` and so on if that is taken. */
  private fun uniqueName(dir: File, name: String): String {
    val dot = name.lastIndexOf('.')
    val (stem, ext) = if (dot > 0) name.substring(0, dot) to name.substring(dot) else name to ""
    var candidate = name
    var n = 2
    while (File(dir, candidate).exists() || File(dir, "$candidate.part").exists()) {
      candidate = "$stem ($n)$ext"
      n++
    }
    return candidate
  }
}
